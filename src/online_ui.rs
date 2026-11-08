//! Small native in-game controls / chat HUD for DDNet 0.6.
//!
//! Keyboard-to-Unicode text uses minifb's input callback, so chat messages
//! preserve non-ASCII text on the wire. The fallback bitmap glyphs still
//! render only Basic Latin until a proper DDNet font renderer is ported.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use font8x8::{BASIC_FONTS, UnicodeFonts};
use minifb::InputCallback;

use crate::gameplay::MAX_CHAT_BYTES;
use crate::graphics::Framebuffer;
use crate::netobjects::{player_name, PlayerInfoNet};
use crate::snapshot_delta::SnapshotState;

const TEXT: u32 = 0xf2f5fa;
const ACCENT: u32 = 0x80e0ee;
const NOTICE: u32 = 0xf5c86c;
const BACKGROUND: u32 = 0x14222f;
const MAX_LINES: usize = 9;
const VISIBLE_FOR: Duration = Duration::from_secs(16);

pub struct TextInputCollector(pub Rc<RefCell<Vec<char>>>);

impl InputCallback for TextInputCollector {
    fn add_char(&mut self, uni_char: u32) {
        if let Some(character) = char::from_u32(uni_char) {
            if !character.is_control() {
                let mut chars = self.0.borrow_mut();
                if chars.len() < MAX_CHAT_BYTES {
                    chars.push(character);
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft {
    team: bool,
    text: String,
}

#[derive(Debug, Clone)]
struct Line {
    text: String,
    arrived_at: Instant,
}

#[derive(Debug, Clone, Default)]
pub struct OnlineHud {
    draft: Option<Draft>,
    lines: VecDeque<Line>,
    announcement: Option<Line>,
}

fn rect(frame: &mut Framebuffer, x: usize, y: usize, width: usize,
    height: usize, color: u32)
{
    let max_x = (x + width).min(frame.width as usize);
    let max_y = (y + height).min(frame.height as usize);
    for row in y.min(max_y)..max_y {
        let offset = row * frame.width as usize;
        for col in x.min(max_x)..max_x {
            frame.pixels[offset + col] = color;
        }
    }
}

fn text(frame: &mut Framebuffer, x: usize, y: usize,
    value: &str, color: u32, max_chars: usize)
{
    for (index, ch) in value.chars().take(max_chars).enumerate() {
        let Some(glyph) = BASIC_FONTS.get(ch).or_else(|| BASIC_FONTS.get('?'))
            else { continue; };
        for (row, bits) in glyph.iter().enumerate() {
            for bit in 0..8 {
                if bits & (1 << bit) != 0 {
                    rect(frame, x + index * 8 + bit, y + row, 1, 1, color);
                }
            }
        }
    }
}

impl OnlineHud {
    pub fn new() -> Self { Self::default() }

    pub fn is_typing(&self) -> bool { self.draft.is_some() }

    pub fn open_chat(&mut self, team: bool) {
        if self.draft.is_none() {
            self.draft = Some(Draft { team, text: String::new() });
        }
    }

    pub fn cancel_chat(&mut self) { self.draft = None; }

    pub fn backspace(&mut self) {
        if let Some(draft) = self.draft.as_mut() {
            draft.text.pop();
        }
    }

    pub fn append_typed(&mut self, chars: &[char]) {
        let Some(draft) = self.draft.as_mut() else { return };
        for &ch in chars {
            if !ch.is_control() && draft.text.len() + ch.len_utf8() <= MAX_CHAT_BYTES {
                draft.text.push(ch);
            }
        }
    }

    pub fn submit_chat(&mut self) -> Option<(bool, String)> {
        let draft = self.draft.take()?;
        let trimmed = draft.text.trim();
        if trimmed.is_empty() { return None; }
        Some((draft.team, trimmed.to_owned()))
    }

    pub fn chat_line(&mut self, prefix: &str, message: &str) {
        let sanitized = message.replace(['\n', '\r'], " ");
        self.lines.push_back(Line {
            text: format!("{prefix}{sanitized}"),
            arrived_at: Instant::now(),
        });
        while self.lines.len() > MAX_LINES { self.lines.pop_front(); }
    }

    pub fn announcement(&mut self, message: &str) {
        let sanitized = message.replace(['\n', '\r'], " ");
        self.announcement = Some(Line {
            text: sanitized,
            arrived_at: Instant::now(),
        });
    }

    fn draw_scoreboard(&self, frame: &mut Framebuffer, snapshot: &SnapshotState) {
        let width = frame.width as usize;
        let height = frame.height as usize;
        if width < 320 || height < 200 { return; }
        let panel_width = width.min(820).saturating_sub(60);
        let panel_x = (width - panel_width) / 2;
        let panel_y = 52;
        let panel_height = (height - 112).min(370);
        rect(frame, panel_x, panel_y, panel_width, panel_height, BACKGROUND);
        text(frame, panel_x + 14, panel_y + 10,
            "SCOREBOARD | TAB TO CLOSE", ACCENT, (panel_width - 24) / 8);

        let mut players = snapshot.items.iter()
            .filter_map(PlayerInfoNet::from_object)
            .collect::<Vec<_>>();
        players.sort_by(|a, b|
            b.score.cmp(&a.score).then(a.client_id.cmp(&b.client_id))
        );
        let columns = if panel_width >= 600 { 2 } else { 1 };
        let per_column = ((panel_height - 55) / 16).min(20);
        for (idx, player) in players.iter().take(columns * per_column).enumerate() {
            let column = idx / per_column;
            let line = idx % per_column;
            let col_width = panel_width / columns;
            let x = panel_x + 14 + column * col_width;
            let y = panel_y + 34 + line * 16;
            let name = u16::try_from(player.client_id).ok()
                .and_then(|id| player_name(snapshot, id))
                .unwrap_or_else(|| format!("#{}", player.client_id));
            let label = format!("{:>4} {:<16} {:>4}",
                player.score, name, player.latency);
            text(frame, x, y, &label,
                if player.local { ACCENT } else { TEXT },
                col_width.saturating_sub(28) / 8);
        }
        if players.len() > columns * per_column {
            text(frame, panel_x + 14, panel_y + panel_height - 16,
                &format!("{} MORE PLAYERS NOT SHOWN",
                    players.len() - columns * per_column),
                NOTICE, (panel_width - 24) / 8);
        }
    }

    pub fn render(
        &self, frame: &mut Framebuffer, stage: &str,
        local_weapon: Option<i32>, server_tick: Option<i32>,
        scoreboard: Option<&SnapshotState>,
    ) {
        let width = frame.width as usize;
        let height = frame.height as usize;
        if width < 240 || height < 120 { return; }

        rect(frame, 0, 0, width, 19, BACKGROUND);
        text(frame, 8, 5,
            &format!("DDNET-RUST | {stage} | T/Y CHAT | 1-5 WEAPONS | F7 SPEC | F8 PLAY | F10 KILL | ESC"),
            TEXT, (width - 16) / 8);

        let weapon = match local_weapon {
            Some(0) => "HAMMER",
            Some(1) => "GUN",
            Some(2) => "SHOTGUN",
            Some(3) => "GRENADE",
            Some(4) => "LASER",
            Some(5) => "NINJA",
            _ => "UNKNOWN",
        };
        rect(frame, 0, height - 20, width, 20, BACKGROUND);
        text(frame, 8, height - 14,
            &format!("WEAPON: {weapon} | TICK: {} | WHEEL SWITCH | +/- ZOOM",
                server_tick.map_or(String::from("?"), |tick| tick.to_string())),
            ACCENT, (width - 16) / 8);

        for (i, line) in self.lines.iter().rev().filter(|line|
            line.arrived_at.elapsed() < VISIBLE_FOR
        ).take(MAX_LINES).enumerate() {
            let y = 26 + i * 13;
            rect(frame, 6, y, (width - 12).min(680), 12, BACKGROUND);
            text(frame, 12, y + 2, &line.text, TEXT, (width - 24).min(664) / 8);
        }

        if let Some(line) = self.announcement.as_ref().filter(|line|
            line.arrived_at.elapsed() < Duration::from_secs(7)
        ) {
            let y = height / 3;
            rect(frame, 6, y, width - 12, 16, BACKGROUND);
            text(frame, 12, y + 4, &line.text, NOTICE, (width - 24) / 8);
        }
        if let Some(snapshot) = scoreboard {
            self.draw_scoreboard(frame, snapshot);
        }
        if let Some(draft) = self.draft.as_ref() {
            let y = height - 45;
            rect(frame, 0, y, width, 24, BACKGROUND);
            let prompt = if draft.team { "TEAM" } else { "ALL" };
            let line = format!("[{prompt}] {}_", draft.text);
            let cols = (width - 16) / 8;
            let truncated = line.chars().rev().take(cols).collect::<String>()
                .chars().rev().collect::<String>();
            text(frame, 8, y + 8, &truncated, ACCENT, cols);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_chat_editor_has_byte_limits_and_cancel() {
        let mut hud = OnlineHud::new();
        hud.open_chat(true);
        hud.append_typed(&['П', 'р', 'и', 'в', 'е', 'т']);
        assert_eq!(hud.submit_chat(), Some((true, "Привет".to_owned())));
        assert!(!hud.is_typing());
        hud.open_chat(false);
        hud.append_typed(&['a', 'b', 'c']);
        hud.backspace();
        assert_eq!(hud.submit_chat(), Some((false, "ab".into())));
        hud.open_chat(false);
        hud.append_typed(&vec!['я'; 600]);
        assert!(hud.draft.as_ref().unwrap().text.len() <= MAX_CHAT_BYTES);
        hud.cancel_chat();
        assert_eq!(hud.submit_chat(), None);
    }

    #[test]
    fn bounded_chat_feed_preserves_recent_messages() {
        let mut hud = OnlineHud::new();
        for i in 0..30 { hud.chat_line("[server] ", &format!("line {i}")); }
        assert_eq!(hud.lines.len(), MAX_LINES);
        assert_eq!(hud.lines.front().unwrap().text, "[server] line 21");
        hud.announcement("map changed");
        assert_eq!(hud.announcement.as_ref().unwrap().text, "map changed");
    }

    #[test]
    fn scoreboard_displays_snapshot_without_mutating_it() {
        use crate::snapshot_delta::SnapshotObject;
        let hud = OnlineHud::new();
        let state = SnapshotState { items: vec![
            SnapshotObject { type_id: 10, id: 3, words: vec![1, 3, 0, 50, 30] },
            SnapshotObject { type_id: 10, id: 4, words: vec![0, 4, 0, 20, 80] },
        ] };
        let before = state.clone();
        let mut frame = Framebuffer::new(640, 480).unwrap();
        frame.clear(0);
        hud.render(&mut frame, "Online", None, Some(100), Some(&state));
        assert_eq!(state, before);
        assert!(frame.pixels.iter().any(|&pixel| pixel != 0));
    }

    #[test]
    fn overlay_renders_in_bounded_native_framebuffer() {
        let mut hud = OnlineHud::new();
        hud.open_chat(false);
        hud.append_typed(&['h', 'i']);
        hud.chat_line("[1] ", "test");
        let mut frame = Framebuffer::new(320, 180).unwrap();
        frame.clear(0);
        hud.render(&mut frame, "Online", Some(1), Some(123), None);
        assert!(frame.pixels.iter().any(|&px| px != 0));
    }
}
