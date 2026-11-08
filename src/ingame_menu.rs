//! Native in-game DDNet menu layout and keyboard/mouse navigation.
//! Tabs and settings groups follow the pinned 20.2.1 menu structure;
//! unsupported settings are shown as read-only, not misleading toggles.
//! This painter uses HudCanvas so it runs on native GPU glyph batches and
//! on the software framebuffer without a per-frame screen upload.

use minifb::Key;

use crate::online_ui::HudCanvas;
use crate::netobjects::{player_name, PlayerInfoNet};
use crate::server_browser::{ClientSettings, BIND_ACTIONS};
use crate::snapshot_delta::SnapshotState;

const PANEL: u32 = 0x213344;
const SHADOW: u32 = 0x101b28;
const HEADER: u32 = 0x304b61;
const SELECTED: u32 = 0x467993;
const WHITE: u32 = 0xf2f5f5;
const PALE: u32 = 0xb8c8d3;
const ACCENT: u32 = 0x9adce8;
const WARNING: u32 = 0xf4c06b;
const ROW_HEIGHT: usize = 36;
const BIND_ROW_HEIGHT: usize = 26;

/// Soft 5px rounded DDNet-style menu panels. Each scanline strip is
/// disjoint, preventing alpha-overlap darkening on transparent UI.
fn rounded_panel<C: HudCanvas>(
    canvas: &mut C, rect: (usize, usize, usize, usize),
    color: u32, alpha: u8,
) {
    let (x,y,w,h) = rect;
    if w < 12 || h < 12 {
        canvas.fill_translucent_rect(x,y,w,h,color,alpha);
        return;
    }
    for (top, inset) in [(0,5),(1,3),(2,2),(3,1),(4,1)] {
        canvas.fill_translucent_rect(
            x+inset,y+top,w-2*inset,1,color,alpha,
        );
        canvas.fill_translucent_rect(
            x+inset,y+h-1-top,w-2*inset,1,color,alpha,
        );
    }
    canvas.fill_translucent_rect(x,y+5,w,h-10,color,alpha);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page { #[default] Game, Players, ServerInfo, Settings }

impl Page {
    const ALL: [Self; 4] = [Self::Game, Self::Players, Self::ServerInfo, Self::Settings];
    fn title(self) -> &'static str {
        match self {
            Self::Game => "GAME", Self::Players => "PLAYERS",
            Self::ServerInfo => "SERVER INFO", Self::Settings => "SETTINGS",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsTab {
    #[default] General, Player, Tee, Appearance, Controls, Graphics, Sound,
}
impl SettingsTab {
    const ALL: [Self; 7] = [Self::General, Self::Player, Self::Tee,
        Self::Appearance, Self::Controls, Self::Graphics, Self::Sound];
    fn title(self) -> &'static str {
        match self {
            Self::General => "GENERAL", Self::Player => "PLAYER", Self::Tee => "TEE",
            Self::Appearance => "APPEARANCE", Self::Controls => "CONTROLS",
            Self::Graphics => "GRAPHICS", Self::Sound => "SOUND",
        }
    }
    fn row_count(self) -> usize {
        match self {
            Self::General => 2, Self::Player => 1,
            Self::Graphics => 2, Self::Sound => 1,
            Self::Controls => BIND_ACTIONS.len(),
            Self::Appearance | Self::Tee => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    Resume,
    Disconnect,
    Spectate,
    Join,
    Kill,
    SettingsChanged,
    ChangeNickname(String),
}

#[derive(Debug, Clone, Default)]
pub struct MenuState {
    open: bool,
    pub page: Page,
    pub settings_tab: SettingsTab,
    selected: usize,
    nickname_edit: Option<String>,
    binding_edit: Option<usize>,
    feedback: Option<String>,
}

#[derive(Clone, Copy)]
pub struct ServerStatus<'a> {
    pub endpoint: &'a str,
    pub stage: &'a str,
    pub map: Option<&'a str>,
    pub tick: Option<i32>,
    pub ack: i32,
}

#[derive(Clone, Copy)]
struct Layout {
    x: usize, y: usize, w: usize, h: usize,
}
impl Layout {
    fn for_size(width: usize, height: usize) -> Self {
        let w = width.saturating_sub(80).min(820);
        let h = height.saturating_sub(40).min(490);
        Self {
            x: (width - w) / 2,
            y: (height - h) / 2,
            w, h,
        }
    }
    fn tab_origin(self) -> usize { self.x + 22 }
    fn category_x(self) -> usize { self.x + 28 }
    fn content_x(self) -> usize { self.x + 194 }
    fn content_y(self) -> usize { self.y + 140 }
}

impl MenuState {
    pub fn new() -> Self { Self::default() }
    pub fn is_open(&self) -> bool { self.open }
    pub fn is_editing(&self) -> bool { self.nickname_edit.is_some() }
    pub fn is_capturing_binding(&self) -> bool { self.binding_edit.is_some() }

    pub fn bind_mouse(&mut self, button: &str, settings: &mut ClientSettings)
        -> Option<MenuAction>
    {
        let index = self.binding_edit.take()?;
        if settings.bindings.set(index, button) {
            self.feedback(format!("{} -> {}", BIND_ACTIONS[index], button));
            Some(MenuAction::SettingsChanged)
        } else {
            self.feedback("Binding unchanged or invalid");
            None
        }
    }

    pub fn toggle(&mut self) {
        if self.binding_edit.is_some() {
            self.binding_edit = None;
        } else if self.nickname_edit.is_some() {
            self.nickname_edit = None;
        } else {
            self.open = !self.open;
        }
    }
    pub fn close(&mut self) {
        self.open = false;
        self.nickname_edit = None;
        self.binding_edit = None;
    }
    pub fn feedback(&mut self, feedback: impl Into<String>) {
        self.feedback = Some(feedback.into());
    }

    fn page_select(&mut self, page: Page) {
        self.page = page;
        self.selected = 0;
        self.nickname_edit = None;
        self.binding_edit = None;
        self.feedback = None;
    }
    fn select_tab(&mut self, tab: SettingsTab) {
        self.settings_tab = tab;
        self.selected = 0;
        self.nickname_edit = None;
        self.binding_edit = None;
        self.feedback = None;
    }
    fn rows(&self) -> usize {
        match self.page {
            Page::Game => 5,
            Page::Settings => self.settings_tab.row_count(),
            Page::Players | Page::ServerInfo => 0,
        }
    }

    pub fn append_typed(&mut self, chars: &[char]) {
        let Some(edit) = self.nickname_edit.as_mut() else { return };
        for &ch in chars {
            if !ch.is_control() && edit.len() + ch.len_utf8() < 16 {
                edit.push(ch);
            }
        }
    }

    pub fn backspace(&mut self) {
        if let Some(edit) = self.nickname_edit.as_mut() { edit.pop(); }
    }

    fn activate(&mut self, settings: &mut ClientSettings, direction: i32)
        -> Option<MenuAction>
    {
        if !self.open { return None; }
        match self.page {
            Page::Game => {
                if direction != 0 { return None; }
                Some(match self.selected {
                    0 => { self.close(); MenuAction::Resume }
                    1 => MenuAction::Spectate,
                    2 => MenuAction::Join,
                    3 => MenuAction::Kill,
                    _ => MenuAction::Disconnect,
                })
            }
            Page::Settings => {
                match (self.settings_tab, self.selected) {
                    (SettingsTab::Player, 0) => {
                        if let Some(nick) = self.nickname_edit.take() {
                            if nick.trim().is_empty() || nick.len() >= 16 {
                                self.feedback("Nickname must use 1-15 UTF-8 bytes");
                                None
                            } else {
                                Some(MenuAction::ChangeNickname(nick))
                            }
                        } else {
                            self.nickname_edit = Some(settings.nickname.clone());
                            None
                        }
                    }
                    (SettingsTab::General, 0) => {
                        settings.show_chat = !settings.show_chat;
                        Some(MenuAction::SettingsChanged)
                    }
                    (SettingsTab::General, 1) => {
                        settings.show_nameplates = !settings.show_nameplates;
                        Some(MenuAction::SettingsChanged)
                    }
                    (SettingsTab::Graphics, 0) => {
                        const LIMITS: [u16; 5] = [30, 60, 120, 144, 240];
                        let index = LIMITS.iter().position(|&v| v == settings.fps_limit)
                            .unwrap_or(1);
                        let next = if direction < 0 {
                            index.saturating_sub(1)
                        } else { (index + 1).min(LIMITS.len()-1) };
                        if next == index { return None; }
                        settings.fps_limit = LIMITS[next];
                        Some(MenuAction::SettingsChanged)
                    }
                    (SettingsTab::Graphics, 1) => {
                        let delta = if direction < 0 { -10 } else { 10 };
                        let value = (i32::from(settings.zoom_percent) + delta).clamp(50, 400);
                        if value as u16 == settings.zoom_percent { return None; }
                        settings.zoom_percent = value as u16;
                        Some(MenuAction::SettingsChanged)
                    }
                    (SettingsTab::Sound, 0) => {
                        settings.sound_enabled = !settings.sound_enabled;
                        Some(MenuAction::SettingsChanged)
                    }
                    (SettingsTab::Controls, row) if row < BIND_ACTIONS.len() => {
                        self.binding_edit = Some(row);
                        self.feedback("Press a key or mouse button; ESC cancels");
                        None
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub fn key(&mut self, key: Key, settings: &mut ClientSettings)
        -> Option<MenuAction>
    {
        if !self.open { return None; }
        if self.is_capturing_binding() {
            if key == Key::Escape {
                self.binding_edit = None;
                self.feedback("Binding change canceled");
                return None;
            }
            let name = format!("{key:?}");
            let index = self.binding_edit.take().unwrap_or(usize::MAX);
            if settings.bindings.set(index, &name) {
                self.feedback(format!("{} -> {}", BIND_ACTIONS[index], name));
                return Some(MenuAction::SettingsChanged);
            }
            self.feedback("Key is reserved or binding unchanged");
            return None;
        }
        if self.is_editing() {
            return match key {
                Key::Escape => { self.nickname_edit = None; None }
                Key::Backspace => { self.backspace(); None }
                Key::Enter => self.activate(settings, 0),
                _ => None,
            };
        }
        match key {
            Key::Escape => { self.close(); Some(MenuAction::Resume) }
            Key::Tab => {
                let index = Page::ALL.iter().position(|&page| page == self.page).unwrap_or(0);
                self.page_select(Page::ALL[(index+1)%Page::ALL.len()]);
                None
            }
            Key::Key1 | Key::Key2 | Key::Key3 | Key::Key4 => {
                let index = match key {
                    Key::Key1 => 0, Key::Key2 => 1, Key::Key3 => 2, _ => 3,
                };
                self.page_select(Page::ALL[index]);
                None
            }
            Key::Up => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            Key::Down => {
                self.selected = (self.selected + 1).min(self.rows().saturating_sub(1));
                None
            }
            Key::Left => {
                if self.page == Page::Settings && self.selected == 0
                    && self.settings_tab.row_count() == 0
                {
                    self.cycle_settings(-1);
                    None
                } else {
                    self.activate(settings, -1)
                }
            }
            Key::Right => {
                if self.page == Page::Settings && self.settings_tab.row_count() == 0 {
                    self.cycle_settings(1);
                    None
                } else {
                    self.activate(settings, 1)
                }
            }
            Key::Enter | Key::Space => self.activate(settings, 0),
            _ => None,
        }
    }

    fn cycle_settings(&mut self, delta: i32) {
        let i = SettingsTab::ALL.iter().position(|&t| t == self.settings_tab).unwrap_or(0);
        let len = SettingsTab::ALL.len() as i32;
        let next = ((i as i32 + delta).rem_euclid(len)) as usize;
        self.select_tab(SettingsTab::ALL[next]);
    }

    pub fn click(
        &mut self, mouse: (f32, f32), viewport: (usize, usize),
        settings: &mut ClientSettings,
    ) -> Option<MenuAction> {
        if !self.open || !mouse.0.is_finite() || !mouse.1.is_finite() { return None; }
        if self.binding_edit.is_some() {
            return self.bind_mouse("MouseLeft", settings);
        }
        if viewport.0 < 520 || viewport.1 < 350 { return None; }
        let lay = Layout::for_size(viewport.0, viewport.1);
        let (mx, my) = (mouse.0, mouse.1);
        let hit = |x: usize, y: usize, w: usize, h: usize| {
            mx >= x as f32 && mx < x.saturating_add(w) as f32
                && my >= y as f32 && my < y.saturating_add(h) as f32
        };
        let available = lay.w.saturating_sub(44);
        let tab_w = available / Page::ALL.len();
        for (idx, page) in Page::ALL.into_iter().enumerate() {
            if hit(lay.tab_origin() + idx*tab_w, lay.y+57, tab_w, 32) {
                self.page_select(page);
                return None;
            }
        }
        if self.page == Page::Settings {
            for (idx, tab) in SettingsTab::ALL.into_iter().enumerate() {
                if hit(lay.category_x(), lay.y+126+idx*32, 148, 29) {
                    self.select_tab(tab);
                    return None;
                }
            }
        }
        let (x, y, w) = if self.page == Page::Settings {
            (lay.content_x(), lay.content_y(), lay.w.saturating_sub(225))
        } else {
            (lay.x+56, lay.content_y(), lay.w.saturating_sub(112))
        };
        let row_height = if self.page == Page::Settings
            && self.settings_tab == SettingsTab::Controls
        { BIND_ROW_HEIGHT } else { ROW_HEIGHT };
        for row in 0..self.rows() {
            if hit(x, y+row*row_height, w, row_height-4) {
                self.selected = row;
                let direction = if self.page == Page::Settings
                    && self.settings_tab == SettingsTab::Graphics
                {
                    if mx < (x+w/2) as f32 { -1 } else { 1 }
                } else { 0 };
                return self.activate(settings, direction);
            }
        }
        None
    }

    pub fn paint<C: HudCanvas>(
        &self, canvas: &mut C, settings: &ClientSettings,
        snapshot: Option<&SnapshotState>, server: ServerStatus<'_>,
    ) {
        if !self.open { return; }
        let (width, height) = canvas.dimensions();
        if width < 520 || height < 350 { return; }
        let l = Layout::for_size(width, height);
        // Preserve the animated world behind the original in-game UI.
        // Both GPU and CPU backends blend these rectangles correctly.
        canvas.fill_translucent_rect(0, 0, width, height, SHADOW, 185);
        rounded_panel(canvas,(l.x,l.y,l.w,l.h),PANEL,240);
        canvas.fill_rect(l.x, l.y, l.w, 48, HEADER);
        canvas.write_text(l.x+24, l.y+19, "DDNET", WHITE, 10);
        canvas.write_text(l.x+l.w.saturating_sub(228), l.y+19,
            "ESC TO RESUME", ACCENT, 24);

        let tab_w = l.w.saturating_sub(44) / Page::ALL.len();
        for (i, page) in Page::ALL.into_iter().enumerate() {
            let x = l.tab_origin()+i*tab_w;
            rounded_panel(canvas,(x,l.y+57,tab_w.saturating_sub(4),32),
                if self.page == page { SELECTED } else { HEADER },255);
            canvas.write_text(x+10, l.y+68, page.title(), WHITE,
                tab_w.saturating_sub(20)/8);
        }
        canvas.fill_rect(l.x+22, l.y+101, l.w.saturating_sub(44), 1, SELECTED);
        match self.page {
            Page::Game => self.paint_game(canvas, l),
            Page::Players => self.paint_players(canvas, l, snapshot),
            Page::ServerInfo => self.paint_server(canvas, l, server),
            Page::Settings => self.paint_settings(canvas, l, settings),
        }
        canvas.fill_rect(l.x+22, l.y+l.h.saturating_sub(48),
            l.w.saturating_sub(44), 1, SELECTED);
        canvas.write_text(l.x+27, l.y+l.h.saturating_sub(33),
            "MOUSE / ARROWS + ENTER   |   TAB NEXT PAGE",
            PALE, l.w.saturating_sub(54)/8);
        if let Some(message) = &self.feedback {
            canvas.write_text(l.x+26, l.y+l.h.saturating_sub(20),
                message, WARNING, l.w.saturating_sub(52)/8);
        }
    }

    fn paint_game<C: HudCanvas>(&self, canvas: &mut C, l: Layout) {
        canvas.write_text(l.x+60, l.content_y()-25, "GAME ACTIONS", ACCENT, 42);
        let buttons = [
            "RESUME GAME", "SPECTATE", "JOIN GAME", "KILL / RESPAWN",
            "DISCONNECT",
        ];
        for (i, label) in buttons.into_iter().enumerate() {
            let y = l.content_y() + i*ROW_HEIGHT;
            rounded_panel(canvas,(l.x+56,y,l.w.saturating_sub(112),ROW_HEIGHT-4),
                if self.selected == i { SELECTED } else { HEADER },255);
            canvas.write_text(l.x+70, y+12, label, WHITE,
                l.w.saturating_sub(140)/8);
        }
    }

    fn paint_server<C: HudCanvas>(&self, canvas: &mut C,
        l: Layout, server: ServerStatus<'_>)
    {
        let x = l.x+55;
        let y = l.content_y();
        for (i, text) in [
            format!("ADDRESS: {}", server.endpoint),
            format!("STATUS: {}", server.stage),
            format!("MAP: {}", server.map.unwrap_or("loading")),
            format!("SERVER TICK: {}", server.tick.map_or("-".to_owned(), |t|t.to_string())),
            format!("ACK: {}", server.ack),
        ].into_iter().enumerate() {
            canvas.write_text(x, y+i*34, &text, WHITE,
                l.w.saturating_sub(95)/8);
        }
    }

    fn paint_players<C: HudCanvas>(
        &self, canvas: &mut C, l: Layout, snapshot: Option<&SnapshotState>,
    ) {
        let Some(snapshot) = snapshot else {
            canvas.write_text(l.x+55, l.content_y(), "WAITING FOR PLAYER DATA",
                WARNING, l.w.saturating_sub(100)/8);
            return;
        };
        let mut players = snapshot.items.iter()
            .filter_map(PlayerInfoNet::from_object).collect::<Vec<_>>();
        players.sort_by(|a,b|b.score.cmp(&a.score).then(a.client_id.cmp(&b.client_id)));
        let x = l.x+45;
        let y = l.content_y();
        canvas.write_text(x, y, "SCORE    PLAYER              PING", ACCENT,
            l.w.saturating_sub(90)/8);
        let max = l.h.saturating_sub(205)/19;
        for (i, player) in players.iter().take(max).enumerate() {
            let name = u16::try_from(player.client_id).ok()
                .and_then(|id|player_name(snapshot,id))
                .unwrap_or_else(|| format!("#{}",player.client_id));
            let label = format!("{:>5}    {:<18} {:>4}",
                player.score, name, player.latency);
            let yy = y+22+i*19;
            if player.local {
                canvas.fill_rect(x.saturating_sub(5), yy.saturating_sub(3),
                    l.w.saturating_sub(85), 17, SELECTED);
            }
            canvas.write_text(x, yy, &label, WHITE, l.w.saturating_sub(90)/8);
        }
        if players.len()>max {
            canvas.write_text(x, y+l.h.saturating_sub(190),
                &format!("{} MORE PLAYERS",players.len()-max),
                WARNING,l.w.saturating_sub(90)/8);
        }
    }

    fn paint_settings<C: HudCanvas>(
        &self, canvas: &mut C, l: Layout, settings: &ClientSettings,
    ) {
        for (i, tab) in SettingsTab::ALL.into_iter().enumerate() {
            let y=l.y+126+i*32;
            rounded_panel(canvas,(l.category_x(),y,148,29),
                if self.settings_tab==tab { SELECTED } else { HEADER },255);
            canvas.write_text(l.category_x()+12, y+10,
                tab.title(), WHITE, 15);
        }
        let x=l.content_x();
        let y=l.content_y();
        let cw=l.w.saturating_sub(225);
        let rows: Vec<String> = match self.settings_tab {
            SettingsTab::General => vec![
                format!("SHOW CHAT: {}",on_off(settings.show_chat)),
                format!("SHOW NAMEPLATES: {}",on_off(settings.show_nameplates)),
            ],
            SettingsTab::Player => vec![format!("NICKNAME: {}{}",
                self.nickname_edit.as_ref().unwrap_or(&settings.nickname),
                if self.is_editing() { "_" } else { "" })],
            SettingsTab::Graphics => vec![
                format!("FPS LIMIT: {}     < / >",settings.fps_limit),
                format!("CAMERA ZOOM: {}%  < / >",settings.zoom_percent),
            ],
            SettingsTab::Sound => vec![
                format!("ENABLE SOUND: {}",on_off(settings.sound_enabled)),
            ],
            SettingsTab::Controls => BIND_ACTIONS.iter().enumerate().map(|(i, action)| {
                let key = if self.binding_edit == Some(i) {
                    "[PRESS A KEY]".to_owned()
                } else {
                    settings.bindings.slots[i].clone()
                };
                format!("{action:<14}  {key}")
            }).collect(),
            SettingsTab::Tee | SettingsTab::Appearance => vec![],
        };
        let row_height = if self.settings_tab == SettingsTab::Controls {
            BIND_ROW_HEIGHT
        } else { ROW_HEIGHT };
        for (i,label) in rows.iter().enumerate() {
            let yy=y+i*row_height;
            rounded_panel(canvas,(x,yy,cw,row_height-4),
                if i==self.selected { SELECTED } else { HEADER },255);
            canvas.write_text(x+12,yy+if row_height==BIND_ROW_HEIGHT { 6 } else { 12 },
                label,WHITE,cw.saturating_sub(22)/8);
        }
        let caption: &str = match self.settings_tab {
            SettingsTab::General => "ENTER OR CLICK TO TOGGLE",
            SettingsTab::Player => "ENTER EDITS; ENTER SAVES; ESC CANCELS",
            SettingsTab::Graphics => "LEFT/RIGHT CHANGES FPS AND ZOOM",
            SettingsTab::Sound => "MUTE APPLIES IMMEDIATELY",
            SettingsTab::Tee => "DEFAULT TEE ATLAS; CUSTOM SKINS NOT YET PORTED",
            SettingsTab::Appearance => "NATIVE DDNET THEME / ASSETS IN PROGRESS",
            SettingsTab::Controls => "ENTER/CLICK TO BIND; ESC CANCELS",
        };
        canvas.write_text(x, y+if self.settings_tab==SettingsTab::Controls { 264 } else { 155 },
            caption, PALE, cw/8);
        if self.settings_tab==SettingsTab::Controls {
            canvas.write_text(x,y+280,
                "MOUSE BUTTONS CAN ALSO BE ASSIGNED",PALE,cw/8);
        }
    }
}
fn on_off(value: bool) -> &'static str {
    if value { "ON" } else { "OFF" }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "gpu")]
    use crate::graphics::Framebuffer;
    #[cfg(feature = "gpu")]
    use crate::gpu_hud::GpuHudScene;

    #[test]
    fn escape_pause_is_reversible_and_does_not_disconnect() {
        let mut m=MenuState::new();
        let mut s=ClientSettings::default();
        assert!(!m.is_open());
        m.toggle();
        assert!(m.is_open());
        assert_eq!(m.key(Key::Escape,&mut s),Some(MenuAction::Resume));
        assert!(!m.is_open());
    }

    #[test]
    fn settings_pages_toggle_runtime_prefs_and_handle_boundaries() {
        let mut m=MenuState::new();m.toggle();m.page_select(Page::Settings);
        let mut s=ClientSettings::default();
        assert_eq!(m.key(Key::Enter,&mut s),Some(MenuAction::SettingsChanged));
        assert!(!s.show_chat);
        m.key(Key::Down,&mut s);
        assert_eq!(m.key(Key::Enter,&mut s),Some(MenuAction::SettingsChanged));
        assert!(!s.show_nameplates);
        m.select_tab(SettingsTab::Graphics);
        for _ in 0..20 {m.key(Key::Right,&mut s);}
        assert_eq!(s.fps_limit,240);
        m.key(Key::Down,&mut s);
        for _ in 0..90 {m.key(Key::Right,&mut s);}
        assert_eq!(s.zoom_percent,400);
    }

    #[test]
    fn nickname_editor_keeps_unicode_valid_and_escape_cancels_edit() {
        let mut m=MenuState::new();m.toggle();m.page_select(Page::Settings);
        m.select_tab(SettingsTab::Player);
        let mut s=ClientSettings::default();
        m.key(Key::Enter,&mut s);
        assert!(m.is_editing());
        for _ in 0..20 {m.backspace();}
        m.append_typed(&['Т','и']);
        assert_eq!(m.key(Key::Enter,&mut s),Some(MenuAction::ChangeNickname("Ти".into())));
        assert!(!m.is_editing());
        m.key(Key::Enter,&mut s);
        m.append_typed(&['z']);
        m.key(Key::Escape,&mut s);
        assert!(m.is_open());
        assert!(!m.is_editing());
    }

    #[test]
    fn mouse_tabs_and_row_clicks_match_render_geometry() {
        let mut m=MenuState::new();m.toggle();
        let mut s=ClientSettings::default();
        let lay=Layout::for_size(960,540);
        m.click(((lay.tab_origin()+3*((lay.w-44)/4)+10) as f32,
            (lay.y+70) as f32),(960,540),&mut s);
        assert_eq!(m.page,Page::Settings);
        m.click(((lay.category_x()+12) as f32,
            (lay.y+126+6*32+12) as f32),(960,540),&mut s);
        assert_eq!(m.settings_tab,SettingsTab::Sound);
        assert_eq!(m.click(((lay.content_x()+20) as f32,
            (lay.content_y()+8) as f32),(960,540),&mut s),
            Some(MenuAction::SettingsChanged));
        assert!(!s.sound_enabled);
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn controls_tab_records_keyboard_and_mouse_rebinds() {
        let mut menu=MenuState::new(); menu.toggle(); menu.page_select(Page::Settings);
        menu.select_tab(SettingsTab::Controls);
        let mut prefs=ClientSettings::default();
        assert_eq!(menu.key(Key::Enter,&mut prefs),None);
        assert!(menu.is_capturing_binding());
        assert_eq!(menu.key(Key::Q,&mut prefs),Some(MenuAction::SettingsChanged));
        assert_eq!(prefs.bindings.slots[0],"Q");
        menu.key(Key::Down,&mut prefs);
        menu.key(Key::Enter,&mut prefs);
        assert!(menu.is_capturing_binding());
        assert_eq!(menu.bind_mouse("MouseRight",&mut prefs),
            Some(MenuAction::SettingsChanged));
        assert_eq!(prefs.bindings.slots[1],"MouseRight");
        assert!(prefs.bindings.validate());
        menu.key(Key::Enter,&mut prefs);
        menu.key(Key::Escape,&mut prefs);
        assert!(!menu.is_capturing_binding());
        assert!(menu.is_open());
    }

    #[test]
    fn ui_paints_on_cpu_and_gpu_without_screen_upload() {
        let mut m=MenuState::new();m.toggle();
        let s=ClientSettings::default();
        let server=ServerStatus {
            endpoint:"127.0.0.1:8303",stage:"EnterGameSent",
            map:Some("ctf1"),tick:Some(123),ack:122,
        };
        let mut cpu=Framebuffer::new(960,540).unwrap();
        cpu.clear(0);
        m.paint(&mut cpu,&s,None,server);
        assert!(cpu.pixels.iter().any(|&v|v!=0));
        let mut gpu=GpuHudScene::new(960,540);
        m.paint(&mut gpu,&s,None,server);
        assert!(!gpu.vertices.is_empty());
        assert!(!gpu.batches.is_empty());
        assert!(gpu.vertices.iter().all(|v|
            v.clip.iter().all(|p|p.is_finite() && (-1.0..=1.0).contains(p))));
    }
}
