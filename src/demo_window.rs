//! Native TWDEMO playback window with time-based motion and seeking.
//! This is a first interactive player, not the original game's full
//! demo UI or a complete implementation of DDNet visual effects.

use std::path::{Path, PathBuf};
use std::time::Instant;
use font8x8::{BASIC_FONTS, UnicodeFonts};
use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};

use crate::datafile::Datafile;
use crate::demo_playback::{DemoClock, DemoCursor};
use crate::graphics::{Camera, Framebuffer, BACKGROUND};
use crate::huffman::Huffman;
use crate::map::GameMap;
use crate::netobjects::CharacterCoreNet;
use crate::physics::Vec2;
use crate::protocol::write_int;
use crate::window_assets::WindowAssets;

const W: usize = 960;
const H: usize = 540;
const TIMELINE_TOP: usize = 508;
const TIMELINE_LEFT: usize = 24;
const TIMELINE_WIDTH: usize = W - TIMELINE_LEFT * 2;
const HUD: u32 = 0x18202c;
const WHITE: u32 = 0xf1f4fa;
const MUTED: u32 = 0xa6b6c8;
const BLUE: u32 = 0x6fd2e6;

fn fill_rect(frame: &mut Framebuffer, x: usize, y: usize, w: usize, h: usize, color: u32) {
    for yy in y..y.saturating_add(h).min(frame.height as usize) {
        let row = yy * frame.width as usize;
        for xx in x..x.saturating_add(w).min(frame.width as usize) {
            frame.pixels[row + xx] = color;
        }
    }
}

fn text(frame: &mut Framebuffer, x: usize, y: usize, message: &str, color: u32) {
    for (i, ch) in message.chars().take(112).enumerate() {
        if x + (i + 1) * 8 > frame.width as usize { break; }
        if let Some(glyph) = BASIC_FONTS.get(ch).or_else(|| BASIC_FONTS.get('?')) {
            for (row, bits) in glyph.into_iter().enumerate() {
                for bit in 0..8 {
                    if bits & (1 << bit) != 0 {
                        fill_rect(frame, x + i * 8 + bit, y + row, 1, 1, color);
                    }
                }
            }
        }
    }
}

struct ReplayMap {
    collision: GameMap,
    assets: WindowAssets,
}

fn demo_map(cursor: &DemoCursor, path: &Path) -> Result<Option<ReplayMap>, String> {
    let Some(bytes) = cursor.index.embedded_map(path)
        .map_err(|e| format!("embedded demo map: {e:?}"))? else { return Ok(None); };
    let file = Datafile::parse(&bytes).map_err(|e| format!("map datafile: {e:?}"))?;
    let collision = GameMap::from_datafile(&file, 16_777_216)
        .map_err(|e| format!("collision: {e:?}"))?;
    let assets = WindowAssets::from_datafile(&file, None);
    Ok(Some(ReplayMap { collision, assets }))
}

fn camera_target(cursor: &DemoCursor, fallback: Vec2) -> Vec2 {
    cursor.state().items.iter().find_map(CharacterCoreNet::from_object)
        .map(|character| Vec2::new(character.x as f32, character.y as f32))
        .unwrap_or(fallback)
}

fn paint(
    frame: &mut Framebuffer, cursor: &DemoCursor, clock: &DemoClock,
    map: Option<&ReplayMap>, camera: Camera,
) -> Result<(), String> {
    frame.clear(BACKGROUND);
    if let Some(map) = map {
        map.assets.draw_map(frame, &map.collision, camera)?;
        map.assets.draw_snapshot(frame, cursor.state(), camera,
                                 cursor.current_tick() as u64)?;
    } else {
        frame.render_snapshot(cursor.state(), camera)
            .map_err(|e| format!("demo characters: {e:?}"))?;
    }
    fill_rect(frame, 0, 0, W, 26, HUD);
    text(frame, 15, 9, "DDNET-RUST  DEMO PLAYER", WHITE);
    fill_rect(frame, 0, 470, W, H - 470, HUD);
    let first = cursor.index.frames.first().map_or(0, |f| f.tick);
    let last = cursor.index.frames.last().map_or(first, |f| f.tick);
    let seconds = (cursor.current_tick() - first) as f32 / 50.0;
    text(frame, TIMELINE_LEFT, 480, &format!(
        "{}  {:.1}s  tick {}/{}   {:.2}x  {}",
        cursor.index.info.map_name, seconds, cursor.current_tick(),
        last, clock.speed(), if clock.paused { "PAUSED" } else { "PLAYING" }
    ), WHITE);
    text(frame, TIMELINE_LEFT, 493,
        "SPACE pause | LEFT/RIGHT frame | PGUP/DN 10s | UP/DN speed | HOME/END", MUTED);
    fill_rect(frame, TIMELINE_LEFT, TIMELINE_TOP, TIMELINE_WIDTH, 12, 0x3d4e62);
    let span = (last - first).max(1) as usize;
    let progressed = (cursor.current_tick() - first).max(0) as usize * TIMELINE_WIDTH / span;
    fill_rect(frame, TIMELINE_LEFT, TIMELINE_TOP, progressed.max(1), 12, BLUE);
    for marker in cursor.index.frames.iter().filter(|f| f.keyframe) {
        let at = (marker.tick - first).max(0) as usize * TIMELINE_WIDTH / span;
        fill_rect(frame, TIMELINE_LEFT + at.min(TIMELINE_WIDTH - 1),
                  TIMELINE_TOP, 1, 12, 0xffdf7d);
    }
    text(frame, TIMELINE_LEFT, 526, "CLICK timeline: seek | R: restart | wheel: zoom | ESC: exit", MUTED);
    Ok(())
}

fn play_impl(path: &Path, screenshot: Option<&Path>) -> Result<(), String> {
    let mut cursor = DemoCursor::open(path).map_err(|e| format!("demo read: {e:?}"))?;
    let map = demo_map(&cursor, path)?;
    if screenshot.is_some() && map.as_ref().is_none_or(|m|
        m.assets.visual_map.is_none() || m.assets.default_skin.is_none()
    ) {
        return Err("smoke needs embedded map and original graphics resources".into());
    }
    let mut clock = DemoClock::new(cursor.current_tick());
    let mut camera = Camera {
        center: camera_target(&cursor, Vec2::default()), scale: 1.5
    };
    let mut frame = Framebuffer::new(W as u32, H as u32)
        .map_err(|e| format!("framebuffer: {e:?}"))?;
    let mut window = Window::new(
        "DDNet-Rust - Demo Player", W, H, WindowOptions::default()
    ).map_err(|e| format!("open window: {e}"))?;
    window.set_target_fps(60);
    let mut previous = Instant::now();
    let mut frames = 0usize;
    let first = cursor.index.frames[0].tick;
    let last = cursor.index.frames.last().map_or(first, |f| f.tick);
    while window.is_open() && !window.is_key_down(Key::Escape) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(previous);
        previous = now;
        if screenshot.is_some() {
            if frames == 0 {
                let target = first + (last - first) / 2;
                cursor.seek_tick(target).map_err(|e| format!("demo seek: {e:?}"))?;
                clock.seek(target);
                clock.faster();
                clock.paused = true;
            }
        } else {
            for key in window.get_keys_pressed(KeyRepeat::No) {
                match key {
                    Key::Space => clock.paused = !clock.paused,
                    Key::Up => clock.faster(),
                    Key::Down => clock.slower(),
                    Key::Left | Key::Right => {
                        let next = if key == Key::Left {
                            cursor.frame_index().saturating_sub(1)
                        } else {
                            (cursor.frame_index() + 1).min(cursor.index.frames.len() - 1)
                        };
                        cursor.seek_frame(next).map_err(|e| format!("step: {e:?}"))?;
                        clock.seek(cursor.current_tick());
                        clock.paused = true;
                    }
                    Key::PageUp | Key::PageDown => {
                        let amount = if key == Key::PageUp { 500 } else { -500 };
                        let tick = cursor.current_tick().saturating_add(amount)
                            .clamp(first, last);
                        cursor.seek_tick(tick).map_err(|e| format!("seek: {e:?}"))?;
                        clock.seek(tick);
                    }
                    Key::Home | Key::End => {
                        let tick = if key == Key::Home { first } else { last };
                        cursor.seek_tick(tick).map_err(|e| format!("seek: {e:?}"))?;
                        clock.seek(tick);
                        clock.paused = true;
                    }
                    Key::R => {
                        cursor.seek_tick(first).map_err(|e| format!("restart: {e:?}"))?;
                        clock.seek(first);
                        clock.paused = false;
                    }
                    _ => {}
                }
            }
            if window.get_mouse_down(MouseButton::Left) {
                if let Some((mx, my)) = window.get_mouse_pos(MouseMode::Discard) {
                    if (TIMELINE_TOP as f32..=(TIMELINE_TOP + 12) as f32).contains(&my)
                        && (TIMELINE_LEFT as f32..=(TIMELINE_LEFT + TIMELINE_WIDTH) as f32)
                            .contains(&mx)
                    {
                        let ratio = ((mx - TIMELINE_LEFT as f32)
                            / TIMELINE_WIDTH as f32).clamp(0.0, 1.0);
                        let tick = first + ((last - first) as f32 * ratio) as i32;
                        cursor.seek_tick(tick).map_err(|e| format!("timeline: {e:?}"))?;
                        clock.seek(tick);
                        clock.paused = true;
                    }
                }
            }
            if let Some((_, wheel)) = window.get_scroll_wheel() {
                if wheel > 0.0 { camera.scale = (camera.scale * 1.12).min(8.0); }
                if wheel < 0.0 { camera.scale = (camera.scale / 1.12).max(0.1); }
            }
            if !clock.paused {
                let tick = clock.advance(elapsed, last);
                cursor.seek_tick(tick).map_err(|e| format!("advance: {e:?}"))?;
            }
        }
        let target = camera_target(&cursor, camera.center);
        if screenshot.is_some() {
            camera.center = target;
        } else {
            camera.center.x += (target.x - camera.center.x) * 0.25;
            camera.center.y += (target.y - camera.center.y) * 0.25;
        }
        paint(&mut frame, &cursor, &clock, map.as_ref(), camera)?;
        window.update_with_buffer(&frame.pixels, W, H)
            .map_err(|e| format!("present: {e}"))?;
        frames += 1;
        if let Some(png) = screenshot {
            if frames >= 6 {
                std::fs::write(png, frame.png_bytes()
                    .map_err(|e| format!("PNG: {e}"))?)
                    .map_err(|e| format!("write screenshot: {e}"))?;
                return Ok(());
            }
        }
    }
    Ok(())
}

pub fn play_demo(path: &Path) -> Result<(), String> { play_impl(path, None) }

/// Deterministic Xvfb smoke: build genuine compressed snapshot and delta
/// chunks, plus a real map embedded in the demo, then render them in the
/// same native player codepath as user recordings.
pub fn smoke_demo(map_path: &Path, screenshot: &Path) -> Result<(), String> {
    let map_data = std::fs::read(map_path).map_err(|e| format!("map: {e}"))?;
    let parsed = Datafile::parse(&map_data).map_err(|e| format!("map data: {e:?}"))?;
    let map = GameMap::from_datafile(&parsed, 16_777_216)
        .map_err(|e| format!("map collision: {e:?}"))?;
    let spawn = crate::window::offline_spawn(&map)
        .ok_or_else(|| "no valid spawn".to_owned())?;
    let data = make_demo_fixture(&map_data, spawn.x as i32, spawn.y as i32);
    let temp: PathBuf = std::env::temp_dir().join(format!(
        "ddnet-rust-demo-smoke-{}.demo", std::process::id(),
    ));
    std::fs::write(&temp, data).map_err(|e| format!("create demo: {e}"))?;
    let result = play_impl(&temp, Some(screenshot));
    let _ = std::fs::remove_file(temp);
    result
}

fn append_compressed(out: &mut Vec<u8>, kind: u8, words: &[i32], codec: &Huffman) {
    let mut packed = Vec::new();
    for &word in words { write_int(word, &mut packed); }
    let compressed = codec.compress(&packed, 65_535).expect("small fixed fixture");
    let len = compressed.len();
    if len < 30 {
        out.push((kind << 5) | len as u8);
    } else if len < 256 {
        out.extend_from_slice(&[(kind << 5) | 30, len as u8]);
    } else {
        out.extend_from_slice(&[(kind << 5) | 31, len as u8, (len >> 8) as u8]);
    }
    out.extend_from_slice(&compressed);
}

fn make_demo_fixture(map: &[u8], x: i32, y: i32) -> Vec<u8> {
    let mut bytes = vec![0u8; 176 + 260];
    bytes[..7].copy_from_slice(b"TWDEMO\0");
    bytes[7] = 5;
    bytes[8..11].copy_from_slice(b"0.6");
    bytes[72..76].copy_from_slice(b"ctf1");
    bytes[136..140].copy_from_slice(&(map.len() as u32).to_be_bytes());
    bytes[144..150].copy_from_slice(b"client");
    bytes[152..156].copy_from_slice(&4u32.to_be_bytes());
    bytes.extend_from_slice(map);
    let codec = Huffman::new();
    let mut character = vec![0i32; 22];
    character[1] = x;
    character[2] = y;
    character[19] = 1;
    for i in 0..=200i32 {
        if i == 0 || i == 100 {
            bytes.push(0xc0);
            bytes.extend_from_slice(&(50 + i).to_be_bytes());
            character[1] = x + i * 3;
            let mut full = vec![92, 1, 0, 9 << 16];
            full.extend_from_slice(&character);
            append_compressed(&mut bytes, 1, &full, &codec);
        } else {
            bytes.push(0xa1);
            let mut delta = vec![0, 1, 0, 9, 0];
            let mut change = vec![0; 22];
            change[1] = 3;
            delta.extend_from_slice(&change);
            append_compressed(&mut bytes, 3, &delta, &codec);
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_fixture_seeks_across_second_keyframe() {
        let path = std::env::temp_dir().join(format!(
            "ddnet-rust-demo-fixture-{}.demo", std::process::id(),
        ));
        std::fs::write(&path, make_demo_fixture(&[], 100, 200)).unwrap();
        let mut cursor = DemoCursor::open(&path).unwrap();
        cursor.seek_tick(149).unwrap();
        assert_eq!(cursor.state().find(9, 0).unwrap().words[1], 397);
        cursor.seek_tick(150).unwrap();
        assert_eq!(cursor.state().find(9, 0).unwrap().words[1], 400);
        cursor.seek_tick(250).unwrap();
        assert_eq!(cursor.state().find(9, 0).unwrap().words[1], 700);
        cursor.seek_tick(52).unwrap();
        assert_eq!(cursor.state().find(9, 0).unwrap().words[1], 106);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn native_player_timeline_renders_with_clipped_text() {
        let path = std::env::temp_dir().join(format!(
            "ddnet-rust-demo-draw-{}.demo", std::process::id(),
        ));
        std::fs::write(&path, make_demo_fixture(&[], 100, 200)).unwrap();
        let cursor = DemoCursor::open(&path).unwrap();
        let mut pixels = Framebuffer::new(W as u32, H as u32).unwrap();
        let clock = DemoClock::new(cursor.current_tick());
        paint(&mut pixels, &cursor, &clock, None, Camera::default()).unwrap();
        assert_eq!(pixels.pixels[TIMELINE_TOP * W + TIMELINE_LEFT], 0xffdf7d);
        assert_eq!(pixels.pixels[TIMELINE_TOP * W + W - 1], HUD);
        std::fs::remove_file(path).unwrap();
    }
}
