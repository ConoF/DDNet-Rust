//! GPU HUD commands for chat, scoreboard, nameplates and prompts.
//! A 128-glyph bitmap atlas is uploaded once, not a full-screen RGB buffer
//! every frame. This preserves the existing HUD layout, although a proper
//! DDNet scalable Unicode font still needs to replace Basic Latin.

use font8x8::{BASIC_FONTS, UnicodeFonts};

use crate::gpu_scene::{GpuBatch, GpuVertex, MAX_FRAME_VERTICES};
use crate::online_ui::HudCanvas;

pub const HUD_SOLID_SLOT: usize = 0;
pub const HUD_FONT_SLOT: usize = 1;
pub const FONT_WIDTH: u32 = 128;
pub const FONT_HEIGHT: u32 = 64;

/// Atlas of 128 ASCII glyphs, in 16 columns and 8 rows of 8x8 RGBA.
/// Matches the legacy bitmap HUD fallback, including missing-glyph '?'.
pub fn font_atlas_rgba() -> Vec<u8> {
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

    /// Every painted pixel is a GPU fragment, never CPU raster work.
    fn quad(&mut self, rect: (usize, usize, usize, usize),
        uv: [[f32; 2]; 4], color: u32, slot: usize)
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
            255,
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
            texture: slot, start, count: 6,
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

    fn write_text(&mut self, x: usize, y: usize, value: &str,
        color: u32, max_chars: usize)
    {
        for (i, ch) in value.chars().take(max_chars).enumerate() {
            let Some(x) = x.checked_add(i.saturating_mul(8)) else { break };
            if x >= self.width || y >= self.height { break; }
            let glyph = if ch.is_ascii() && BASIC_FONTS.get(ch).is_some() {
                ch as u32
            } else { '?' as u32 };
            if ch == ' ' { continue; }
            let col = glyph % 16;
            let row = glyph / 16;
            let u = col as f32 / 16.0;
            let v = row as f32 / 8.0;
            let uv = [
                [u,v],[u + 1.0/16.0,v],
                [u,v + 1.0/8.0],[u + 1.0/16.0,v + 1.0/8.0],
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
    fn ascii_glyph_atlas_matches_pixel_font() {
        let atlas = font_atlas_rgba();
        assert_eq!(atlas.len(), FONT_WIDTH as usize * FONT_HEIGHT as usize * 4);
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
        scene.write_text(12, 14, "A", 0xff0000, 1);
        assert_eq!(scene.vertices.len(), 6);
        assert!(scene.vertices.iter().all(|v| v.clip.iter().all(|&p|(-1.0..=1.0).contains(&p))));
        scene.fill_rect(usize::MAX, usize::MAX, 20,20,0xffffff);
        assert_eq!(scene.vertices.len(),6);
    }
}
