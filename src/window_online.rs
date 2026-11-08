//! Experimental windowed DDNet 0.6 session viewer.
//!
//! This uses the native Rust transport, map downloader and snapshot decoder,
//! not an embedded DDNet C++ client. It is NOT a full DDNet player client:
//! missing full renderer/prediction/editor, custom skins, sound
//! and some 0.6/0.7 protocol extensions.

use std::io::ErrorKind;
use std::net::{ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use minifb::{Key, MouseButton, MouseMode, Window, WindowOptions};

use crate::graphics::{Camera, Framebuffer};
use crate::datafile::Datafile;
use crate::window_assets::{WindowAssets, candidate_roots};
use crate::game_audio::{attack_sound_for_weapon, try_start, GameAudio};
use crate::sound_assets::GameSound;
use crate::server_browser::ClientSettings;
use crate::netobjects::CHARACTER_TYPE;
use crate::interpolation::interpolate_snapshots;
use crate::input::{InputFrame, PlayerInput};
use crate::netobjects::{CharacterCoreNet, PlayerInfoNet};
use crate::physics::Vec2;
use crate::session::{GameAction, GameSession06, Stage};
use crate::snapshot_delta::SnapshotState;
use crate::transport::State;

const WIDTH: usize = 960;
const HEIGHT: usize = 540;
const TICK: Duration = Duration::from_millis(20);

fn preferred_character(snapshot: &SnapshotState) -> Option<Vec2> {
    let local_id = snapshot.items.iter().filter_map(PlayerInfoNet::from_object)
        .find(|player| player.local).map(|player| player.client_id);
    let local = local_id.and_then(|id| snapshot.items.iter()
        .find(|obj| obj.id as i32 == id)
        .and_then(CharacterCoreNet::from_object));
    let first = snapshot.items.iter().find_map(CharacterCoreNet::from_object);
    local.or(first).map(|c| Vec2::new(c.x as f32, c.y as f32))
}

fn movement_input(direction: i32, jump: bool, hook: bool, target: Vec2) -> PlayerInput {
    PlayerInput {
        direction: direction.clamp(-1, 1),
        target_x: target.x.round().clamp(-10000.0, 10000.0) as i32,
        target_y: target.y.round().clamp(-10000.0, 10000.0) as i32,
        jump: i32::from(jump),
        hook: i32::from(hook),
        wanted_weapon: 1,
        ..PlayerInput::default()
    }
}

/// Derive game effects only from consecutive authoritative snapshots,
/// never from interpolated frames, so >60 FPS render does not replay sound.
/// Character weapon (word 19), attack tick (word 21), and hook (word 9)
/// are DDNet 0.6 netobject fields.
fn play_snapshot_sounds(audio: &mut GameAudio, previous: &SnapshotState,
                        current: &SnapshotState) {
    for item in &current.items {
        if item.type_id != CHARACTER_TYPE || item.words.len() < 22 { continue; }
        let Some(prev) = previous.items.iter()
            .find(|other| other.type_id == CHARACTER_TYPE
                && other.id == item.id && other.words.len() >= 22) else {
            continue;
        };
        let location = Some(Vec2::new(item.words[1] as f32, item.words[2] as f32));
        if item.words[21] > 0 && item.words[21] != prev.words[21] {
            if let Some(sound) = attack_sound_for_weapon(item.words[19]) {
                audio.play(sound, location);
            }
        }
        if item.words[9] == 5 && prev.words[9] != 5 {
            audio.play(GameSound::HookAttach, location);
        }
        if item.words[7] & 1 != 0 && prev.words[7] & 1 == 0 {
            audio.play(GameSound::PlayerJump, location);
        }
    }
}

/// Opens a real window and connects to a *remote* DDNet 0.6 server.
/// Inputs go over UDP at 50 Hz. Visuals use map tiles and default skin
/// when original DDNet data assets are available, with a collision fallback.
pub fn play_online(endpoint: &str) -> Result<(), String> {
    let address = if endpoint.contains(':') { endpoint.to_owned() }
        else { format!("{endpoint}:8303") };
    let peer = address.to_socket_addrs()
        .map_err(|e| format!("DNS: {e}"))?
        .next().ok_or("No addresses resolved")?;
    let socket = UdpSocket::bind(if peer.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })
        .map_err(|e| format!("UDP bind: {e}"))?;
    socket.connect(peer).map_err(|e| format!("UDP connect: {e}"))?;
    socket.set_nonblocking(true).map_err(|e| format!("nonblocking: {e}"))?;

    let mut window = Window::new(
        "DDNet-Rust — online 0.6 protocol prototype",
        WIDTH, HEIGHT, WindowOptions::default(),
    ).map_err(|e| format!("window: {e}"))?;
    window.set_target_fps(60);
    let mut pixels = Framebuffer::new(WIDTH as u32, HEIGHT as u32)
        .map_err(|e| format!("framebuffer: {e:?}"))?;
    let mut camera = Camera { center: Vec2::default(), scale: 1.5 };
    let mut assets = WindowAssets::default();
    let mut audio = try_start(&candidate_roots(None));
    let connection = ClientSettings::load(&ClientSettings::default_path())
        .unwrap_or_default();
    let reconnect_limit = connection.reconnect_attempts;
    let receive_timeout = Duration::from_secs(connection.connect_timeout_secs);
    let mut session = GameSession06::new(String::new(), 64 * 1024 * 1024, 16_777_216);
    session.start_info.name = connection.nickname;
    let mut hello = session.connect().map_err(|e| format!("handshake: {e:?}"))?;
    socket.send(&hello).map_err(|e| format!("CONNECT: {e}"))?;

    let mut packet = [0u8; 1400];
    let mut last_hello = Instant::now();
    let mut last_keepalive = Instant::now();
    let mut last_receive = Instant::now();
    let mut last_frame = Instant::now();
    let mut accumulator = Duration::ZERO;
    let mut intended_tick: i32 = 1;
    let mut last_ack_snapshot: i32 = -1;
    let mut current_snapshot: Option<i32> = None;
    let mut previous_snapshot: Option<i32> = None;
    let mut last_snapshot_at = Instant::now();
    let mut last_title = Instant::now();
    let mut reconnect_attempts = 0u8;

    while window.is_open() && !window.is_key_down(Key::Escape) {
        let now = Instant::now();
        accumulator += (now - last_frame).min(Duration::from_millis(100));
        last_frame = now;
        // Bound the datagrams processed in a single render frame.
        for _ in 0..256 {
            let count = match socket.recv(&mut packet) {
                Ok(count) => count,
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(format!("UDP receive: {e}")),
            };
            let events = match session.receive(&packet[..count]) {
                Ok(events) => events,
                Err(error) => {
                    eprintln!("Unsupported or malformed server packet: {error:?}");
                    continue;
                }
            };
            last_receive = Instant::now();
            for event in events {
                match event {
                    GameAction::Outgoing(wire) => {
                        socket.send(&wire).map_err(|e| format!("UDP send: {e}"))?;
                    }
                    GameAction::SnapshotDecoded { tick, .. } => {
                        if let (Some(out), Some(old_tick)) = (audio.as_mut(), current_snapshot) {
                            if let (Some(previous), Some(current)) =
                                (session.snapshot_state(old_tick), session.snapshot_state(tick))
                            {
                                play_snapshot_sounds(out, previous, current);
                            }
                        }
                        reconnect_attempts = 0;
                        last_ack_snapshot = tick;
                        previous_snapshot = current_snapshot;
                        current_snapshot = Some(tick);
                        last_snapshot_at = Instant::now();
                        intended_tick = intended_tick.max(tick.saturating_add(1));
                    }
                    GameAction::Disconnected(reason) => {
                        return Err(format!("Server disconnected: {reason}"));
                    }
                    GameAction::MapLoaded { width, height, .. } => {
                        camera.center = Vec2::new(width as f32 * 16.0, height as f32 * 16.0);
                        // The transport keeps CRC-checked original map bytes,
                        // not only the collision layer. Decode assets once.
                        assets = session.loaded_map_bytes.as_deref()
                            .and_then(|bytes| Datafile::parse(bytes).ok())
                            .map(|file| WindowAssets::from_datafile(&file, None))
                            .unwrap_or_default();
                    }
                    _ => {}
                }
            }
        }

        let mut failed = None;
        if session.transport.state == State::Online {
            match session.transport.poll_retransmission(Instant::now()) {
                Ok(Some(retry)) => {
                    socket.send(&retry).map_err(|e| format!("resend vital chunk: {e}"))?;
                }
                Ok(None) => {}
                Err(e) => failed = Some(format!("reliable transport stopped: {e:?}")),
            }
        }
        if failed.is_none() && last_receive.elapsed() > receive_timeout {
            failed = Some("No valid server packets for 20 seconds".into());
        }
        if let Some(reason) = failed {
            if reconnect_attempts >= reconnect_limit {
                return Err(format!("{reason}; maximum {reconnect_limit} reconnects exhausted"));
            }
            reconnect_attempts += 1;
            eprintln!("Connection lost ({reason}); reconnect attempt {reconnect_attempts}/{reconnect_limit}");
            hello = session.restart_connection()
                .map_err(|e| format!("reconnect state: {e:?}"))?;
            socket.send(&hello).map_err(|e| format!("reconnect UDP: {e}"))?;
            last_hello = Instant::now();
            last_keepalive = last_hello;
            last_receive = last_hello;
            last_snapshot_at = last_hello;
            last_ack_snapshot = -1;
            intended_tick = 1;
            current_snapshot = None;
            previous_snapshot = None;
            assets = WindowAssets::default();
            // Reset event deduplication with the snapshot chain. Audio device
            // stays active and reusable across reconnects.
            accumulator = Duration::ZERO;
            continue;
        }
        if session.stage == Stage::Connecting &&
            last_hello.elapsed() >= Duration::from_millis(500)
        {
            socket.send(&hello).map_err(|e| format!("resend CONNECT: {e}"))?;
            last_hello = Instant::now();
        }
        if session.transport.state == State::Online &&
            last_keepalive.elapsed() >= Duration::from_secs(1)
        {
            if let Ok(keepalive) = session.transport.keepalive() {
                socket.send(&keepalive).map_err(|e| format!("keepalive: {e}"))?;
            }
            last_keepalive = Instant::now();
        }

        if let Some((_, wheel)) = window.get_scroll_wheel() {
            if wheel > 0.0 { camera.scale = (camera.scale * 1.15).min(8.0); }
            if wheel < 0.0 { camera.scale = (camera.scale / 1.15).max(0.1); }
        }
        let mouse = window.get_mouse_pos(MouseMode::Discard)
            .unwrap_or((WIDTH as f32 * 0.75, HEIGHT as f32 * 0.5));
        let control = movement_input(
            i32::from(window.is_key_down(Key::D) || window.is_key_down(Key::Right)) -
            i32::from(window.is_key_down(Key::A) || window.is_key_down(Key::Left)),
            window.is_key_down(Key::Space) || window.is_key_down(Key::W)
                || window.is_key_down(Key::Up),
            window.get_mouse_down(MouseButton::Right) || window.is_key_down(Key::E),
            Vec2::new(
                (mouse.0 - WIDTH as f32 / 2.0) / camera.scale,
                (mouse.1 - HEIGHT as f32 / 2.0) / camera.scale,
            ),
        );
        for _ in 0..5 {
            if accumulator < TICK { break; }
            accumulator -= TICK;
            if session.stage == Stage::EnterGameSent {
                let frame = InputFrame {
                    last_ack_snapshot,
                    intended_tick,
                    input: control,
                };
                let bytes = session.send_input(frame)
                    .map_err(|e| format!("input encoding: {e:?}"))?;
                socket.send(&bytes).map_err(|e| format!("send input: {e}"))?;
                intended_tick = intended_tick.saturating_add(1).min(0x6fff_ffff);
            }
        }

        // Interpolation is display-only; input ACKs and prediction always use
        // the unmodified decoded snapshots stored in GameSession06.
        let display = current_snapshot
            .and_then(|tick| session.snapshot_state(tick))
            .map(|newest| {
                if let Some(previous) = previous_snapshot
                    .and_then(|tick| session.snapshot_state(tick))
                {
                    let progress = last_snapshot_at.elapsed().as_secs_f32() / 0.02;
                    interpolate_snapshots(previous, newest, progress)
                } else {
                    newest.clone()
                }
            });
        if let Some(snapshot) = display.as_ref() {
            if let Some(pos) = preferred_character(snapshot) {
                if let Some(out) = audio.as_ref() { out.listener(pos); }
                camera.center.x += (pos.x - camera.center.x) * 0.3;
                camera.center.y += (pos.y - camera.center.y) * 0.3;
            }
        }
        if let Some(map) = session.loaded_map.as_ref() {
            assets.draw_map(&mut pixels, map, camera)?;
        } else {
            pixels.clear(crate::graphics::BACKGROUND);
        }
        if let Some(snapshot) = display.as_ref() {
            assets.draw_snapshot(&mut pixels, snapshot, camera,
                current_snapshot.unwrap_or(0).max(0) as u64)?;
        }
        if last_title.elapsed() > Duration::from_secs(1) {
            window.set_title(&format!("DDNet-Rust 0.6 prototype | {peer} | {:?} | tick {:?}",
                session.stage, current_snapshot));
            last_title = Instant::now();
        }
        window.update_with_buffer(&pixels.pixels, WIDTH, HEIGHT)
            .map_err(|e| format!("present: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot_delta::SnapshotObject;

    #[test]
    fn prefer_local_player_when_multiple_character_objects_exist() {
        let mut remote = vec![0; 22];
        remote[1] = 500;
        remote[2] = 600;
        let mut local = vec![0; 22];
        local[1] = 100;
        local[2] = 200;
        let state = SnapshotState { items: vec![
            SnapshotObject { type_id: 9, id: 1, words: remote },
            SnapshotObject { type_id: 9, id: 2, words: local },
            SnapshotObject { type_id: 10, id: 2, words: vec![1,2,0,0,0] },
        ] };
        assert_eq!(preferred_character(&state), Some(Vec2::new(100.0, 200.0)));
        assert_eq!(preferred_character(&SnapshotState::default()), None);
    }

    #[test]
    fn outgoing_player_input_stays_inside_valid_ranges() {
        let input = movement_input(-10, true, true, Vec2::new(30000.0, -20000.0));
        assert_eq!(input.direction, -1);
        assert_eq!(input.target_x, 10000);
        assert_eq!(input.target_y, -10000);
        assert_eq!(input.jump, 1);
        assert_eq!(input.hook, 1);
        assert_eq!(input.wanted_weapon, 1);
        assert!(input.validate().is_ok());
    }
}
