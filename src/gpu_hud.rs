//! GPU HUD commands for chat, scoreboard, nameplates and prompts.
//! Original DDNet DejaVu Sans TrueType is rasterized once into a glyph
//! atlas; native GPU scenes batch proportional Unicode glyph rectangles.


use font8x8::{BASIC_FONTS, UnicodeFonts};

use crate::gpu_scene::{GpuBatch, GpuVertex, MAX_FRAME_VERTICES};
use crate::online_ui::HudCanvas;
use crate::sprite_render::{weapon_cursor_sprite, valid_sheet, GRID_GAME};
use crate::texture::Texture;
use crate::ui_font;

pub const HUD_SOLID_SLOT: usize = 0;
pub const HUD_FONT_SLOT: usize = 1;
/// Original game.png, bound after map image slots and the white/skin atlases.
pub const HUD_GAME_CURSOR_SLOT: usize = 2;
pub const FONT_WIDTH: u32 = ui_font::WIDTH;
pub const FONT_HEIGHT: u32 = ui_font::HEIGHT;

/// Atlas of 128 ASCII glyphs, in 16 columns and 8 rows of 8x8 RGBA.
/// Matches the legacy bitmap HUD fallback, including missing-glyph '?'.
pub fn font_atlas_rgba() -> Vec<u8> {
    if let Some(font) = ui_font::font_atlas() {
        return font.rgba.clone();
    }
    // Keep the ASCII atlas when running without original DDNet assets.
    let mut pixels = vec![0u8; (FONT_WIDTH * FONT_HEIGHT * 4) as usize];
    for code in 0..128u32 {
        let character = char::from_u32(code).unwrap_or('?');
        let Some(glyph) = BASIC_FONTS.get(character) else { continue };
        let x0 = (code % 16) * 8;
        let y0 = (code / 16) * 8;
        for (y, row) in glyph.iter().enumerate() {
            for bit in 0..8u32 {
                if row & (1 << bit) != 0 {
                    let pixel = (((y0 + y as u32) * FONT_WIDTH + x0 + bit) * 4) as usize;
                    pixels[pixel..pixel + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
    }
    pixels
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuHudScene {
    pub vertices: Vec<GpuVertex>,
    pub batches: Vec<GpuBatch>,
    width: usize,
    height: usize,
}

impl GpuHudScene {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, ..Self::default() }
    }

    pub fn clear(&mut self) {
        self.vertices.clear();
        self.batches.clear();
    }

    /// Render the original game.png weapon cursor at the local mouse target,
    /// after nameplates/chat and world foreground. The cursor is screen-space
    /// HUD geometry, not a world sprite affected by camera zoom.
    pub fn draw_weapon_cursor(
        &mut self, weapon: i32, position: (f32, f32),
        game: Option<&Texture>,
    ) {
        let Some(sprite) = weapon_cursor_sprite(weapon) else { return; };
        if !position.0.is_finite() || !position.1.is_finite()
            || self.width == 0 || self.height == 0
            || self.vertices.len() > MAX_FRAME_VERTICES - 6
        {
            return;
        }
        if game.is_none_or(|tex| !valid_sheet(tex, GRID_GAME)) {
            let x = position.0.clamp(0.0, (self.width - 1) as f32) as usize;
            let y = position.1.clamp(0.0, (self.height - 1) as f32) as usize;
            self.fill_rect(x.saturating_sub(1), y.saturating_sub(5),
                3, 11, 0xffffff);
            self.fill_rect(x.saturating_sub(5), y.saturating_sub(1),
                11, 3, 0xffffff);
            return;
        }
        let left = (position.0.round() as i32).saturating_sub(16);
        let top = (position.1.round() as i32).saturating_sub(16);
        let right = left.saturating_add(32);
        let bottom = top.saturating_add(32);
        let w = i32::try_from(self.width).unwrap_or(i32::MAX);
        let h = i32::try_from(self.height).unwrap_or(i32::MAX);
        let [x0, y0, x1, y1] = [
            left.clamp(0, w), top.clamp(0, h),
            right.clamp(0, w), bottom.clamp(0, h),
        ];
        if x0 >= x1 || y0 >= y1 { return; }
        let u = |x: i32| {
            (sprite.x as f32
                + (x - left) as f32 / 32.0 * sprite.w as f32)
                / GRID_GAME.0 as f32
        };
        let v = |y: i32| {
            (sprite.y as f32
                + (y - top) as f32 / 32.0 * sprite.h as f32)
                / GRID_GAME.1 as f32
        };
        let corners = [
            (x0, y0, u(x0), v(y0)),
            (x1, y0, u(x1), v(y0)),
            (x0, y1, u(x0), v(y1)),
            (x1, y1, u(x1), v(y1)),
        ];
        let start = self.vertices.len() as u32;
        for index in [0, 1, 2, 1, 3, 2] {
            let (x, y, tx, ty) = corners[index];
            self.vertices.push(GpuVertex {
                clip: [
                    x as f32 * 2.0 / self.width as f32 - 1.0,
                    1.0 - y as f32 * 2.0 / self.height as f32,
                ],
                uv: [tx, ty], color: [255; 4],
            });
        }
        self.batches.push(GpuBatch {
            texture: HUD_GAME_CURSOR_SLOT, start, count: 6, clip: None,
        });
    }

    /// Every painted pixel is a GPU fragment, never CPU raster work.
    fn quad(&mut self, rect: (usize, usize, usize, usize),
        uv: [[f32; 2]; 4], color: u32, slot: usize)
    {
        self.quad_alpha(rect,uv,color,255,slot);
    }

    fn quad_alpha(&mut self, rect: (usize, usize, usize, usize),
        uv: [[f32; 2]; 4], color: u32, alpha: u8, slot: usize)
    {
        let (x, y, w, h) = rect;
        if w == 0 || h == 0 || self.width == 0 || self.height == 0
            || self.vertices.len() > MAX_FRAME_VERTICES - 6
        {
            return;
        }
        let x0 = x.min(self.width) as f32;
        let y0 = y.min(self.height) as f32;
        let x1 = x.saturating_add(w).min(self.width) as f32;
        let y1 = y.saturating_add(h).min(self.height) as f32;
        if x0 >= x1 || y0 >= y1 { return; }
        // UVs must be adjusted when part of a glyph is clipped. The hardware
        // shader also clips to viewport, but keeping geometry bounded is safe.
        let sx = (x1 - x0) / w as f32;
        let sy = (y1 - y0) / h as f32;
        let mut coords = uv;
        for v in [1, 3] {
            coords[v][0] = uv[0][0] + (uv[v][0] - uv[0][0]) * sx;
        }
        for v in [2, 3] {
            coords[v][1] = uv[0][1] + (uv[v][1] - uv[0][1]) * sy;
        }
        let p = [(x0, y0), (x1,y0), (x0,y1), (x1,y1)];
        let tint = [
            ((color >> 16) & 255) as u8,
            ((color >> 8) & 255) as u8,
            (color & 255) as u8,
            alpha,
        ];
        let start = self.vertices.len() as u32;
        for corner in [0, 1, 2, 1, 3, 2] {
            let (sx, sy) = p[corner];
            self.vertices.push(GpuVertex {
                clip: [2.0 * sx / self.width as f32 - 1.0,
                    1.0 - 2.0 * sy / self.height as f32],
                uv: coords[corner], color: tint,
            });
        }
        if let Some(last) = self.batches.last_mut() {
            if last.texture == slot && last.start + last.count == start {
                last.count += 6;
                return;
            }
        }
        self.batches.push(GpuBatch {
            texture: slot, start, count: 6, clip: None,
        });
    }
}

impl HudCanvas for GpuHudScene {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        self.quad((x,y,w,h), [
            [0.0,0.0],[1.0,0.0],[0.0,1.0],[1.0,1.0],
        ], color, HUD_SOLID_SLOT);
    }

    fn fill_translucent_rect(
        &mut self, x: usize, y: usize, w: usize, h: usize, color: u32, alpha: u8,
    ) {
        self.quad_alpha((x,y,w,h), [
            [0.0,0.0],[1.0,0.0],[0.0,1.0],[1.0,1.0],
        ], color, alpha, HUD_SOLID_SLOT);
    }

    fn write_text(&mut self, x: usize, y: usize, value: &str,
        color: u32, max_chars: usize)
    {
        if let Some(atlas) = ui_font::font_atlas() {
            let mut pen = x as f32;
            let baseline = y as f32 + 15.0;
            for ch in value.chars().take(max_chars) {
                let Some(glyph) = ui_font::glyph_at(atlas, ch) else { continue };
                let (gx, gy) = ui_font::glyph_origin(glyph,pen,baseline);
                if gx >= 0 && gy >= 0 && glyph.width > 0 && glyph.height > 0 {
                    let x0 = glyph.x as f32 / FONT_WIDTH as f32;
                    let y0 = glyph.y as f32 / FONT_HEIGHT as f32;
                    let x1 = (glyph.x+glyph.width) as f32 / FONT_WIDTH as f32;
                    let y1 = (glyph.y+glyph.height) as f32 / FONT_HEIGHT as f32;
                    self.quad((gx as usize,gy as usize,
                        glyph.width as usize,glyph.height as usize),
                        [[x0,y0],[x1,y0],[x0,y1],[x1,y1]],
                        color, HUD_FONT_SLOT);
                }
                pen += glyph.advance.max(0.0);
                if pen >= self.width as f32 { break; }
            }
            return;
        }
        for (i, ch) in value.chars().take(max_chars).enumerate() {
            let Some(x) = x.checked_add(i.saturating_mul(8)) else { break };
            if x >= self.width || y >= self.height { break; }
            let glyph = if ch.is_ascii() && BASIC_FONTS.get(ch).is_some() {
                ch as u32
            } else { '?' as u32 };
            if ch == ' ' { continue; }
            let col = glyph % 16;
            let row = glyph / 16;
            let u = (col * 8) as f32 / FONT_WIDTH as f32;
            let v = (row * 8) as f32 / FONT_HEIGHT as f32;
            let step_u = 8.0 / FONT_WIDTH as f32;
            let step_v = 8.0 / FONT_HEIGHT as f32;
            let uv = [
                [u,v],[u + step_u,v],
                [u,v + step_v],[u + step_u,v + step_v],
            ];
            self.quad((x,y,8,8), uv, color, HUD_FONT_SLOT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::online_ui::OnlineHud;
    use crate::graphics::Framebuffer;

    #[test]
    fn original_dejavu_font_atlas_or_ascii_fallback_is_valid() {
        let atlas = font_atlas_rgba();
        assert_eq!(atlas.len(), FONT_WIDTH as usize * FONT_HEIGHT as usize * 4);
        if let Some(font) = ui_font::font_atlas() {
            for ch in ['W','A','П','р'] {
                let glyph=font.glyphs.get(&ch).unwrap();
                assert!(glyph.x+glyph.width<=FONT_WIDTH);
                assert!(glyph.y+glyph.height<=FONT_HEIGHT);
            }
        } else {
            let glyph = BASIC_FONTS.get('A').unwrap();
            let col = ('A' as u32 % 16) * 8;
            let row = ('A' as u32 / 16) * 8;
            for (y, bits) in glyph.iter().enumerate() {
                for bit in 0..8u32 {
                    let index = (((row + y as u32) * FONT_WIDTH + col + bit) * 4 + 3) as usize;
                    assert_eq!(atlas[index], if bits & (1 << bit) != 0 { 255 } else { 0 });
                }
            }
        }
    }

    #[test]
    fn all_six_gpu_cursor_quads_use_game_atlas_and_clip_at_window_edge() {
        let atlas = Texture::from_rgba(320, 160, vec![255; 320 * 160 * 4])
            .unwrap();
        let mut hud = GpuHudScene::new(64, 64);
        for (weapon, row) in [
            (0, 0.0), (1, 4.0), (2, 6.0),
            (3, 8.0), (4, 12.0), (5, 10.0),
        ] {
            hud.clear();
            hud.draw_weapon_cursor(weapon, (32.0, 32.0), Some(&atlas));
            assert_eq!(hud.vertices.len(), 6);
            assert_eq!(hud.batches[0].texture, HUD_GAME_CURSOR_SLOT);
            assert_eq!(hud.vertices[0].uv[0], 0.0);
            assert_eq!(hud.vertices[0].uv[1], row / 16.0);
        }
        hud.clear();
        hud.draw_weapon_cursor(1, (2.0, 3.0), Some(&atlas));
        assert_eq!(hud.vertices.len(), 6);
        assert!(hud.vertices.iter().all(|v|
            v.clip.iter().all(|&n| (-1.0..=1.0).contains(&n))
        ));
        assert!(hud.vertices[0].uv[0] > 0.0,
            "left-clipped cursor must shift its source UVs");
        hud.clear();
        hud.draw_weapon_cursor(1, (f32::NAN, 0.0), Some(&atlas));
        assert!(hud.vertices.is_empty());
        hud.draw_weapon_cursor(1, (32.0, 32.0), None);
        assert!(!hud.vertices.is_empty(), "missing atlas uses visible fallback");
        assert!(hud.batches.iter().all(|b| b.texture == HUD_SOLID_SLOT));
    }

    #[test]
    fn gpu_hud_batches_panels_and_text_in_paint_order() {
        let mut hud = OnlineHud::new();
        hud.open_chat(false);
        hud.append_typed(&['t','e','s','t']);
        hud.chat_line("[1] ", "hello");
        let mut gpu = GpuHudScene::new(320,180);
        hud.render(&mut gpu, "Online", None, Some(3), None);
        assert!(!gpu.vertices.is_empty());
        assert!(gpu.batches.len() >= 4);
        assert_eq!(gpu.batches[0].texture, HUD_SOLID_SLOT);
        assert_eq!(gpu.batches[1].texture, HUD_FONT_SLOT);
        assert!(gpu.vertices.iter().all(|v|
            v.clip.iter().all(|c| c.is_finite() && (-1.0..=1.0).contains(c))));
        let mut software = Framebuffer::new(320,180).unwrap();
        software.clear(0);
        hud.render(&mut software, "Online", None, Some(3), None);
        assert!(software.pixels.iter().any(|&color| color != 0));
        gpu.clear();
        assert!(gpu.vertices.is_empty() && gpu.batches.is_empty());
    }

    #[test]
    fn partial_viewport_glyph_is_clipped_and_bounded() {
        let mut scene = GpuHudScene::new(16,16);
        scene.write_text(12, 0, "A", 0xff0000, 1);
        assert_eq!(scene.vertices.len(), 6);
        assert!(scene.vertices.iter().all(|v| v.clip.iter().all(|&p|(-1.0..=1.0).contains(&p))));
        scene.fill_rect(usize::MAX, usize::MAX, 20,20,0xffffff);
        assert_eq!(scene.vertices.len(),6);
    }
}
