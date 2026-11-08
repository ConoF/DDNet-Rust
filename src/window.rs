//! Interactive, offline-only DDNet physics sandbox backed by the Rust CPU renderer.
//! Offline prototype: textured DDNet map and default skin where assets exist;
//! no server connection or full DDNet GUI.

use std::time::{Duration, Instant};
use std::cell::RefCell;
use std::rc::Rc;
use std::path::Path;

use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};

use crate::datafile::Datafile;
use crate::graphics::{Camera, Framebuffer};
#[cfg(feature = "gpu")]
use crate::gpu_renderer::{GpuRenderer, TeeFrame, OVERLAY_KEY};
#[cfg(feature = "gpu")]
use crate::gpu_hud::GpuHudScene;
#[cfg(feature = "gpu")]
use crate::online_ui::HudCanvas;
use crate::hook::HookState;
use crate::map::GameMap;
use crate::physics::Vec2;
use crate::interpolation::camera_follow_fraction;
use crate::envelope::EnvelopeTimes;
use crate::simulator::{SimInput, TeeSimulator};
use crate::snapshot_delta::{SnapshotObject, SnapshotState};
use crate::window_assets::{WindowAssets, candidate_roots};
use crate::server_browser::ClientSettings;
use crate::ingame_menu::{MenuAction, MenuState, ServerStatus};
use crate::online_ui::TextInputCollector;
use crate::game_audio::{attack_sound_for_weapon, try_start};
use crate::sound_assets::GameSound;

const SCREEN_W: usize = 960;
const SCREEN_H: usize = 540;
const PHYSICS_TICK: Duration = Duration::from_millis(20);

/// This is only a fallback for the offline sandbox, not vanilla spawn logic.
pub fn offline_spawn(map: &GameMap) -> Option<Vec2> {
    let size = Vec2::new(28.0, 28.0);
    for (i, tile) in map.tiles.iter().enumerate() {
        if (192..=194).contains(&tile.index) {
            let p = Vec2::new(
                (i % map.width) as f32 * 32.0 + 16.0,
                (i / map.width) as f32 * 32.0 + 16.0,
            );
            if !map.test_box(p, size) { return Some(p); }
        }
    }
    let mut nearest = None;
    for y in 1..map.height.saturating_sub(1) {
        for x in 1..map.width.saturating_sub(1) {
            if !map.tile(x, y+1).is_some_and(|t| matches!(t.index, 1 | 3)) {
                continue;
            }
            let p = Vec2::new(x as f32 * 32.0 + 16.0, y as f32 * 32.0 + 16.0);
            if map.test_box(p, size) { continue; }
            let score = x.abs_diff(map.width / 2) + y.abs_diff(map.height / 2);
            if nearest.as_ref().is_none_or(|(old, _)| score < *old) {
                nearest = Some((score, p));
            }
        }
    }
    nearest.map(|(_, p)| p)
}

/// Draw one map in a native desktop window; simulate movement at fixed 50 Hz.
/// A/D: run, W/Space: jump, right mouse/E: hook, wheel: zoom,
/// R: respawn, Escape: close.
pub fn play_offline(path: &str) -> Result<(), String> {
    play_offline_impl(path, None, false)
}

/// Opens the OS window inside a virtual display, advances scripted inputs and
/// emits a real PNG screenshot; intended for GitHub Actions/Xvfb smoke tests.
pub fn smoke_offline(path: &str, output: &str) -> Result<(), String> {
    play_offline_impl(path, Some(output), false)
}

#[cfg(feature = "gpu")]
pub fn smoke_gpu_offline(path: &str) -> Result<(), String> {
    play_offline_impl(path, None, true)
}

fn play_offline_impl(
    path: &str, smoke_output: Option<&str>, gpu_smoke: bool,
) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("map read: {e}"))?;
    let file = Datafile::parse(&bytes).map_err(|e| format!("datafile: {e:?}"))?;
    let map = GameMap::from_datafile(&file, 16_777_216)
        .map_err(|e| format!("game layer: {e:?}"))?;
    let assets = WindowAssets::from_datafile(&file, Some(Path::new(path)));
    if (smoke_output.is_some() || gpu_smoke)
        && (assets.visual_map.is_none() || assets.default_skin.is_none()
            || assets.game_sprites.is_none())
    {
        return Err("textured smoke requires DDNet mapres and skins/default.png".into());
    }
    let spawn = offline_spawn(&map).ok_or("no safe spawn available")?;
    let mut window = Window::new(
        "DDNet-Rust: OFFLINE physics prototype",
        SCREEN_W, SCREEN_H, WindowOptions::default(),
    ).map_err(|e| format!("window: {e}"))?;
    let settings_path = ClientSettings::default_path();
    let mut settings = if smoke_output.is_some() || gpu_smoke {
        ClientSettings::default()
    } else {
        ClientSettings::load(&settings_path).unwrap_or_default()
    };
    window.set_target_fps(usize::from(settings.fps_limit));
    let chars = Rc::new(RefCell::new(Vec::new()));
    window.set_input_callback(Box::new(TextInputCollector(Rc::clone(&chars))));
    let mut pixels = Framebuffer::new(SCREEN_W as u32, SCREEN_H as u32)
        .map_err(|e| format!("framebuffer: {e:?}"))?;
    #[cfg(feature = "gpu")]
    let mut gpu = if smoke_output.is_some()
        || (!gpu_smoke && std::env::var("DDNET_RENDERER").as_deref() == Ok("software"))
    {
        None
    } else {
        match GpuRenderer::new(&window, SCREEN_W as u32, SCREEN_H as u32) {
            Ok(mut renderer) => {
                renderer.sync_assets(assets.visual_map.as_ref(),
                    assets.default_skin.as_ref(), assets.game_sprites.as_ref());
                Some(renderer)
            }
            Err(error) if !gpu_smoke => {
                eprintln!("GPU unavailable in offline mode: {error}; falling back to CPU");
                None
            }
            Err(error) => return Err(format!("GPU smoke requires working adapter: {error}")),
        }
    };
    #[cfg(feature = "gpu")]
    let mut gpu_tee = Framebuffer::new(SCREEN_W as u32, SCREEN_H as u32)
        .map_err(|e| format!("GPU tee overlay: {e:?}"))?;
    #[cfg(feature = "gpu")]
    let mut gpu_hud = GpuHudScene::new(SCREEN_W, SCREEN_H);
    // Hardware-free Xvfb smoke remains deterministic. Real play opens the
    // default WASAPI/ALSA output and keeps decoding on the game thread.
    let mut audio = if smoke_output.is_none() && !gpu_smoke {
        try_start(&candidate_roots(Some(Path::new(path))))
    } else { None };
    let mut simulator = TeeSimulator::new(spawn);
    let mut camera = Camera {
        center: spawn, scale: f32::from(settings.zoom_percent) / 100.0,
    };
    let mut snapshot = SnapshotState {
        items: vec![SnapshotObject { type_id: 9, id: 0, words: vec![0; 22] }],
    };
    let mut selected_weapon = 1i32; // 0 hammer; 1 gun; 2 shotgun; 3 grenade; 4 laser; 5 ninja
    let mut attack_tick = 0i32;
    let mut last_attack_button = false;
    let mut previous = Instant::now();
    let mut accumulator = Duration::ZERO;
    let mut frames: usize = 0;
    let mut menu = MenuState::new();
    let mut left_was_down = false;

    while window.is_open() {
        let typed = std::mem::take(&mut *chars.borrow_mut());
        if menu.is_editing() { menu.append_typed(&typed); }
        let mut menu_actions = Vec::new();
        for key in window.get_keys_pressed(if menu.is_editing() {
            KeyRepeat::Yes
        } else { KeyRepeat::No }) {
            if menu.is_open() {
                if let Some(action) = menu.key(key, &mut settings) {
                    menu_actions.push(action);
                }
            } else if key == Key::Escape {
                menu.toggle();
            }
        }
        let left_down = window.get_mouse_down(MouseButton::Left);
        if menu.is_open() && left_down && !left_was_down {
            if let Some(pos) = window.get_mouse_pos(MouseMode::Discard) {
                if let Some(action) = menu.click(pos, (SCREEN_W, SCREEN_H), &mut settings) {
                    menu_actions.push(action);
                }
            }
        }
        left_was_down = left_down;
        for action in menu_actions {
            match action {
                MenuAction::Resume => {}
                MenuAction::Disconnect => return Ok(()),
                MenuAction::Kill => {
                    simulator = TeeSimulator::new(spawn);
                    attack_tick = 0;
                    accumulator = Duration::ZERO;
                    menu.close();
                }
                MenuAction::Join | MenuAction::Spectate =>
                    menu.feedback("Team selection requires an online server"),
                MenuAction::SettingsChanged => {
                    window.set_target_fps(usize::from(settings.fps_limit));
                    camera.scale = f32::from(settings.zoom_percent) / 100.0;
                    if let Err(error) = settings.save(&settings_path) {
                        menu.feedback(format!("Cannot save settings: {error:?}"));
                    }
                }
                MenuAction::ChangeNickname(nick) => {
                    settings.nickname = nick;
                    match settings.save(&settings_path) {
                        Ok(()) => menu.feedback("Nickname saved for next connection"),
                        Err(error) => menu.feedback(
                            format!("Cannot save nickname: {error:?}")),
                    }
                }
            }
        }
        let now = Instant::now();
        let frame_duration = (now - previous).min(Duration::from_millis(100));
        if menu.is_open() {
            accumulator = Duration::ZERO;
        } else {
            accumulator += frame_duration;
        }
        previous = now;
        if !menu.is_open() && window.is_key_down(Key::R) {
            simulator = TeeSimulator::new(spawn);
            accumulator = Duration::ZERO;
        }
        if let Some((_, wheel)) = window.get_scroll_wheel() {
            if !menu.is_open() {
                if wheel > 0.0 { camera.scale = (camera.scale * 1.12).min(8.0); }
                if wheel < 0.0 { camera.scale = (camera.scale / 1.12).max(0.1); }
            }
        }
        let mouse = window.get_mouse_pos(MouseMode::Discard)
            .unwrap_or((SCREEN_W as f32 * 0.75, SCREEN_H as f32 * 0.5));
        let direction = if menu.is_open() { 0 } else {
            i32::from(window.is_key_down(Key::D) || window.is_key_down(Key::Right))
                - i32::from(window.is_key_down(Key::A) || window.is_key_down(Key::Left))
        };
        let previous_weapon = selected_weapon;
        if !menu.is_open() && window.is_key_down(Key::Key1) { selected_weapon = 0; }
        if !menu.is_open() && window.is_key_down(Key::Key2) { selected_weapon = 1; }
        if !menu.is_open() && window.is_key_down(Key::Key3) { selected_weapon = 2; }
        if !menu.is_open() && window.is_key_down(Key::Key4) { selected_weapon = 3; }
        if !menu.is_open() && window.is_key_down(Key::Key5) { selected_weapon = 4; }
        if !menu.is_open() && window.is_key_down(Key::Key6) { selected_weapon = 5; }
        if selected_weapon != previous_weapon {
            if let Some(out) = audio.as_mut().filter(|_| settings.sound_enabled) {
                out.play(GameSound::WeaponSwitch, None);
            }
        }
        let attack_button = window.get_mouse_down(MouseButton::Left);
        if !menu.is_open() && attack_button && !last_attack_button {
            attack_tick = simulator.tick.min(i32::MAX as u64) as i32;
            if let Some(kind) = attack_sound_for_weapon(selected_weapon) {
                if let Some(out) = audio.as_mut().filter(|_| settings.sound_enabled) {
                    out.play(kind, Some(simulator.position));
                }
            }
        }
        last_attack_button = attack_button;
        let controls = SimInput {
            direction,
            jump: window.is_key_down(Key::W) || window.is_key_down(Key::Space)
                || window.is_key_down(Key::Up),
            hook: window.get_mouse_down(MouseButton::Right) || window.is_key_down(Key::E),
            aim: Vec2::new(
                (mouse.0 - SCREEN_W as f32 / 2.0) / camera.scale,
                (mouse.1 - SCREEN_H as f32 / 2.0) / camera.scale,
            ),
        };
        // Exercise both movement and hook in CI without synthetic keyboard events.
        let controls = if menu.is_open() {
            SimInput {
                direction: 0, jump: false, hook: false, aim: Vec2::new(1.0, 0.0),
            }
        } else if smoke_output.is_some() || gpu_smoke {
            SimInput {
                direction: 1,
                jump: frames == 2,
                hook: (10..24).contains(&frames),
                aim: Vec2::new(1.0, -0.5),
            }
        } else {
            controls
        };
        for _ in 0..5 {
            if accumulator < PHYSICS_TICK { break; }
            let step = simulator.step(&map, controls)
                .map_err(|e| format!("tick: {e:?}"))?;
            if let Some(out) = audio.as_mut().filter(|_| settings.sound_enabled) {
                out.play_movement(step);
            }
            accumulator -= PHYSICS_TICK;
        }

        if let Some(out) = audio.as_ref() { out.listener(simulator.position); }
        let follow = camera_follow_fraction(frame_duration);
        camera.center.x += (simulator.position.x - camera.center.x) * follow;
        camera.center.y += (simulator.position.y - camera.center.y) * follow;
        let animation_times = EnvelopeTimes::from_tick(
            simulator.tick as f64 + accumulator.as_secs_f64() * 50.0,
        );
        #[cfg(feature = "gpu")]
        let using_gpu = gpu.is_some();
        #[cfg(not(feature = "gpu"))]
        let using_gpu = false;
        if !using_gpu {
            assets.draw_background_at(&mut pixels, &map, camera, animation_times)?;
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
                gpu_tee.render_map(&map, camera)
                    .map_err(|e| format!("GPU collision map fallback: {e:?}"))?;
            }
        }
        let entity = &mut snapshot.items[0].words;
        entity[1] = simulator.position.x.round() as i32;
        entity[2] = simulator.position.y.round() as i32;
        entity[3] = (simulator.character.velocity.x * 256.0).round() as i32;
        entity[4] = (simulator.character.velocity.y * 256.0).round() as i32;
        entity[5] = (controls.aim.y.atan2(controls.aim.x) * 256.0).round() as i32;
        entity[6] = direction;
        entity[19] = selected_weapon;
        entity[20] = 0; // EMOTE_NORMAL
        entity[21] = attack_tick;
        entity[9] = if matches!(simulator.hook.state, HookState::Flying | HookState::Grabbed) {
            simulator.hook.state as i32
        } else { 0 };
        entity[11] = simulator.hook.position.x.round() as i32;
        entity[12] = simulator.hook.position.y.round() as i32;
        #[cfg(feature = "gpu")]
        let tee_frame = if using_gpu { &mut gpu_tee } else { &mut pixels };
        #[cfg(not(feature = "gpu"))]
        let tee_frame = &mut pixels;
        if !native_tees {
            assets.draw_snapshot(tee_frame, &snapshot, camera, simulator.tick)?;
        }
        if !using_gpu {
            assets.draw_foreground_at(&mut pixels, camera, animation_times)?;
        }
        #[cfg(feature = "gpu")]
        if let Some(renderer) = gpu.as_mut() {
            gpu_hud.clear();
            menu.paint(&mut gpu_hud, &settings, Some(&snapshot), ServerStatus {
                endpoint: "LOCAL", stage: "Offline sandbox",
                map: Some(path), tick: Some(simulator.tick as i32), ack: -1,
            });
            if gpu_smoke {
                gpu_hud.write_text(10, 10, "GPU HUD", 0xffffff, 7);
            }
            renderer.render(assets.visual_map.as_ref(), camera, animation_times,
                TeeFrame {
                    state: Some(&snapshot),
                    tick: simulator.tick,
                    skin: assets.default_skin.as_ref(),
                    game: assets.game_sprites.as_ref(),
                }, &gpu_tee, &gpu_hud)?;
            if gpu_smoke && renderer.last_native_tee_vertices() == 0 {
                return Err("GPU smoke presented a frame without native tee sprites".into());
            }
            if gpu_smoke && renderer.last_native_hud_vertices() == 0 {
                return Err("GPU smoke presented a frame without native HUD glyphs".into());
            }
            window.update();
        } else {
            menu.paint(&mut pixels, &settings, Some(&snapshot), ServerStatus {
                endpoint: "LOCAL", stage: "Offline sandbox",
                map: Some(path), tick: Some(simulator.tick as i32), ack: -1,
            });
            window.update_with_buffer(&pixels.pixels, SCREEN_W, SCREEN_H)
                .map_err(|e| format!("present: {e}"))?;
        }
        #[cfg(not(feature = "gpu"))]
        {
            menu.paint(&mut pixels, &settings, Some(&snapshot), ServerStatus {
                endpoint: "LOCAL", stage: "Offline sandbox",
                map: Some(path), tick: Some(simulator.tick as i32), ack: -1,
            });
            window.update_with_buffer(&pixels.pixels, SCREEN_W, SCREEN_H)
                .map_err(|e| format!("present: {e}"))?;
        }
        frames += 1;
        if gpu_smoke && frames >= 30 {
            println!("GPU DDNet X11 window smoke: 30 hardware-presented frames");
            return Ok(());
        }
        if let Some(output) = smoke_output {
            if frames >= 30 {
                let png = pixels.png_bytes().map_err(|e| format!("png: {e:?}"))?;
                std::fs::write(output, png).map_err(|e| format!("write {output}: {e}"))?;
                println!("Textured DDNet map and skin window smoke: 30 frames -> {output}");
                return Ok(());
            }
        }
    }
    if smoke_output.is_some() || gpu_smoke {
        return Err("smoke test window closed before 30 frames".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_spawn_prefers_spawn_tile_and_falls_back_to_safe_ground() {
        let mut bytes = vec![0u8; 5 * 5 * 4];
        for x in 0..5 { bytes[(3 * 5 + x) * 4] = 1; }
        let map = GameMap::from_tile_bytes(5, 5, &bytes).unwrap();
        assert_eq!(offline_spawn(&map), Some(Vec2::new(80.0, 80.0)));
        bytes[(2 * 5 + 1) * 4] = 192;
        let map = GameMap::from_tile_bytes(5, 5, &bytes).unwrap();
        assert_eq!(offline_spawn(&map), Some(Vec2::new(48.0, 80.0)));
        let empty = GameMap::from_tile_bytes(5, 5, &[0; 100]).unwrap();
        assert_eq!(offline_spawn(&empty), None);
    }
}
