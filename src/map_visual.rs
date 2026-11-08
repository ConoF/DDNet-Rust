//! Preview DDNet map tile images using external PNGs and embedded RGBA data.
//! This is a CPU-only first step toward the original map renderer.
//! Quad layers, envelope animations and GPU batching remain unimplemented.

use std::path::Path;

use crate::datafile::{Datafile, DatafileError};
use crate::graphics::{Camera, Framebuffer, RenderError, BACKGROUND};
use crate::map::Tile;
use crate::texture::{Texture, TextureError};

const MAP_IMAGE: u16 = 2;
const MAP_GROUP: u16 = 4;
const MAP_LAYER: u16 = 5;
const TILEMAP: i32 = 2;
const MAX_IMAGES: usize = 64;
const MAX_LAYERS: usize = 1024;
const MAX_LAYER_CELLS: usize = 2_000_000;
const MAX_TOTAL_CELLS: usize = 8_000_000;
const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualError {
    InvalidMap,
    InvalidImage,
    InvalidAssetName,
    MissingImage,
    TooLarge,
    Io,
    Datafile(DatafileError),
    Texture(TextureError),
    Render(RenderError),
}
impl From<DatafileError> for VisualError {
    fn from(value: DatafileError) -> Self { Self::Datafile(value) }
}
impl From<TextureError> for VisualError {
    fn from(value: TextureError) -> Self { Self::Texture(value) }
}
impl From<RenderError> for VisualError {
    fn from(value: RenderError) -> Self { Self::Render(value) }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroupTransform {
    pub offset_x: f32,
    pub offset_y: f32,
    pub parallax_x: f32,
    pub parallax_y: f32,
}
impl Default for GroupTransform {
    fn default() -> Self {
        Self { offset_x: 0.0, offset_y: 0.0, parallax_x: 100.0, parallax_y: 100.0 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TexturedLayer {
    pub width: usize,
    pub height: usize,
    pub image: usize,
    pub tint: [u8; 4],
    pub transform: GroupTransform,
    pub tiles: Vec<Tile>,
    /// Layers after the game layer render in front of players, like upstream.
    pub foreground: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VisualMap {
    pub images: Vec<Texture>,
    pub layers: Vec<TexturedLayer>,
}

fn field(payload: &[u8], at: usize) -> Result<i32, VisualError> {
    let slice: [u8; 4] = payload.get(at..at + 4)
        .ok_or(VisualError::InvalidMap)?
        .try_into().map_err(|_| VisualError::InvalidMap)?;
    Ok(i32::from_le_bytes(slice))
}

fn positive(value: i32) -> Result<usize, VisualError> {
    let number = usize::try_from(value).map_err(|_| VisualError::InvalidMap)?;
    if number == 0 { Err(VisualError::InvalidMap) } else { Ok(number) }
}

fn index(value: i32) -> Result<usize, VisualError> {
    usize::try_from(value).map_err(|_| VisualError::InvalidMap)
}

fn external_name(file: &Datafile<'_>, index_id: i32) -> Result<String, VisualError> {
    let bytes = file.raw_data(index(index_id)?, 256)?;
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let name = std::str::from_utf8(&bytes[..end])
        .map_err(|_| VisualError::InvalidAssetName)?;
    if name.is_empty() || name.len() > 120
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(VisualError::InvalidAssetName);
    }
    Ok(name.to_owned())
}

fn load_image(
    file: &Datafile<'_>,
    payload: &[u8],
    assets: &Path,
) -> Result<Texture, VisualError> {
    if payload.len() < 24 { return Err(VisualError::InvalidImage); }
    let version = field(payload, 0)?;
    if !(1..=2).contains(&version)
        || (version == 2 && field(payload, 24)? != 1)
    {
        return Err(VisualError::InvalidImage);
    }
    let width = u32::try_from(field(payload, 4)?)
        .map_err(|_| VisualError::InvalidImage)?;
    let height = u32::try_from(field(payload, 8)?)
        .map_err(|_| VisualError::InvalidImage)?;
    let name = external_name(file, field(payload, 16)?)?;
    match field(payload, 12)? {
        0 => {
            let size = (width as usize).checked_mul(height as usize)
                .and_then(|n| n.checked_mul(4))
                .ok_or(VisualError::TooLarge)?;
            if width == 0 || height == 0 || width > 4096 || height > 4096 {
                return Err(VisualError::TooLarge);
            }
            let rgba = file.raw_data(index(field(payload, 20)?)?, size)?;
            Ok(Texture::from_rgba(width, height, rgba)?)
        }
        1 => {
            let path = assets.join("mapres").join(format!("{name}.png"));
            let metadata = std::fs::metadata(&path).map_err(|_| VisualError::Io)?;
            if metadata.len() > MAX_ASSET_BYTES { return Err(VisualError::TooLarge); }
            let bytes = std::fs::read(path).map_err(|_| VisualError::Io)?;
            // Exactly as in DDNet 20.2.1 mapimages.cpp: external images
            // use the PNG's dimensions, not m_Width/m_Height from the map.
            // Legacy maps can set those metadata fields to 0 or stale sizes.
            Ok(Texture::from_png(&bytes)?)
        }
        _ => Err(VisualError::InvalidImage),
    }
}

fn collect_groups(file: &Datafile<'_>, count: usize) -> Result<Vec<GroupTransform>, VisualError> {
    let mut result = vec![GroupTransform::default(); count];
    for item in file.items.iter().filter(|item| item.type_id == MAP_GROUP) {
        if item.payload.len() < 28 { return Err(VisualError::InvalidMap); }
        let first = index(field(item.payload, 20)?)?;
        let amount = index(field(item.payload, 24)?)?;
        let end = first.checked_add(amount).ok_or(VisualError::TooLarge)?;
        if end > count { return Err(VisualError::InvalidMap); }
        let transform = GroupTransform {
            offset_x: field(item.payload, 4)? as f32,
            offset_y: field(item.payload, 8)? as f32,
            parallax_x: field(item.payload, 12)? as f32,
            parallax_y: field(item.payload, 16)? as f32,
        };
        result[first..end].fill(transform);
    }
    Ok(result)
}

impl VisualMap {
    /// Load image resources and ordinary tiles layers in original layer order.
    /// The root directory is the DDNet \`data/\` directory (not the mapres
    /// subdirectory). Malformed paths cannot escape \`mapres\`.
    pub fn from_datafile(
        file: &Datafile<'_>, asset_root: &Path,
    ) -> Result<Self, VisualError> {
        let image_items: Vec<_> = file.items.iter()
            .filter(|item| item.type_id == MAP_IMAGE).collect();
        if image_items.len() > MAX_IMAGES { return Err(VisualError::TooLarge); }
        let mut images = Vec::with_capacity(image_items.len());
        for item in image_items {
            images.push(load_image(file, item.payload, asset_root)?);
        }
        let layer_items: Vec<_> = file.items.iter()
            .filter(|item| item.type_id == MAP_LAYER).collect();
        if layer_items.len() > MAX_LAYERS { return Err(VisualError::TooLarge); }
        let groups = collect_groups(file, layer_items.len())?;
        let mut layers = Vec::new();
        let mut total = 0usize;
        let game_layer_index = layer_items.iter().position(|item| {
            field(item.payload, 4).ok() == Some(TILEMAP)
                && field(item.payload, 24).ok()
                    .is_some_and(|flags| flags & 1 != 0)
        });
        for (ordinal, item) in layer_items.iter().enumerate() {
            if field(item.payload, 4)? != TILEMAP { continue; }
            if item.payload.len() < 60 { return Err(VisualError::InvalidMap); }
            // Flags mark physics layers (game/front/switch/tele/...).
            // Only ordinary image tile layers are rendered at this stage.
            if field(item.payload, 24)? != 0 { continue; }
            let image_index = field(item.payload, 52)?;
            if image_index < 0 { continue; }
            let image_index = index(image_index)?;
            if image_index >= images.len() { return Err(VisualError::MissingImage); }
            let width = positive(field(item.payload, 16)?)?;
            let height = positive(field(item.payload, 20)?)?;
            let count = width.checked_mul(height).ok_or(VisualError::TooLarge)?;
            total = total.checked_add(count).ok_or(VisualError::TooLarge)?;
            if count > MAX_LAYER_CELLS || total > MAX_TOTAL_CELLS {
                return Err(VisualError::TooLarge);
            }
            let bytes = file.raw_data(index(field(item.payload, 56)?)?, count * 4)?;
            if bytes.len() != count * 4 { return Err(VisualError::InvalidMap); }
            let tiles = bytes.chunks_exact(4).map(|r| Tile {
                index: r[0], flags: r[1], skip: r[2], reserved: r[3],
            }).collect();
            let mut tint = [255; 4];
            for (i, color) in tint.iter_mut().enumerate() {
                *color = field(item.payload, 28 + i * 4)?.clamp(0, 255) as u8;
            }
            layers.push(TexturedLayer {
                width, height, image: image_index,
                tint, transform: groups[ordinal], tiles,
                foreground: game_layer_index.is_some_and(|game_index| ordinal > game_index),
            });
        }
        Ok(Self { images, layers })
    }

    /// Keep the legacy one-call map rendering API for previews and tests.
    /// Interactive game windows use two passes so foreground tiles cover tees.
    pub fn render(
        &self, frame: &mut Framebuffer, camera: Camera,
    ) -> Result<usize, VisualError> {
        let background_count = self.render_background(frame, camera)?;
        let foreground_count = self.render_foreground(frame, camera)?;
        Ok(background_count + foreground_count)
    }

    pub fn render_background(
        &self, frame: &mut Framebuffer, camera: Camera,
    ) -> Result<usize, VisualError> {
        self.render_pass(frame, camera, false, true)
    }

    pub fn render_foreground(
        &self, frame: &mut Framebuffer, camera: Camera,
    ) -> Result<usize, VisualError> {
        self.render_pass(frame, camera, true, false)
    }

    fn render_pass(
        &self, frame: &mut Framebuffer, camera: Camera,
        foreground: bool, clear: bool,
    ) -> Result<usize, VisualError> {
        if !(0.05..=16.0).contains(&camera.scale) || !camera.scale.is_finite()
            || !camera.center.x.is_finite() || !camera.center.y.is_finite()
        {
            return Err(VisualError::Render(RenderError::InvalidCamera));
        }
        if clear { frame.clear(BACKGROUND); }
        let mut drawn = 0usize;
        for layer in &self.layers {
            if layer.foreground != foreground { continue; }
            let texture = self.images.get(layer.image).ok_or(VisualError::MissingImage)?;
            let cell_w = texture.width as usize / 16;
            let cell_h = texture.height as usize / 16;
            if cell_w == 0 || cell_h == 0
                || texture.width % 16 != 0 || texture.height % 16 != 0
                || layer.width.checked_mul(layer.height) != Some(layer.tiles.len())
            {
                return Err(VisualError::InvalidImage);
            }
            let cx = camera.center.x * layer.transform.parallax_x / 100.0
                - layer.transform.offset_x;
            let cy = camera.center.y * layer.transform.parallax_y / 100.0
                - layer.transform.offset_y;
            let visible_x = frame.width as f32 / (2.0 * camera.scale);
            let visible_y = frame.height as f32 / (2.0 * camera.scale);
            let x0 = ((cx - visible_x) / 32.0).floor().max(0.0) as usize;
            let y0 = ((cy - visible_y) / 32.0).floor().max(0.0) as usize;
            let x1 = ((cx + visible_x) / 32.0).ceil().max(0.0) as usize;
            let y1 = ((cy + visible_y) / 32.0).ceil().max(0.0) as usize;
            for y in y0.min(layer.height)..y1.saturating_add(1).min(layer.height) {
                for x in x0.min(layer.width)..x1.saturating_add(1).min(layer.width) {
                    let tile = layer.tiles[y * layer.width + x];
                    if tile.index == 0 { continue; }
                    let start_x = ((x as f32 * 32.0 - cx) * camera.scale
                        + frame.width as f32 * 0.5).round() as i64;
                    let start_y = ((y as f32 * 32.0 - cy) * camera.scale
                        + frame.height as f32 * 0.5).round() as i64;
                    let end_x = (((x + 1) as f32 * 32.0 - cx) * camera.scale
                        + frame.width as f32 * 0.5).round() as i64;
                    let end_y = (((y + 1) as f32 * 32.0 - cy) * camera.scale
                        + frame.height as f32 * 0.5).round() as i64;
                    if end_x <= start_x || end_y <= start_y { continue; }
                    blit_tile(frame, texture, tile, layer.tint,
                        (start_x, start_y, end_x, end_y), (cell_w, cell_h));
                    drawn += 1;
                }
            }
        }
        Ok(drawn)
    }
}

fn blit_tile(
    frame: &mut Framebuffer,
    texture: &Texture,
    tile: Tile,
    tint: [u8; 4],
    bounds: (i64, i64, i64, i64),
    cell: (usize, usize),
) {
    let (left, top, right, bottom) = bounds;
    let src_x = usize::from(tile.index % 16) * cell.0;
    let src_y = usize::from(tile.index / 16) * cell.1;
    let dst_width = (right - left) as f32;
    let dst_height = (bottom - top) as f32;
    let clipped_left = left.clamp(0, i64::from(frame.width));
    let clipped_top = top.clamp(0, i64::from(frame.height));
    let clipped_right = right.clamp(0, i64::from(frame.width));
    let clipped_bottom = bottom.clamp(0, i64::from(frame.height));
    for sy in clipped_top..clipped_bottom {
        for sx in clipped_left..clipped_right {
            let mut u = ((sx - left) as f32 + 0.5) / dst_width;
            let mut v = ((sy - top) as f32 + 0.5) / dst_height;
            if tile.flags & 8 != 0 {
                (u, v) = (v, 1.0 - u);
            }
            if tile.flags & 1 != 0 { u = 1.0 - u; }
            if tile.flags & 2 != 0 { v = 1.0 - v; }
            let ix = src_x + ((u * cell.0 as f32) as usize).min(cell.0 - 1);
            let iy = src_y + ((v * cell.1 as f32) as usize).min(cell.1 - 1);
            let src = (iy * texture.width as usize + ix) * 4;
            let alpha = u32::from(texture.rgba[src + 3]) * u32::from(tint[3]) / 255;
            if alpha == 0 { continue; }
            let at = sy as usize * frame.width as usize + sx as usize;
            // Opaque pixels never need a destination read or alpha blending.
            // Most terrain tiles on ordinary DDNet maps take this path.
            if alpha == 255 {
                frame.pixels[at] =
                    (u32::from(texture.rgba[src]) * u32::from(tint[0]) / 255 << 16)
                    | (u32::from(texture.rgba[src + 1]) * u32::from(tint[1]) / 255 << 8)
                    | (u32::from(texture.rgba[src + 2]) * u32::from(tint[2]) / 255);
                continue;
            }
            let dst = frame.pixels[at];
            let back = [
                (dst >> 16) & 255,
                (dst >> 8) & 255,
                dst & 255,
            ];
            let mut result = 0u32;
            for (i, channel) in back.iter().enumerate() {
                let foreground = u32::from(texture.rgba[src + i]) * u32::from(tint[i]) / 255;
                let value = (foreground * alpha + channel * (255 - alpha) + 127) / 255;
                result = (result << 8) | value;
            }
            frame.pixels[at] = result;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datafile::{Item, RawBlock};
    use crate::physics::Vec2;

    fn words(values: &[i32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn make_fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let img_item = words(&[1, 16, 16, 0, 0, 1]);
        let layer = words(&[
            0, 2, 0, 2, 2, 1, 0,
            255, 255, 255, 255, -1, 0, 0, 2,
        ]);
        let mut atlas = vec![0u8; 16 * 16 * 4];
        atlas[4..8].copy_from_slice(&[255, 0, 0, 255]);
        atlas[8..12].copy_from_slice(&[0, 255, 0, 128]);
        let tile_data = vec![1, 0, 0, 0, 2, 0, 0, 0];
        (img_item, layer, atlas, tile_data)
    }

    #[test]
    fn renders_real_rgba_atlas_tiles_with_alpha_blending() {
        let (image_payload, layer_payload, atlas, tiles) = make_fixture();
        let name = b"synthetic\0";
        let file = Datafile {
            version: 3, item_types: vec![],
            items: vec![
                Item { type_id: MAP_IMAGE, id: 0, payload: &image_payload },
                Item { type_id: MAP_LAYER, id: 0, payload: &layer_payload },
            ],
            raw_blocks: vec![
                RawBlock { encoded: name, uncompressed_size: None },
                RawBlock { encoded: &atlas, uncompressed_size: None },
                RawBlock { encoded: &tiles, uncompressed_size: None },
            ],
        };
        let map = VisualMap::from_datafile(&file, Path::new(".")).unwrap();
        assert_eq!(map.layers.len(), 1);
        let mut frame = Framebuffer::new(64, 32).unwrap();
        let count = map.render(&mut frame, Camera {
            center: Vec2::new(32.0, 16.0), scale: 1.0,
        }).unwrap();
        assert_eq!(count, 2);
        assert_eq!(frame.pixel(16, 16), Some(0xff0000));
        let blended = frame.pixel(48, 16).unwrap();
        assert_ne!(blended, BACKGROUND);
        assert!((blended >> 8 & 255) > (blended >> 16 & 255));
    }

    #[test]
    fn game_layer_splits_background_and_foreground_renders() {
        let atlas = vec![255u8; 16 * 16 * 4];
        let image = Texture::from_rgba(16, 16, atlas).unwrap();
        let layer = |foreground: bool| TexturedLayer {
            width: 1, height: 1, image: 0,
            tint: if foreground { [0, 0, 255, 255] } else { [255, 0, 0, 255] },
            transform: GroupTransform::default(),
            tiles: vec![Tile { index: 1, flags: 0, skip: 0, reserved: 0 }],
            foreground,
        };
        let map = VisualMap { images: vec![image], layers: vec![layer(false), layer(true)] };
        let camera = Camera { center: Vec2::new(16.0,16.0), scale: 1.0 };
        let mut frame = Framebuffer::new(32, 32).unwrap();
        assert_eq!(map.render_background(&mut frame, camera).unwrap(), 1);
        assert_eq!(frame.pixel(16, 16), Some(0xff0000));
        // A tee-like visual between passes is behind the foreground.
        frame.pixels[16 * 32 + 16] = 0x00ff00;
        assert_eq!(map.render_foreground(&mut frame, camera).unwrap(), 1);
        assert_eq!(frame.pixel(16, 16), Some(0x0000ff));
        assert_eq!(map.render(&mut frame, camera).unwrap(), 2);
        assert_eq!(frame.pixel(16, 16), Some(0x0000ff));
    }

    #[test]
    fn rejects_invalid_image_index_and_unsafe_asset_names() {
        let (image_payload, mut layer_payload, atlas, tiles) = make_fixture();
        layer_payload[52..56].copy_from_slice(&3i32.to_le_bytes());
        let file = Datafile {
            version: 3, item_types: vec![],
            items: vec![
                Item { type_id: MAP_IMAGE, id: 0, payload: &image_payload },
                Item { type_id: MAP_LAYER, id: 0, payload: &layer_payload },
            ],
            raw_blocks: vec![
                RawBlock { encoded: b"synthetic\0", uncompressed_size: None },
                RawBlock { encoded: &atlas, uncompressed_size: None },
                RawBlock { encoded: &tiles, uncompressed_size: None },
            ],
        };
        assert_eq!(
            VisualMap::from_datafile(&file, Path::new(".")),
            Err(VisualError::MissingImage),
        );
        let forbidden = b"../escape\0";
        let modified = Datafile {
            raw_blocks: vec![
                RawBlock { encoded: forbidden, uncompressed_size: None },
                RawBlock { encoded: &atlas, uncompressed_size: None },
                RawBlock { encoded: &tiles, uncompressed_size: None },
            ],
            ..file
        };
        assert_eq!(
            VisualMap::from_datafile(&modified, Path::new(".")),
            Err(VisualError::InvalidAssetName),
        );
    }

    #[test]
    fn external_image_dimensions_come_from_png_not_stale_map_metadata() {
        // DDNet's mapimages.cpp does not inspect map dimensions for
        // external resources; only embedded RGBA images use them.
        let root = std::env::temp_dir().join(format!(
            "ddnet-rust-mapres-dim-test-{}", std::process::id(),
        ));
        let mapres = root.join("mapres");
        std::fs::create_dir_all(&mapres).unwrap();
        let image = crate::graphics::Framebuffer::new(2, 2).unwrap();
        std::fs::write(mapres.join("synthetic.png"), image.png_bytes().unwrap()).unwrap();
        let image_payload = words(&[1, 0, 0, 1, 0, -1]);
        let file = Datafile {
            version: 4, item_types: vec![],
            items: vec![Item {
                type_id: MAP_IMAGE, id: 0, payload: &image_payload,
            }],
            raw_blocks: vec![RawBlock {
                encoded: b"synthetic\0", uncompressed_size: None,
            }],
        };
        let visuals = VisualMap::from_datafile(&file, &root).unwrap();
        assert_eq!(visuals.images.len(), 1);
        assert_eq!(visuals.images[0].width, 2);
        assert_eq!(visuals.images[0].height, 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_camera_is_detected_before_render() {
        let map = VisualMap { images: vec![], layers: vec![] };
        let mut frame = Framebuffer::new(16, 16).unwrap();
        assert_eq!(map.render(&mut frame, Camera {
            center: Vec2::default(), scale: f32::NAN,
        }), Err(VisualError::Render(RenderError::InvalidCamera)));
    }
}
