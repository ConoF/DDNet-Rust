//! Minimal native DDNet server browser (minifb + 8x8 bitmap font).
//! This is intentionally an initial UI, not the full C++ DDNet menus.

use std::path::Path;

use font8x8::{BASIC_FONTS, UnicodeFonts};
use minifb::{Key, KeyRepeat, Window, WindowOptions};

use crate::graphics::Framebuffer;
use crate::server_browser::{
    BrowserError, ClientSettings, ServerEntry, fetch_master,
    matches_filter, parse_master,
};

const W: usize = 960;
const H: usize = 540;
const BG: u32 = 0x111923;
const PANEL: u32 = 0x1d2a38;
const ACTIVE: u32 = 0x305375;
const TEXT: u32 = 0xe9eef5;
const MUTED: u32 = 0x91a8b9;
const ACCENT: u32 = 0x6fd2e6;

fn rect(frame: &mut Framebuffer, x: usize, y: usize, w: usize,
        h: usize, color: u32) {
    let end_x = x.saturating_add(w).min(frame.width as usize);
    let end_y = y.saturating_add(h).min(frame.height as usize);
    for yy in y.min(end_y)..end_y {
        let start = yy * frame.width as usize;
        for xx in x.min(end_x)..end_x {
            frame.pixels[start + xx] = color;
        }
    }
}

fn draw_text(frame: &mut Framebuffer, x: usize, y: usize, message: &str,
             color: u32, scale: usize, max_chars: usize) {
    for (i, character) in message.chars().take(max_chars).enumerate() {
        // BASIC_FONTS covers Latin characters. Unicode names fall back
        // to '?' without panicking or accessing an invalid glyph.
        let glyph = BASIC_FONTS.get(character).or_else(|| BASIC_FONTS.get('?'));
        let Some(glyph) = glyph else { continue };
        for (line, bits) in glyph.into_iter().enumerate() {
            for bit in 0..8 {
                if bits & (1 << bit) != 0 {
                    rect(frame, x + i * 8 * scale + bit * scale,
                         y + line * scale, scale, scale, color);
                }
            }
        }
    }
}

fn paint(
    frame: &mut Framebuffer, servers: &[ServerEntry],
    filter: &str, hide_empty: bool, selected: usize, source: &str,
    nickname: &str,
) -> Vec<usize> {
    frame.clear(BG);
    rect(frame, 0, 0, W, 56, PANEL);
    rect(frame, 0, 52, W, 3, ACCENT);
    draw_text(frame, 20, 15, "DDNET-RUST   SERVER BROWSER / 0.6", TEXT, 2, 49);
    draw_text(frame, 20, 70, &format!("SEARCH: {filter}_"), ACCENT, 1, 98);
    draw_text(frame, 20, 88, &format!(
        "NICK: {nickname} | F5 REFRESH | F6 HIDE EMPTY: {}",
        if hide_empty { "ON" } else { "OFF" }
    ), MUTED, 1, 100);
    rect(frame, 14, 108, 932, 20, PANEL);
    draw_text(frame, 21, 114, "PLAYERS   MODE          MAP               SERVER", MUTED, 1, 85);

    let matching: Vec<usize> = servers.iter().enumerate()
        .filter(|(_, s)| matches_filter(s, filter, hide_empty))
        .map(|(i, _)| i).collect();
    let visible_rows = 16;
    let start = selected.saturating_sub(visible_rows / 2)
        .min(matching.len().saturating_sub(visible_rows));
    for (on_screen, index) in matching.iter().skip(start)
        .take(visible_rows).enumerate()
    {
        let server = &servers[*index];
        let row_y = 133 + on_screen * 22;
        if start + on_screen == selected {
            rect(frame, 14, row_y, 932, 21, ACTIVE);
        } else if on_screen % 2 == 0 {
            rect(frame, 14, row_y, 932, 21, PANEL);
        }
        let line = format!(
            "{:>2}/{:<2}     {:<12} {:<18} {:<50}",
            server.players, server.max_players,
            server.game_type, server.map, server.name,
        );
        draw_text(frame, 22, row_y + 6, &line, TEXT, 1, 112);
    }
    rect(frame, 0, 503, W, 37, PANEL);
    draw_text(frame, 20, 509, &format!(
        "{}/{} SERVERS  |  UP/DOWN SELECT  |  ENTER JOIN  |  ESC EXIT",
        matching.len(), servers.len(),
    ), TEXT, 1, 107);
    draw_text(frame, 20, 523, &format!("SOURCE: {source}"), MUTED, 1, 105);
    matching
}

fn handle_filter_key(key: Key, filter: &mut String) {
    if key == Key::Backspace {
        filter.pop();
    } else if key == Key::Space {
        if filter.len() < 64 { filter.push(' '); }
    } else {
        let name = format!("{key:?}");
        let symbol = if name.len() == 1 && name.is_ascii() {
            Some(name.chars().next().unwrap_or('_').to_ascii_lowercase())
        } else {
            name.strip_prefix("Key").and_then(|number| {
                if number.len() == 1 { number.chars().next() } else { None }
            })
        };
        if let Some(ch) = symbol {
            if ch.is_ascii_alphanumeric() && filter.len() < 64 {
                filter.push(ch);
            }
        }
    }
}

fn show(
    servers: Vec<ServerEntry>, source: &str, screenshot: Option<&Path>,
) -> Result<(), String> {
    let path = ClientSettings::default_path();
    let mut settings = if screenshot.is_some() {
        ClientSettings::default()
    } else {
        ClientSettings::load(&path).unwrap_or_default()
    };
    let mut window = Window::new(
        "DDNet-Rust: Server Browser", W, H, WindowOptions::default(),
    ).map_err(|e| format!("server browser window: {e}"))?;
    window.set_target_fps(60);
    let mut frame = Framebuffer::new(W as u32, H as u32)
        .map_err(|e| format!("framebuffer: {e:?}"))?;
    let mut selected = 0usize;
    let mut frames = 0usize;
    while window.is_open() && !window.is_key_down(Key::Escape) {
        if screenshot.is_none() {
            for key in window.get_keys_pressed(KeyRepeat::Yes) {
                match key {
                    Key::Down => selected = selected.saturating_add(1),
                    Key::Up => selected = selected.saturating_sub(1),
                    Key::PageDown => selected = selected.saturating_add(10),
                    Key::PageUp => selected = selected.saturating_sub(10),
                    Key::F6 => {
                        settings.hide_empty = !settings.hide_empty;
                        selected = 0;
                    }
                    Key::F5 => {
                        match fetch_master() {
                            Ok((new_list, _url)) => {
                                return show(new_list, "refreshed master", None);
                            }
                            Err(error) => eprintln!("Master refresh failed: {error:?}"),
                        }
                    }
                    Key::Enter => {
                        let matching: Vec<_> = servers.iter()
                            .filter(|s| matches_filter(s, &settings.search, settings.hide_empty))
                            .collect();
                        if !matching.is_empty() {
                            let host = matching[selected.min(matching.len() - 1)].endpoint.clone();
                            settings.last_server = host.clone();
                            settings.save(&path).map_err(|e| format!("save settings: {e:?}"))?;
                            drop(window);
                            return crate::window_online::play_online(&host);
                        }
                    }
                    _ => {
                        let old = settings.search.clone();
                        handle_filter_key(key, &mut settings.search);
                        if old != settings.search { selected = 0; }
                    }
                }
            }
        }
        let matching = paint(
            &mut frame, &servers, &settings.search, settings.hide_empty, selected,
            source, &settings.nickname,
        );
        selected = selected.min(matching.len().saturating_sub(1));
        window.update_with_buffer(&frame.pixels, W, H)
            .map_err(|e| format!("server browser present: {e}"))?;
        frames += 1;
        if let Some(output) = screenshot {
            if frames >= 5 {
                let png = frame.png_bytes().map_err(|e| format!("png: {e:?}"))?;
                std::fs::write(output, png)
                    .map_err(|e| format!("write {}: {e}", output.display()))?;
                return Ok(());
            }
        }
    }
    if screenshot.is_none() {
        settings.save(&path).map_err(|e| format!("save settings: {e:?}"))?;
    }
    Ok(())
}

/// Download and display official DDNet master servers; Enter attempts
/// the existing experimental 0.6 connection only (not 0.7).
pub fn browse_servers() -> Result<(), String> {
    let (servers, url) = fetch_master().map_err(|e| format!("master: {e:?}"))?;
    show(servers, url, None)
}

/// Deterministic Xvfb smoke: no public network access, no local settings
/// access and no attempt to connect to fixture IPs.
pub fn smoke_server_browser(fixture: &str, screenshot: &str) -> Result<(), String> {
    let bytes = std::fs::read(fixture).map_err(|e| format!("fixture: {e}"))?;
    let servers = parse_master(&bytes).map_err(|e| format!("parse fixture: {e:?}"))?;
    show(servers, "local master fixture", Some(Path::new(screenshot)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_to_search_keeps_lowercase_ascii_and_bound() {
        let mut filter = String::new();
        handle_filter_key(Key::A, &mut filter);
        handle_filter_key(Key::Key7, &mut filter);
        handle_filter_key(Key::Space, &mut filter);
        assert_eq!(filter, "a7 ");
        handle_filter_key(Key::Backspace, &mut filter);
        assert_eq!(filter, "a7");
        handle_filter_key(Key::F1, &mut filter);
        assert_eq!(filter, "a7");
        for _ in 0..100 {
            handle_filter_key(Key::B, &mut filter);
        }
        assert_eq!(filter.len(), 64);
    }
}
