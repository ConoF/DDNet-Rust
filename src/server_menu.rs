//! Minimal native DDNet server browser (minifb + 8x8 bitmap font).
//! This is intentionally an initial UI, not the full C++ DDNet menus.

use std::path::Path;

use font8x8::{BASIC_FONTS, UnicodeFonts};
use minifb::{Key, KeyRepeat, Window, WindowOptions};

use crate::graphics::Framebuffer;
use crate::server_browser::{
    ClientSettings, ServerEntry, fetch_master,
    visible_indices, parse_master,
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
    settings: &ClientSettings, selected: usize, source: &str,
) -> Vec<usize> {
    frame.clear(BG);
    rect(frame, 0, 0, W, 56, PANEL);
    rect(frame, 0, 52, W, 3, ACCENT);
    draw_text(frame, 20, 15, "DDNET-RUST   SERVER BROWSER / 0.6", TEXT, 2, 49);
    draw_text(frame, 20, 70, &format!("SEARCH: {}_", settings.search), ACCENT, 1, 98);
    draw_text(frame, 20, 88, &format!(
        "NICK: {} | F2 FAVORITES: {} | F6 HIDE EMPTY: {} | F5 REFRESH",
        settings.nickname,
        if settings.show_favorites { "ONLY" } else { "ALL" },
        if settings.hide_empty { "ON" } else { "OFF" },
    ), MUTED, 1, 110);
    rect(frame, 14, 108, 932, 20, PANEL);
    draw_text(frame, 21, 114, "PLAYERS   MODE          MAP               SERVER", MUTED, 1, 85);

    let matching = visible_indices(servers, settings);
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
        let marker = if settings.is_favorite(&server.endpoint) { "+" } else { " " };
        let line = format!(
            "{marker} {:>2}/{:<2}  {:<12} {:<18} {:<49}",
            server.players, server.max_players,
            server.game_type, server.map, server.name,
        );
        draw_text(frame, 22, row_y + 6, &line, TEXT, 1, 112);
    }
    rect(frame, 0, 503, W, 37, PANEL);
    draw_text(frame, 20, 509, &format!(
        "{}/{} SERVERS | UP/DOWN | ENTER JOIN | F FAVORITE | F2 FAVORITES | ESC",
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
    mut servers: Vec<ServerEntry>, source: &str, screenshot: Option<&Path>,
) -> Result<Option<String>, String> {
    let mut source_label = source.to_owned();
    let path = ClientSettings::default_path();
    let mut settings = if screenshot.is_some() {
        ClientSettings::default()
    } else {
        ClientSettings::load(&path).unwrap_or_default()
    };
    let mut window = Window::new(
        "DDNet-Rust: Server Browser", W, H, WindowOptions::default(),
    ).map_err(|e| format!("server browser window: {e}"))?;
    window.set_target_fps(usize::from(settings.fps_limit));
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
                    Key::F2 => {
                        settings.show_favorites = !settings.show_favorites;
                        selected = 0;
                    }
                    Key::F => {
                        let matching = visible_indices(&servers, &settings);
                        if let Some(index) = matching.get(selected) {
                            let endpoint = servers[*index].endpoint.clone();
                            let favorite = !settings.is_favorite(&endpoint);
                            settings.set_favorite(&endpoint, favorite)
                                .map_err(|e| format!("favorite: {e:?}"))?;
                            settings.save(&path)
                                .map_err(|e| format!("save favorites: {e:?}"))?;
                            selected = selected.min(visible_indices(&servers, &settings)
                                .len().saturating_sub(1));
                        }
                    }
                    Key::F5 => {
                        match fetch_master() {
                            Ok((new_list, new_source)) => {
                                servers = new_list;
                                source_label = new_source.to_owned();
                                selected = 0;
                            }
                            Err(error) => eprintln!("Master refresh failed: {error:?}"),
                        }
                    }
                    Key::Enter => {
                        let matching = visible_indices(&servers, &settings);
                        if !matching.is_empty() {
                            let idx = matching[selected.min(matching.len() - 1)];
                            let host = servers[idx].endpoint.clone();
                            settings.last_server = host.clone();
                            settings.save(&path).map_err(|e| format!("save settings: {e:?}"))?;
                            return Ok(Some(host));
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
            &mut frame, &servers, &settings, selected, &source_label,
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
                return Ok(None);
            }
        }
    }
    if screenshot.is_none() {
        settings.save(&path).map_err(|e| format!("save settings: {e:?}"))?;
    }
    Ok(None)
}

/// Download and display official DDNet master servers; Enter attempts
/// the existing experimental 0.6 connection only (not 0.7).
pub fn browse_servers() -> Result<(), String> {
    let (servers, url) = fetch_master().map_err(|e| format!("master: {e:?}"))?;
    // Re-open the browser after an in-game Disconnect without recursive
    // menus and without leaking any network or window resources.
    loop {
        let Some(endpoint) = show(servers.clone(), url, None)? else {
            return Ok(());
        };
        if let Err(error) = crate::window_online::play_online(&endpoint) {
            eprintln!("Disconnected from {endpoint}: {error}");
        }
    }
}

/// Deterministic Xvfb smoke: no public network access, no local settings
/// access and no attempt to connect to fixture IPs.
pub fn smoke_server_browser(fixture: &str, screenshot: &str) -> Result<(), String> {
    let bytes = std::fs::read(fixture).map_err(|e| format!("fixture: {e}"))?;
    let servers = parse_master(&bytes).map_err(|e| format!("parse fixture: {e:?}"))?;
    show(servers, "local master fixture", Some(Path::new(screenshot)))
        .map(|_| ())
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

    #[test]
    fn favorites_view_filter_and_selection_refer_to_same_master_rows() {
        let listing = r#"{"servers":[
            {"addresses":["tw-0.6+udp://127.0.0.1:8303"],
             "info":{"name":"One","num_players":10,"map":"Sunny"}},
            {"addresses":["tw-0.6+udp://127.0.0.1:8304"],
             "info":{"name":"Two","num_players":2,"map":"Kobra"}}
        ]}"#;
        let servers = parse_master(listing.as_bytes()).unwrap();
        let mut settings = ClientSettings::default();
        settings.set_favorite("127.0.0.1:8304", true).unwrap();
        settings.show_favorites = true;
        let shown = visible_indices(&servers, &settings);
        assert_eq!(shown.len(), 1);
        assert_eq!(servers[shown[0]].endpoint, "127.0.0.1:8304");
        let mut frame = Framebuffer::new(W as u32, H as u32).unwrap();
        assert_eq!(paint(&mut frame, &servers, &settings, 0, "fixture").len(), 1);
    }
}
