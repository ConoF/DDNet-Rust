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
#[cfg(feature = "gpu")]
use crate::gpu_renderer::{GpuRenderer, TeeFrame, OVERLAY_KEY};
#[cfg(feature = "gpu")]
use crate::gpu_hud::GpuHudScene;
use crate::datafile::Datafile;
use crate::window_assets::{WindowAssets, candidate_roots};
use crate::game_audio::{attack_sound_for_weapon, try_start, GameAudio};
use crate::sound_assets::GameSound;
use crate::server_browser::ClientSettings;
use crate::ingame_menu::{MenuAction, MenuState, ServerStatus};
use crate::netobjects::CHARACTER_TYPE;
use crate::interpolation::{
    camera_follow_fraction, interpolate_snapshots, online_interpolation_fraction,
};
use crate::envelope::EnvelopeTimes;
use crate::frame_stats::FrameTiming;
use crate::input::{
    correct_next_input_tick, FireCounter, InputFrame, PlayerInput,
    PLAYERFLAG_CHATTING, PLAYERFLAG_PLAYING, PLAYERFLAG_IN_MENU,
};
use crate::gameplay::WeaponControls;
use crate::online_ui::{OnlineHud, TextInputCollector};
use crate::netobjects::{player_name, CharacterCoreNet};
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
    target: Vec2, weapons: WeaponControls, chatting: bool, in_menu: bool,
) -> PlayerInput {
    if in_menu {
        return PlayerInput {
            fire, player_flags: PLAYERFLAG_IN_MENU,
            ..PlayerInput::default()
        };
    }
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
    // The menu may change the frame limiter while a connection is live.
    let settings_path = ClientSettings::default_path();
    let mut connection = ClientSettings::load(&settings_path)
        .unwrap_or_default();
    window.set_target_fps(usize::from(connection.fps_limit));
    let chars = Rc::new(RefCell::new(Vec::new()));
    window.set_input_callback(Box::new(TextInputCollector(Rc::clone(&chars))));
    let mut pixels = Framebuffer::new(WIDTH as u32, HEIGHT as u32)
        .map_err(|e| format!("framebuffer: {e:?}"))?;
    #[cfg(feature = "gpu")]
    let mut gpu = if std::env::var("DDNET_RENDERER").as_deref() == Ok("software") {
        None
    } else {
        match GpuRenderer::new(&window, WIDTH as u32, HEIGHT as u32) {
            Ok(mut renderer) => {
                renderer.sync_assets(None, None, None);
                Some(renderer)
            }
            Err(error) => {
                eprintln!("GPU backend unavailable ({error}); using software renderer");
                None
            }
        }
    };
    #[cfg(feature = "gpu")]
    let mut gpu_tee = Framebuffer::new(WIDTH as u32, HEIGHT as u32)
        .map_err(|e| format!("GPU tee framebuffer: {e:?}"))?;
    #[cfg(feature = "gpu")]
    let mut gpu_hud = GpuHudScene::new(WIDTH, HEIGHT);
    let mut camera = Camera {
        center: Vec2::default(), scale: f32::from(connection.zoom_percent) / 100.0,
    };
    let mut assets = WindowAssets::default();
    let mut audio = try_start(&candidate_roots(None));
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
    let mut map_loaded_at = Instant::now();
    let mut last_title = Instant::now();
    let mut reconnect_attempts = 0u8;
    let mut frames_in_sample = 0u32;
    let mut frame_times = FrameTiming::default();
    let mut fire_counter = FireCounter::default();
    let mut input_time_left_ms = None::<i32>;
    let mut predictor = LocalPredictor::new();
    let mut hud = OnlineHud::new();
    let mut weapons = WeaponControls::default();
    let mut menu = MenuState::new();
    let mut left_mouse_was_down = false;
    let mut exit_requested = false;

    while window.is_open() && !exit_requested {
        let now = Instant::now();
        let frame_duration = (now - last_frame).min(Duration::from_millis(100));
        accumulator += frame_duration;
        last_frame = now;

        // Minifb receives actual Unicode characters through its callback;
        // physical key codes are used only for gameplay actions and editing.
        // Discard the same-frame T/Y opener character when entering chat.
        let was_typing = hud.is_typing();
        let typed = std::mem::take(&mut *chars.borrow_mut());
        if was_typing { hud.append_typed(&typed); }
        if menu.is_editing() { menu.append_typed(&typed); }
        let mut menu_actions = Vec::new();
        let keys = window.get_keys_pressed(if was_typing || menu.is_editing() {
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
            } else if menu.is_open() {
                if let Some(action) = menu.key(key, &mut connection) {
                    menu_actions.push(action);
                }
            } else {
                match key {
                    Key::Escape => menu.toggle(),
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
        // Mouse clicks use the same tested hitboxes as the menu renderer.
        // A held fire button must not activate multiple menu entries.
        let left_mouse_down = window.get_mouse_down(MouseButton::Left);
        if menu.is_open() && left_mouse_down && !left_mouse_was_down {
            if let Some(pos) = window.get_mouse_pos(MouseMode::Discard) {
                if let Some(action) = menu.click(pos, (WIDTH, HEIGHT), &mut connection) {
                    menu_actions.push(action);
                }
            }
        }
        left_mouse_was_down = left_mouse_down;

        for action in menu_actions {
            match action {
                MenuAction::Resume => {}
                MenuAction::Disconnect => {
                    exit_requested = true;
                }
                MenuAction::Spectate | MenuAction::Join | MenuAction::Kill => {
                    if session.stage != Stage::EnterGameSent {
                        menu.feedback("Not in game yet");
                        continue;
                    }
                    let result = match action {
                        MenuAction::Spectate => session.set_team(-1),
                        MenuAction::Join => session.set_team(0),
                        _ => session.request_kill(),
                    };
                    match result {
                        Ok(packet) => {
                            socket.send(&packet).map_err(|e| format!("menu action UDP: {e}"))?;
                            menu.close();
                        }
                        Err(error) => menu.feedback(format!("Server rejected action: {error:?}")),
                    }
                }
                MenuAction::SettingsChanged => {
                    window.set_target_fps(usize::from(connection.fps_limit));
                    camera.scale = f32::from(connection.zoom_percent) / 100.0;
                    if let Err(error) = connection.save(&settings_path) {
                        menu.feedback(format!("Settings save failed: {error:?}"));
                    }
                }
                MenuAction::ChangeNickname(name) => {
                    // On a live server the change is a vital Cl_ChangeInfo;
                    // during handshake the new name is used for StartInfo.
                    let change = if session.stage == Stage::EnterGameSent {
                        session.change_name(&name).map(Some)
                    } else {
                        session.start_info.name = name.clone();
                        Ok(None)
                    };
                    match change {
                        Ok(packet) => {
                            if let Some(packet) = packet {
                                socket.send(&packet)
                                    .map_err(|e| format!("rename UDP: {e}"))?;
                            }
                            connection.nickname = name;
                            match connection.save(&settings_path) {
                                Ok(()) => menu.feedback("Nickname saved"),
                                Err(error) => menu.feedback(
                                    format!("Cannot save nickname: {error:?}")),
                            }
                        }
                        Err(error) => menu.feedback(
                            format!("Nickname update failed: {error:?}")),
                    }
                }
            }
        }
        if exit_requested { break; }

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
                        if let (Some(out), Some(old_tick)) =
                            (audio.as_mut().filter(|_| connection.sound_enabled), current_snapshot)
                        {
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
                        let channel = if team != 0 { "TEAM" } else { "ALL" };
                        let name = u16::try_from(client_id).ok()
                            .and_then(|id| current_snapshot
                                .and_then(|tick| session.snapshot_state(tick))
                                .and_then(|state| player_name(state, id)))
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| if client_id < 0 {
                                String::from("SERVER")
                            } else {
                                format!("#{client_id}")
                            });
                        hud.chat_line(&format!("[{channel} {name}] "), &text);
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
                        map_loaded_at = Instant::now();
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
                        #[cfg(feature = "gpu")]
                        if let Some(renderer) = gpu.as_mut() {
                            renderer.sync_assets(assets.visual_map.as_ref(),
                                assets.default_skin.as_ref(), assets.game_sprites.as_ref());
                        }
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
            #[cfg(feature = "gpu")]
            if let Some(renderer) = gpu.as_mut() {
                renderer.sync_assets(None, None, None);
            }
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
            if !hud.is_typing() && !menu.is_open() && wheel.abs() >= 1.0 {
                weapons.scroll(wheel.round() as i32);
            }
        }
        if !hud.is_typing() && !menu.is_open() {
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
        let blocked = typing || menu.is_open();
        let control = movement_input(
            i32::from(window.is_key_down(Key::D) || window.is_key_down(Key::Right)) -
            i32::from(window.is_key_down(Key::A) || window.is_key_down(Key::Left)),
            window.is_key_down(Key::Space) || window.is_key_down(Key::W)
                || window.is_key_down(Key::Up),
            window.get_mouse_down(MouseButton::Right) || window.is_key_down(Key::E),
            fire_counter.sample(!blocked && window.get_mouse_down(MouseButton::Left)),
            Vec2::new(
                (mouse.0 - WIDTH as f32 / 2.0) / camera.scale,
                (mouse.1 - HEIGHT as f32 / 2.0) / camera.scale,
            ),
            weapons,
            typing, menu.is_open(),
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
                    let progress = online_interpolation_fraction(
                        previous_snapshot.unwrap_or_default(),
                        current_snapshot.unwrap_or_default(),
                        last_snapshot_at.elapsed(),
                    );
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
                let follow = camera_follow_fraction(frame_duration);
                camera.center.x += (pos.x - camera.center.x) * follow;
                camera.center.y += (pos.y - camera.center.y) * follow;
            }
        }
        let render_started = Instant::now();
        let game_tick = current_snapshot.unwrap_or(0).max(0) as f64
            + (last_snapshot_at.elapsed().as_secs_f64() * 50.0).min(3.0);
        let animation_times = EnvelopeTimes {
            game_ms: game_tick * 20.0,
            local_ms: map_loaded_at.elapsed().as_secs_f64() * 1000.0,
        };
        #[cfg(feature = "gpu")]
        let using_gpu = gpu.is_some();
        #[cfg(not(feature = "gpu"))]
        let using_gpu = false;
        if !using_gpu {
            if let Some(map) = session.loaded_map.as_ref() {
                assets.draw_background_at(&mut pixels, map, camera, animation_times)?;
            } else {
                pixels.clear(crate::graphics::BACKGROUND);
            }
        }
        #[cfg(feature = "gpu")]
        let native_tees = using_gpu && assets.visual_map.is_some()
            && gpu.as_ref().is_some_and(|renderer| renderer.native_tees_ready());
        #[cfg(not(feature = "gpu"))]
        let native_tees = false;
        #[cfg(feature = "gpu")]
        if using_gpu && !native_tees {
            gpu_tee.clear(OVERLAY_KEY);
            if assets.visual_map.is_none() {
                // Maps without supported visual art still show collision
                // geometry while keeping the native GPU surface active.
                if let Some(map) = session.loaded_map.as_ref() {
                    gpu_tee.render_map(map, camera)
                        .map_err(|e| format!("GPU map fallback: {e:?}"))?;
                }
            }
        }
        #[cfg(feature = "gpu")]
        let tee_frame = if using_gpu { &mut gpu_tee } else { &mut pixels };
        #[cfg(not(feature = "gpu"))]
        let tee_frame = &mut pixels;
        if !native_tees {
            if let Some(snapshot) = display.as_ref() {
                assets.draw_snapshot(tee_frame, snapshot, camera,
                    current_snapshot.unwrap_or(0).max(0) as u64)?;
            }
        }
        if !using_gpu {
            assets.draw_foreground_at(&mut pixels, camera, animation_times)?;
        }
        let local_weapon = display.as_ref().and_then(|snapshot|
            local_character_id(snapshot).and_then(|id|
                snapshot.find(CHARACTER_TYPE, id)
            )
        ).and_then(|obj| obj.words.get(19).copied());
        let scoreboard = if window.is_key_down(Key::Tab)
            && !hud.is_typing() && !menu.is_open() {
            current_snapshot.and_then(|tick| session.snapshot_state(tick))
        } else {
            None
        };
        #[cfg(feature = "gpu")]
        if using_gpu {
            gpu_hud.clear();
            if connection.show_nameplates && !menu.is_open() {
                if let Some(snapshot) = display.as_ref() {
                    hud.draw_nameplates(&mut gpu_hud, snapshot, camera);
                }
            }
            hud.render_with_visibility(&mut gpu_hud, &format!("{:?}", session.stage),
                local_weapon, current_snapshot, scoreboard, connection.show_chat);
            menu.paint(&mut gpu_hud, &connection, display.as_ref(),
                ServerStatus {
                    endpoint, stage: &format!("{:?}", session.stage),
                    map: session.map_name(), tick: current_snapshot,
                    ack: session.snapshot_ack_tick(),
                });
        }
        if !using_gpu {
            if connection.show_nameplates && !menu.is_open() {
                if let Some(snapshot) = display.as_ref() {
                    hud.draw_nameplates(&mut pixels, snapshot, camera);
                }
            }
            hud.render_with_visibility(&mut pixels, &format!("{:?}", session.stage),
                local_weapon, current_snapshot, scoreboard, connection.show_chat);
            menu.paint(&mut pixels, &connection, display.as_ref(),
                ServerStatus {
                    endpoint, stage: &format!("{:?}", session.stage),
                    map: session.map_name(), tick: current_snapshot,
                    ack: session.snapshot_ack_tick(),
                });
        }
        #[cfg(feature = "gpu")]
        if let Some(renderer) = gpu.as_mut() {
            renderer.render(assets.visual_map.as_ref(), camera, animation_times,
                TeeFrame {
                    state: display.as_ref(),
                    tick: current_snapshot.unwrap_or(0).max(0) as u64,
                    skin: assets.default_skin.as_ref(),
                    game: assets.game_sprites.as_ref(),
                }, &gpu_tee, &gpu_hud)?;
            window.update();
        } else {
            window.update_with_buffer(&pixels.pixels, WIDTH, HEIGHT)
                .map_err(|e| format!("software present: {e}"))?;
        }
        #[cfg(not(feature = "gpu"))]
        window.update_with_buffer(&pixels.pixels, WIDTH, HEIGHT)
            .map_err(|e| format!("software present: {e}"))?;
        frames_in_sample += 1;
        frame_times.record(render_started.elapsed());
        let sample_seconds = last_title.elapsed().as_secs_f32();
        if sample_seconds >= 1.0 {
            let displayed_fps = frames_in_sample as f32 / sample_seconds;
            if let Some(metrics) = frame_times.take_metrics() {
                window.set_title(&format!(
                    "DDNet-Rust | {peer} | FPS {:.0} | draw p50 {:.1} p95 {:.1} p99 {:.1} ms | tick {:?} | ACK {} | recovery {} | input {:?}ms",
                    displayed_fps, metrics.p50_ms, metrics.p95_ms, metrics.p99_ms,
                    current_snapshot, session.snapshot_ack_tick(),
                    session.is_snapshot_recovering(), input_time_left_ms,
                ));
            }
            frames_in_sample = 0;
            last_title = Instant::now();
        }
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
            Vec2::new(30000.0, -20000.0), weapons, false, false);
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
            Vec2::new(100.0, 80.0), weapons, true, false);
        assert_eq!(chat.direction, 0);
        assert_eq!(chat.jump, 0);
        assert_eq!(chat.hook, 0);
        assert_eq!(chat.fire, 4);
        assert_eq!(chat.player_flags, PLAYERFLAG_CHATTING);
        assert_eq!(chat.wanted_weapon, 0);
        let paused = movement_input(1, true, true, 4,
            Vec2::new(100.0, 80.0), weapons, false, true);
        assert_eq!(paused.player_flags, PLAYERFLAG_IN_MENU);
        assert_eq!(paused.direction, 0);
        assert_eq!(paused.fire, 4);
        assert_eq!(paused.hook, 0);
    }
}
