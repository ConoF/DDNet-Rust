//! Interactive, offline-only DDNet physics sandbox backed by the Rust CPU renderer.
//! No server connection, texture assets or DDNet GUI are implemented here.

use std::time::{Duration, Instant};

use minifb::{Key, MouseButton, MouseMode, Window, WindowOptions};

use crate::datafile::Datafile;
use crate::graphics::{Camera, Framebuffer};
use crate::hook::HookState;
use crate::map::GameMap;
use crate::physics::Vec2;
use crate::simulator::{SimInput, TeeSimulator};
use crate::snapshot_delta::{SnapshotObject, SnapshotState};

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
    play_offline_impl(path, None)
}

/// Opens the OS window inside a virtual display, advances scripted inputs and
/// emits a real PNG screenshot; intended for GitHub Actions/Xvfb smoke tests.
pub fn smoke_offline(path: &str, output: &str) -> Result<(), String> {
    play_offline_impl(path, Some(output))
}

fn play_offline_impl(path: &str, smoke_output: Option<&str>) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("map read: {e}"))?;
    let file = Datafile::parse(&bytes).map_err(|e| format!("datafile: {e:?}"))?;
    let map = GameMap::from_datafile(&file, 16_777_216)
        .map_err(|e| format!("game layer: {e:?}"))?;
    let spawn = offline_spawn(&map).ok_or("no safe spawn available")?;
    let mut window = Window::new(
        "DDNet-Rust: OFFLINE physics prototype",
        SCREEN_W, SCREEN_H, WindowOptions::default(),
    ).map_err(|e| format!("window: {e}"))?;
    window.set_target_fps(60);
    let mut pixels = Framebuffer::new(SCREEN_W as u32, SCREEN_H as u32)
        .map_err(|e| format!("framebuffer: {e:?}"))?;
    let mut simulator = TeeSimulator::new(spawn);
    let mut camera = Camera { center: spawn, scale: 1.5 };
    let mut snapshot = SnapshotState {
        items: vec![SnapshotObject { type_id: 9, id: 0, words: vec![0; 22] }],
    };
    let mut previous = Instant::now();
    let mut accumulator = Duration::ZERO;
    let mut frames: usize = 0;

    while window.is_open() && !window.is_key_down(Key::Escape) {
        let now = Instant::now();
        accumulator += (now - previous).min(Duration::from_millis(100));
        previous = now;
        if window.is_key_down(Key::R) {
            simulator = TeeSimulator::new(spawn);
            accumulator = Duration::ZERO;
        }
        if let Some((_, wheel)) = window.get_scroll_wheel() {
            if wheel > 0.0 { camera.scale = (camera.scale * 1.12).min(8.0); }
            if wheel < 0.0 { camera.scale = (camera.scale / 1.12).max(0.1); }
        }
        let mouse = window.get_mouse_pos(MouseMode::Discard)
            .unwrap_or((SCREEN_W as f32 * 0.75, SCREEN_H as f32 * 0.5));
        let direction = i32::from(window.is_key_down(Key::D) || window.is_key_down(Key::Right))
            - i32::from(window.is_key_down(Key::A) || window.is_key_down(Key::Left));
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
        let controls = if smoke_output.is_some() {
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
            simulator.step(&map, controls).map_err(|e| format!("tick: {e:?}"))?;
            accumulator -= PHYSICS_TICK;
        }

        camera.center.x += (simulator.position.x - camera.center.x) * 0.2;
        camera.center.y += (simulator.position.y - camera.center.y) * 0.2;
        pixels.render_map(&map, camera).map_err(|e| format!("draw map: {e:?}"))?;
        let entity = &mut snapshot.items[0].words;
        entity[1] = simulator.position.x.round() as i32;
        entity[2] = simulator.position.y.round() as i32;
        entity[9] = if matches!(simulator.hook.state, HookState::Flying | HookState::Grabbed) {
            simulator.hook.state as i32
        } else { 0 };
        entity[11] = simulator.hook.position.x.round() as i32;
        entity[12] = simulator.hook.position.y.round() as i32;
        pixels.render_snapshot(&snapshot, camera)
            .map_err(|e| format!("draw tee: {e:?}"))?;
        window.update_with_buffer(&pixels.pixels, SCREEN_W, SCREEN_H)
            .map_err(|e| format!("present: {e}"))?;
        frames += 1;
        if let Some(output) = smoke_output {
            if frames >= 30 {
                let png = pixels.png_bytes().map_err(|e| format!("png: {e:?}"))?;
                std::fs::write(output, png).map_err(|e| format!("write {output}: {e}"))?;
                println!("Window/X11 render smoke test: 30 frames -> {output}");
                return Ok(());
            }
        }
    }
    if smoke_output.is_some() {
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
