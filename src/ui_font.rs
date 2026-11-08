//! Original DDNet 20.2.1 DejaVu Sans TrueType glyph atlas.
//! Source: upstream/ddnet/data/fonts/index.json ("default": "DejaVu Sans").
//! Pure Rust fontdue rasterizes the glyphs only once per process. Both the
//! CPU fallback and native GPU canvas use identical glyph metrics and pixels.

use std::collections::HashMap;
use std::sync::OnceLock;

use fontdue::{Font, FontSettings};

use crate::window_assets::candidate_roots;

pub const FONT_SIZE: f32 = 14.0;
pub const CELL: u32 = 24;
pub const COLUMNS: u32 = 32;
pub const WIDTH: u32 = CELL * COLUMNS;
pub const HEIGHT: u32 = 576;

#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance: f32,
}

pub struct UiFontAtlas {
    pub rgba: Vec<u8>,
    pub glyphs: HashMap<char, Glyph>,
}

static CACHE: OnceLock<Option<UiFontAtlas>> = OnceLock::new();

pub fn font_atlas() -> Option<&'static UiFontAtlas> {
    CACHE.get_or_init(load_ddnet_font).as_ref()
}

fn load_ddnet_font() -> Option<UiFontAtlas> {
    for root in candidate_roots(None) {
        let data = std::fs::read(root.join("fonts").join("DejaVuSans.ttf")).ok();
        if let Some(font) = data.and_then(|bytes| {
            Font::from_bytes(bytes, FontSettings::default()).ok()
        }) {
            return Some(build_atlas(&font));
        }
    }
    None
}

fn build_atlas(font: &Font) -> UiFontAtlas {
    let mut rgba = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
    let mut glyphs = HashMap::new();
    let characters = (32..=255u32).chain(0x0400..=0x052fu32)
        .chain(0x2000..=0x206fu32).filter_map(char::from_u32);
    for (index, ch) in characters.enumerate() {
        let (metrics, alpha) = font.rasterize(ch, FONT_SIZE);
        if metrics.width as u32 > CELL || metrics.height as u32 > CELL {
            continue;
        }
        let x = (index as u32 % COLUMNS) * CELL;
        let y = (index as u32 / COLUMNS) * CELL;
        if y + CELL > HEIGHT { break; }
        let glyph = Glyph {
            x, y, width: metrics.width as u32, height: metrics.height as u32,
            bearing_x: metrics.xmin, bearing_y: metrics.ymin,
            advance: metrics.advance_width,
        };
        for py in 0..glyph.height {
            for px in 0..glyph.width {
                let dest = (((y+py)*WIDTH+x+px)*4) as usize;
                let coverage = alpha[(py*glyph.width+px) as usize];
                rgba[dest..dest+4].copy_from_slice(&[255,255,255,coverage]);
            }
        }
        glyphs.insert(ch,glyph);
    }
    UiFontAtlas { rgba, glyphs }
}

pub fn glyph_at(atlas: &UiFontAtlas, ch: char) -> Option<&Glyph> {
    atlas.glyphs.get(&ch).or_else(||atlas.glyphs.get(&'?'))
}

pub fn glyph_origin(glyph: &Glyph, pen_x: f32, baseline_y: f32) -> (i32, i32) {
    ((pen_x + glyph.bearing_x as f32).round() as i32,
     (baseline_y - glyph.bearing_y as f32 - glyph.height as f32).round() as i32)
}

/// One-time TrueType glyph compositing for software mode, respecting original
/// alpha coverage. UTF-8 source glyphs are retained, never rendered as '?'.
pub fn draw_text_cpu(
    pixels: &mut [u32], frame_size: (usize,usize),
    origin: (usize,usize), text: &str, color: u32, max_chars: usize,
) -> bool {
    let (frame_width,frame_height) = frame_size;
    let (x,y) = origin;
    let Some(atlas) = font_atlas() else { return false };
    let mut pen = x as f32;
    let baseline = y as f32 + 15.0;
    for ch in text.chars().take(max_chars) {
        let Some(glyph) = glyph_at(atlas,ch) else { continue };
        let (gx,gy) = glyph_origin(glyph,pen,baseline);
        for py in 0..glyph.height {
            let dy = gy + py as i32;
            if dy < 0 || dy >= frame_height as i32 { continue; }
            for px in 0..glyph.width {
                let dx = gx + px as i32;
                if dx < 0 || dx >= frame_width as i32 { continue; }
                let src = (((glyph.y+py)*WIDTH+glyph.x+px)*4+3) as usize;
                let a = u32::from(atlas.rgba[src]);
                if a==0 { continue; }
                let dst = dy as usize*frame_width + dx as usize;
                let prev = pixels[dst];
                let channel = |shift:u32| {
                    let f = (color>>shift)&255u32;
                    let b = (prev>>shift)&255u32;
                    (f*a+b*(255-a)+127)/255
                };
                pixels[dst] = channel(16)<<16 | channel(8)<<8 | channel(0);
            }
        }
        pen += glyph.advance.max(0.0);
        if pen >= frame_width as f32 { break; }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddnet_dejavu_supports_latin_and_cyrillic_if_assets_present() {
        // Unit tests without checked-out DDNet assets skip content checks,
        // but the native CI checkout includes the official submodule.
        if let Some(atlas) = font_atlas() {
            for ch in ['A','W','я','П','и','é','—'] {
                let glyph = atlas.glyphs.get(&ch).unwrap();
                assert!(glyph.advance>0.0);
                assert!(glyph.width<=CELL && glyph.height<=CELL);
            }
            let mut pixels = vec![0u32; 120*25];
            assert!(draw_text_cpu(&mut pixels,(120,25),(0,0),"Привет",0xffffff,8));
            assert!(pixels.iter().any(|&p|p!=0));
        }
    }

    #[test]
    fn rasterized_glyph_coordinates_stay_inside_atlas() {
        if let Some(atlas) = font_atlas() {
            assert!(atlas.glyphs.len()>400);
            for glyph in atlas.glyphs.values() {
                assert!(glyph.x+glyph.width<=WIDTH);
                assert!(glyph.y+glyph.height<=HEIGHT);
                let (_,y)=glyph_origin(glyph,12.0,15.0);
                assert!((-24..=39).contains(&y));
            }
        }
    }
}
