//! Pure-Rust, CPU-only debug framebuffer for visual regression and headless
//! snapshot inspection. This is NOT DDNet's complete sprite renderer.
//! It paints tile collision categories, netobject character centers and
//! hook lines in screen space. No OS window or GPU library is required.

use crate::map::GameMap;
use crate::netobjects::CharacterCoreNet;
use crate::physics::Vec2;
use crate::snapshot_delta::SnapshotState;

pub const BACKGROUND: u32 = 0x171e28;
pub const SOLID: u32 = 0x718da5;
pub const NOHOOK: u32 = 0x405a70;
pub const DEATH: u32 = 0xe65e56;
pub const FREEZE: u32 = 0x48abd7;
pub const TEE: u32 = 0xf7d671;
pub const HOOK: u32 = 0xbcc8ce;
const MAX_DIMENSION: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderError {
    InvalidDimensions,
    InvalidCamera,
    FrameTooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub center: Vec2,
    /// Screen pixels per world pixel.
    pub scale: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            center: Vec2::default(),
            scale: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    /// RGB24 values stored as 0x00RRGGBB for easy window/upload integration.
    pub pixels: Vec<u32>,
}

impl Framebuffer {
    pub fn new(width: u32, height: u32) -> Result<Self, RenderError> {
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(RenderError::InvalidDimensions);
        }
        let count = (width as usize).checked_mul(height as usize)
            .ok_or(RenderError::FrameTooLarge)?;
        Ok(Self { width, height, pixels: vec![BACKGROUND; count] })
    }

    pub fn clear(&mut self, color: u32) {
        self.pixels.fill(color);
    }

    fn set(&mut self, x: i64, y: i64, color: u32) {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return;
        }
        self.pixels[y as usize * self.width as usize + x as usize] = color;
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<u32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.pixels.get(y as usize * self.width as usize + x as usize).copied()
    }

    fn screen(&self, world: Vec2, camera: Camera) -> (i64, i64) {
        let x = (world.x - camera.center.x) * camera.scale + self.width as f32 / 2.0;
        let y = (world.y - camera.center.y) * camera.scale + self.height as f32 / 2.0;
        (x.round() as i64, y.round() as i64)
    }

    fn rect_screen(&mut self, ax: i64, ay: i64, bx: i64, by: i64, color: u32) {
        let left = ax.max(0).min(i64::from(self.width));
        let right = bx.max(0).min(i64::from(self.width));
        let top = ay.max(0).min(i64::from(self.height));
        let bottom = by.max(0).min(i64::from(self.height));
        for y in top..bottom {
            let start = y as usize * self.width as usize;
            for x in left..right {
                self.pixels[start + x as usize] = color;
            }
        }
    }

    fn circle_screen(&mut self, x: i64, y: i64, radius: i64, color: u32) {
        if !(0..=4096).contains(&radius) {
            return;
        }
        let min_x = (x - radius).max(0);
        let max_x = (x + radius).min(i64::from(self.width) - 1);
        let min_y = (y - radius).max(0);
        let max_y = (y + radius).min(i64::from(self.height) - 1);
        for cy in min_y..=max_y {
            for cx in min_x..=max_x {
                let dx = cx - x;
                let dy = cy - y;
                if dx * dx + dy * dy <= radius * radius {
                    self.set(cx, cy, color);
                }
            }
        }
    }

    fn line_screen(&mut self, from: (i64, i64), to: (i64, i64), color: u32) {
        // Clip by sampling. Only called with ordinary DDNet hook length,
        // never with an untrusted arbitrarily long world coordinate.
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        let steps = dx.abs().max(dy.abs()).min(8192);
        if steps == 0 {
            self.set(from.0, from.1, color);
            return;
        }
        for i in 0..=steps {
            let x = from.0 + dx * i / steps;
            let y = from.1 + dy * i / steps;
            self.set(x, y, color);
        }
    }

    /// Map tile debug colors, not original graphical image layers.
    pub fn render_map(&mut self, map: &GameMap, camera: Camera) -> Result<(), RenderError> {
        if !camera.scale.is_finite() || !(0.05..=16.0).contains(&camera.scale)
            || !camera.center.x.is_finite() || !camera.center.y.is_finite()
        {
            return Err(RenderError::InvalidCamera);
        }
        self.clear(BACKGROUND);
        let size = 32.0 * camera.scale;
        // The visible screen area limits the number of processed tile cells.
        let left = ((camera.center.x - self.width as f32 / (2.0 * camera.scale))
            / 32.0).floor().max(0.0) as usize;
        let right = ((camera.center.x + self.width as f32 / (2.0 * camera.scale))
            / 32.0).ceil().max(0.0) as usize;
        let top = ((camera.center.y - self.height as f32 / (2.0 * camera.scale))
            / 32.0).floor().max(0.0) as usize;
        let bottom = ((camera.center.y + self.height as f32 / (2.0 * camera.scale))
            / 32.0).ceil().max(0.0) as usize;
        for y in top.min(map.height)..bottom.saturating_add(1).min(map.height) {
            for x in left.min(map.width)..right.saturating_add(1).min(map.width) {
                let Some(tile) = map.tile(x, y) else { continue };
                let color = match tile.index {
                    1 => SOLID,
                    2 => DEATH,
                    3 => NOHOOK,
                    9 | 12 | 144 => FREEZE,
                    _ => continue,
                };
                let (sx, sy) = self.screen(Vec2::new(x as f32 * 32.0,
                    y as f32 * 32.0), camera);
                let extent = size.ceil().max(1.0) as i64;
                self.rect_screen(sx, sy, sx + extent, sy + extent, color);
            }
        }
        Ok(())
    }

    /// Draw character positions and ground/player hook line indicators.
    /// Source coordinates are DDNet netobject integer pixel positions.
    pub fn render_snapshot(
        &mut self, snapshot: &SnapshotState, camera: Camera,
    ) -> Result<usize, RenderError> {
        if !camera.scale.is_finite() || !(0.05..=16.0).contains(&camera.scale) {
            return Err(RenderError::InvalidCamera);
        }
        let mut drawn = 0usize;
        for item in &snapshot.items {
            let Some(character) = CharacterCoreNet::from_object(item) else {
                continue;
            };
            self.draw_character(character, camera);
            drawn += 1;
        }
        Ok(drawn)
    }

    fn draw_character(&mut self, chara: CharacterCoreNet, camera: Camera) {
        let tee = Vec2::new(chara.x as f32, chara.y as f32);
        let center = self.screen(tee, camera);
        if chara.hook_state > 0 {
            let hook_point = Vec2::new(chara.hook_x as f32, chara.hook_y as f32);
            if (hook_point.x - tee.x).abs() < 8192.0
                && (hook_point.y - tee.y).abs() < 8192.0 {
                let hook = self.screen(hook_point, camera);
                self.line_screen(center, hook, HOOK);
            }
        }
        self.circle_screen(center.0, center.1, (11.0 * camera.scale).round() as i64, TEE);
    }

    /// Portable PPM image format for deterministic offline visual tests.
    pub fn ppm_bytes(&self) -> Vec<u8> {
        let mut bytes = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        for color in &self.pixels {
            bytes.push((color >> 16) as u8);
            bytes.push((color >> 8) as u8);
            bytes.push(*color as u8);
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot_delta::SnapshotObject;

    fn tiles() -> GameMap {
        let mut bytes = vec![0u8; 3 * 2 * 4];
        bytes[0] = 1;
        bytes[4] = 9;
        bytes[8] = 3;
        bytes[12] = 2;
        GameMap::from_tile_bytes(3, 2, &bytes).unwrap()
    }

    #[test]
    fn render_tiles_with_centered_camera_and_expected_pixels() {
        let map = tiles();
        let mut frame = Framebuffer::new(96, 64).unwrap();
        let camera = Camera { center: Vec2::new(48.0, 32.0), scale: 1.0 };
        frame.render_map(&map, camera).unwrap();
        assert_eq!(frame.pixel(16,16), Some(SOLID));
        assert_eq!(frame.pixel(48,16), Some(FREEZE));
        assert_eq!(frame.pixel(80,16), Some(NOHOOK));
        assert_eq!(frame.pixel(16,48), Some(DEATH));
        assert_eq!(frame.pixel(48,48), Some(BACKGROUND));
        assert_eq!(frame.ppm_bytes()[..2], *b"P6");
    }

    #[test]
    fn render_character_and_hook_in_front_of_tiles() {
        let map = tiles();
        let mut frame = Framebuffer::new(96, 64).unwrap();
        let camera = Camera { center: Vec2::new(48.0,32.0), scale: 1.0 };
        frame.render_map(&map,camera).unwrap();
        let mut words = vec![0; 22];
        words[1] = 48;
        words[2] = 32;
        words[9] = 5;
        words[11] = 80;
        words[12] = 32;
        let snap = SnapshotState {
            items: vec![SnapshotObject { type_id: 9, id: 1, words }]
        };
        assert_eq!(frame.render_snapshot(&snap,camera), Ok(1));
        assert_eq!(frame.pixel(48,32), Some(TEE));
        assert_eq!(frame.pixel(65,32), Some(HOOK));
        assert_eq!(frame.pixel(80,32), Some(HOOK));
    }

    #[test]
    fn reject_invalid_buffers_or_cameras() {
        assert_eq!(Framebuffer::new(0,1),Err(RenderError::InvalidDimensions));
        assert_eq!(Framebuffer::new(5000,1),Err(RenderError::InvalidDimensions));
        let mut frame = Framebuffer::new(16,16).unwrap();
        assert_eq!(frame.render_map(&tiles(), Camera {
            center: Vec2::default(), scale: f32::NAN
        }), Err(RenderError::InvalidCamera));
        assert_eq!(frame.ppm_bytes().len(), "P6\n16 16\n255\n".len() + 16*16*3);
    }

    #[test]
    fn random_offscreen_coordinates_do_not_panic() {
        let mut frame = Framebuffer::new(32,32).unwrap();
        let mut state = 0x6bad_5eedu32;
        for _ in 0..1000 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let center = Vec2::new(state as i32 as f32,
                (!state) as i32 as f32);
            let camera = Camera { center, scale: 1.0 };
            frame.render_map(&tiles(),camera).unwrap();
        }
    }
}
