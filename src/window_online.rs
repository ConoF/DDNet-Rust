//! Experimental windowed DDNet 0.6 session viewer.
//!
//! This uses the native Rust transport, map downloader and snapshot decoder,
//! not an embedded DDNet C++ client. It is NOT a full DDNet player client:
//! missing full renderer, authoritative prediction parity, editor, custom
//! skins and some 0.6/0.7 protocol extensions.

use std::cell::RefCell;
use std::io::ErrorKind;
use std::rc::Rc;
use std::net::{ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};

use crate::graphics::{Camera, Framebuffer};
use crate::datafile::Datafile;
use crate::window_assets::{WindowAssets, candidate_roots};
use crate::game_audio::{attack_sound_for_weapon, try_start, GameAudio};
use crate::sound_assets::GameSound;
use crate::server_browser::ClientSettings;
use crate::netobjects::CHARACTER_TYPE;
use crate::interpolation::interpolate_snapshots;
use crate::input::{
    correct_next_input_tick, FireCounter, InputFrame, PlayerInput,
    PLAYERFLAG_CHATTING, PLAYERFLAG_PLAYING,
};
use crate::gameplay::WeaponControls;
use crate::online_ui::{OnlineHud, TextInputCollector};
use crate::netobjects::CharacterCoreNet;
use crate::physics::Vec2;
use crate::prediction::{apply_visual_prediction, local_character_id, LocalPredictor};
use crate::session::{GameAction, GameSession06, Stage};
use crate::snapshot_delta::SnapshotState;
use crate::transport::State;

const WIDTH: usize = 960;
const HEIGHT: usize = 540;
const TICK: Duration = Duration::from_millis(20);

fn preferred_character(snapshot: &SnapshotState) -> Option<Vec2> {
    let local = local_character_id(snapshot)
        .and_then(|id| snapshot.find(CHARACTER_TYPE, id))
        .and_then(CharacterCoreNet::from_object);
    let first = snapshot.items.iter().find_map(CharacterCoreNet::from_object);
    local.or(first).map(|c| Vec2::new(c.x as f32, c.y as f32))
}

fn movement_input(
    direction: i32, jump: bool, hook: bool, fire: i32,
    target: Vec2, weapons: WeaponControls, chatting: bool,
) -> PlayerInput {
    if chatting {
        return PlayerInput {
            fire, player_flags: PLAYERFLAG_CHATTING,
            ..PlayerInput::default()
        };
    }
    PlayerInput {
        direction: direction.clamp(-1, 1),
        target_x: target.x.round().clamp(-10000.0, 10000.0) as i32,
        target_y: target.y.round().clamp(-10000.0, 10000.0) as i32,
        jump: i32::from(jump),
        fire,
        player_flags: PLAYERFLAG_PLAYING,
        hook: i32::from(hook),
        wanted_weapon: weapons.wanted_weapon,
        next_weapon: weapons.next_weapon,
        prev_weapon: weapons.prev_weapon,
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
    let chars = Rc::new(RefCell::new(Vec::new()));
    window.set_input_callback(Box::new(TextInputCollector(Rc::clone(&chars))));
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
    let mut current_snapshot: Option<i32> = None;
    let mut previous_snapshot: Option<i32> = None;
    let mut last_snapshot_at = Instant::now();
    let mut last_title = Instant::now();
    let mut reconnect_attempts = 0u8;
    let mut fire_counter = FireCounter::default();
    let mut input_time_left_ms = None::<i32>;
    let mut predictor = LocalPredictor::new();
    let mut hud = OnlineHud::new();
    let mut weapons = WeaponControls::default();
    let mut exit_requested = false;

    while window.is_open() && !exit_requested {
        let now = Instant::now();
        accumulator += (now - last_frame).min(Duration::from_millis(100));
        last_frame = now;

        // Minifb receives actual Unicode characters through its callback;
        // physical key codes are used only for gameplay actions and editing.
        // Discard the same-frame T/Y opener character when entering chat.
        let was_typing = hud.is_typing();
        let typed = std::mem::take(&mut *chars.borrow_mut());
        if was_typing { hud.append_typed(&typed); }
        let keys = window.get_keys_pressed(if was_typing {
            KeyRepeat::Yes
        } else {
            KeyRepeat::No
        });
        for key in keys {
            if hud.is_typing() {
                match key {
                    Key::Escape => hud.cancel_chat(),
                    Key::Backspace => hud.backspace(),
                    Key::Enter => {
                        if let Some((team, text)) = hud.submit_chat() {
                            match session.send_chat(team, &text) {
                                Ok(wire) => {
                                    socket.send(&wire).map_err(|e|
                                        format!("send chat: {e}")
                                    )?;
                                }
                                Err(error) => hud.chat_line(
                                    "[client] ", &format!("Chat failed: {error:?}"),
                                ),
                            }
                        }
                    }
                    _ => {}
                }
            } else {
                match key {
                    Key::Escape => exit_requested = true,
                    Key::T => hud.open_chat(false),
                    Key::Y => hud.open_chat(true),
                    Key::Key1 => { weapons.select(1); }
                    Key::Key2 => { weapons.select(2); }
                    Key::Key3 => { weapons.select(3); }
                    Key::Key4 => { weapons.select(4); }
                    Key::Key5 => { weapons.select(5); }
                    Key::F7 | Key::F8 | Key::F10 if session.stage == Stage::EnterGameSent => {
                        let wire = match key {
                            Key::F7 => session.set_team(-1),
                            Key::F8 => session.set_team(0),
                            _ => session.request_kill(),
                        }.map_err(|e| format!("game action: {e:?}"))?;
                        socket.send(&wire).map_err(|e| format!("game action send: {e}"))?;
                    }
                    _ => {}
                }
            }
        }
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
                        previous_snapshot = current_snapshot;
                        current_snapshot = Some(tick);
                        last_snapshot_at = Instant::now();
                        intended_tick = intended_tick.max(tick.saturating_add(1));
                    }
                    GameAction::SnapshotRecoveryRequested { tick, reason } => {
                        eprintln!(
                            "Snapshot {tick} rejected ({reason:?}); requesting full state"
                        );
                    }
                    GameAction::InputTiming { time_left_ms, .. } => {
                        // Bounded, one-time correction from feedback for a
                        // *locally sent* tick; never rewind our input stream.
                        intended_tick = correct_next_input_tick(
                            intended_tick, session.snapshot_ack_tick(), time_left_ms,
                        );
                        input_time_left_ms = Some(time_left_ms);
                    }
                    GameAction::ChatReceived { team, client_id, text } => {
                        let label = if team != 0 { "team" } else { "all" };
                        hud.chat_line(&format!("[{label} #{client_id}] "), &text);
                    }
                    GameAction::ServerBroadcast(text) => {
                        hud.announcement(&text);
                    }
                    GameAction::ServerMotd(text) => {
                        hud.chat_line("[MOTD] ", &text);
                    }
                    GameAction::Disconnected(reason) => {
                        return Err(format!("Server disconnected: {reason}"));
                    }
                    GameAction::MapLoaded { width, height, .. } => {
                        predictor.reset();
                        previous_snapshot = None;
                        current_snapshot = None;
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
            intended_tick = 1;
            current_snapshot = None;
            previous_snapshot = None;
            assets = WindowAssets::default();
            // Reset event deduplication with the snapshot chain. Audio device
            // stays active and reusable across reconnects.
            accumulator = Duration::ZERO;
            fire_counter = FireCounter::default();
            input_time_left_ms = None;
            predictor.reset();
            hud = OnlineHud::new();
            weapons = WeaponControls::default();
            chars.borrow_mut().clear();
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
            if !hud.is_typing() && wheel.abs() >= 1.0 {
                weapons.scroll(wheel.round() as i32);
            }
        }
        if !hud.is_typing() {
            if window.is_key_down(Key::Equal) {
                camera.scale = (camera.scale * 1.02).min(8.0);
            }
            if window.is_key_down(Key::Minus) {
                camera.scale = (camera.scale / 1.02).max(0.1);
            }
        }
        let mouse = window.get_mouse_pos(MouseMode::Discard)
            .unwrap_or((WIDTH as f32 * 0.75, HEIGHT as f32 * 0.5));
        let typing = hud.is_typing();
        let control = movement_input(
            i32::from(window.is_key_down(Key::D) || window.is_key_down(Key::Right)) -
            i32::from(window.is_key_down(Key::A) || window.is_key_down(Key::Left)),
            window.is_key_down(Key::Space) || window.is_key_down(Key::W)
                || window.is_key_down(Key::Up),
            window.get_mouse_down(MouseButton::Right) || window.is_key_down(Key::E),
            fire_counter.sample(!typing && window.get_mouse_down(MouseButton::Left)),
            Vec2::new(
                (mouse.0 - WIDTH as f32 / 2.0) / camera.scale,
                (mouse.1 - HEIGHT as f32 / 2.0) / camera.scale,
            ),
            weapons,
            typing,
        );
        for _ in 0..5 {
            if accumulator < TICK { break; }
            accumulator -= TICK;
            if session.stage == Stage::EnterGameSent {
                let frame = InputFrame {
                    last_ack_snapshot: session.snapshot_ack_tick(),
                    intended_tick,
                    input: control,
                };
                let bytes = session.send_input(frame)
                    .map_err(|e| format!("input encoding: {e:?}"))?;
                socket.send(&bytes).map_err(|e| format!("send input: {e}"))?;
                predictor.record_sent_input(frame);
                intended_tick = intended_tick.saturating_add(1).min(0x6fff_ffff);
            }
        }

        // Prediction only changes a cloned display snapshot. CRC-verified
        // history and the NETMSG_INPUT ACK remain server-authoritative.
        let mut display = current_snapshot
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
        if let (Some(tick), Some(map), Some(display_state)) = (
            current_snapshot, session.loaded_map.as_ref(), display.as_mut(),
        ) {
            if !session.is_snapshot_recovering() {
                if let Some(authoritative) = session.snapshot_state(tick) {
                    if let Some(prediction) = predictor.predict(tick, authoritative, map) {
                        apply_visual_prediction(display_state, prediction);
                    }
                }
            }
        }
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
        let local_weapon = display.as_ref().and_then(|snapshot|
            local_character_id(snapshot).and_then(|id|
                snapshot.find(CHARACTER_TYPE, id)
            )
        ).and_then(|obj| obj.words.get(19).copied());
        hud.render(&mut pixels, &format!("{:?}", session.stage),
            local_weapon, current_snapshot);
        if last_title.elapsed() > Duration::from_secs(1) {
            window.set_title(&format!(
                "DDNet-Rust 0.6 prototype | {peer} | {:?} | tick {:?} | ACK {} | recovery {} | input {:?}ms",
                session.stage, current_snapshot, session.snapshot_ack_tick(),
                session.is_snapshot_recovering(), input_time_left_ms,
            ));
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
    fn invalid_local_info_does_not_override_first_character_fallback() {
        let mut state = SnapshotState::default();
        state.items.push(SnapshotObject {
            type_id: 9, id: 5,
            words: { let mut w = vec![0; 22]; w[1] = 123; w[2] = 456; w },
        });
        state.items.push(SnapshotObject {
            type_id: 10, id: 5, words: vec![1, 5, 0, 0, 0],
        });
        assert_eq!(local_character_id(&state), Some(5));
        assert_eq!(preferred_character(&state), Some(Vec2::new(123.0, 456.0)));
    }

    #[test]
    fn outgoing_player_input_stays_inside_valid_ranges() {
        let mut weapons = WeaponControls::default();
        weapons.select(3);
        let input = movement_input(-10, true, true, 3,
            Vec2::new(30000.0, -20000.0), weapons, false);
        assert_eq!(input.direction, -1);
        assert_eq!(input.target_x, 10000);
        assert_eq!(input.target_y, -10000);
        assert_eq!(input.jump, 1);
        assert_eq!(input.hook, 1);
        assert_eq!(input.fire, 3);
        assert_eq!(input.player_flags, PLAYERFLAG_PLAYING);
        assert_eq!(input.wanted_weapon, 3);
        assert!(input.validate().is_ok());
        let chat = movement_input(1, true, true, 4,
            Vec2::new(100.0, 80.0), weapons, true);
        assert_eq!(chat.direction, 0);
        assert_eq!(chat.jump, 0);
        assert_eq!(chat.hook, 0);
        assert_eq!(chat.fire, 4);
        assert_eq!(chat.player_flags, PLAYERFLAG_CHATTING);
        assert_eq!(chat.wanted_weapon, 0);
    }
}
