//! CPU renderer for original DDNet map tiles, quad geometry and envelopes.
//! Loads PNGs and embedded RGBA data, preserving layer order and alpha.
//! GPU batching, group clipping and some DDNet extensions remain pending.

use std::path::Path;

use crate::datafile::{Datafile, DatafileError};
use crate::envelope::{EnvelopeSet, EnvelopeTimes};
use crate::graphics::{Camera, Framebuffer, RenderError, BACKGROUND};
use crate::map::Tile;
use crate::quad_render::{
    decode_quads, QuadLayer, QUAD_BYTES, MAX_QUADS_PER_LAYER, MAX_TOTAL_QUADS,
};
use crate::texture::{Texture, TextureError};

const MAP_IMAGE: u16 = 2;
const MAP_GROUP: u16 = 4;
const MAP_LAYER: u16 = 5;
const TILEMAP: i32 = 2;
const QUADMAP: i32 = 3;
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
pub struct GroupClip {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroupTransform {
    pub offset_x: f32,
    pub offset_y: f32,
    pub parallax_x: f32,
    pub parallax_y: f32,
    /// Group clipping uses the game-world camera, not parallax coordinates.
    pub clip: Option<GroupClip>,
}
impl GroupTransform {
    pub fn screen_clip(self, camera: Camera, width: f32, height: f32)
        -> Option<[f32; 4]>
    {
        let rect = self.clip?;
        let x = (rect.x - camera.center.x) * camera.scale + width * 0.5;
        let y = (rect.y - camera.center.y) * camera.scale + height * 0.5;
        Some([x, y, x + rect.width * camera.scale,
            y + rect.height * camera.scale])
    }
}
impl Default for GroupTransform {
    fn default() -> Self {
        Self {
            offset_x: 0.0, offset_y: 0.0,
            parallax_x: 100.0, parallax_y: 100.0, clip: None,
        }
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
    pub color_env: i32,
    pub color_env_offset: i32,
    /// Layers after the game layer render in front of players, like upstream.
    pub foreground: bool,
    /// Original map-layer index; used for exact tile/quad interleaving.
    pub order: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VisualMap {
    pub images: Vec<Texture>,
    pub layers: Vec<TexturedLayer>,
    pub quad_layers: Vec<QuadLayer>,
    pub envelopes: EnvelopeSet,
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
        let clip = if field(item.payload, 0)? >= 2 {
            if item.payload.len() < 48 { return Err(VisualError::InvalidMap); }
            (field(item.payload, 28)? != 0).then(|| -> Result<GroupClip, VisualError> {
                Ok(GroupClip {
                    x: field(item.payload, 32)? as f32,
                    y: field(item.payload, 36)? as f32,
                    width: field(item.payload, 40)? as f32,
                    height: field(item.payload, 44)? as f32,
                })
            }).transpose()?
        } else { None };
        let transform = GroupTransform {
            offset_x: field(item.payload, 4)? as f32,
            offset_y: field(item.payload, 8)? as f32,
            parallax_x: field(item.payload, 12)? as f32,
            parallax_y: field(item.payload, 16)? as f32,
            clip,
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
        let envelopes = EnvelopeSet::from_datafile(file)?;
        let mut layers = Vec::new();
        let mut quad_layers = Vec::new();
        let mut total = 0usize;
        let mut total_quads = 0usize;
        let game_layer_index = layer_items.iter().position(|item| {
            field(item.payload, 4).ok() == Some(TILEMAP)
                && field(item.payload, 24).ok()
                    .is_some_and(|flags| flags & 1 != 0)
        });
        for (ordinal, item) in layer_items.iter().enumerate() {
            if item.payload.len() < 8 { return Err(VisualError::InvalidMap); }
            if field(item.payload, 4)? == QUADMAP {
                // CMapItemLayerQuads: base layer (12), version (4),
                // quad count (4), data index (4), image index (4).
                if item.payload.len() < 28 { return Err(VisualError::InvalidMap); }
                let version = field(item.payload, 12)?;
                if !(1..=2).contains(&version) {
                    return Err(VisualError::InvalidMap);
                }
                let count = index(field(item.payload, 16)?)?;
                if count > MAX_QUADS_PER_LAYER { return Err(VisualError::TooLarge); }
                total_quads = total_quads.checked_add(count).ok_or(VisualError::TooLarge)?;
                if total_quads > MAX_TOTAL_QUADS { return Err(VisualError::TooLarge); }
                let image = match field(item.payload, 24)? {
                    -1 => None,
                    value if value >= 0 => {
                        let index = index(value)?;
                        if index >= images.len() { return Err(VisualError::MissingImage); }
                        Some(index)
                    }
                    _ => return Err(VisualError::InvalidImage),
                };
                if count > 0 {
                    let raw_bytes = file.raw_data(index(field(item.payload, 20)?)?,
                        count.checked_mul(QUAD_BYTES).ok_or(VisualError::TooLarge)?)?;
                    if raw_bytes.len() != count * QUAD_BYTES {
                        return Err(VisualError::InvalidMap);
                    }
                    quad_layers.push(QuadLayer {
                        order: ordinal,
                        foreground: game_layer_index.is_some_and(|index| ordinal > index),
                        transform: groups[ordinal],
                        image,
                        quads: decode_quads(&raw_bytes)?,
                    });
                }
                continue;
            }
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
                color_env: field(item.payload, 44)?,
                color_env_offset: field(item.payload, 48)?,
                foreground: game_layer_index.is_some_and(|game_index| ordinal > game_index),
                order: ordinal,
            });
        }
        Ok(Self { images, layers, quad_layers, envelopes })
    }

    /// Static frame for CLI previews. Live clients provide explicit clocks.
    pub fn render(
        &self, frame: &mut Framebuffer, camera: Camera,
    ) -> Result<usize, VisualError> {
        self.render_at(frame, camera, EnvelopeTimes::default())
    }

    pub fn render_at(
        &self, frame: &mut Framebuffer, camera: Camera, times: EnvelopeTimes,
    ) -> Result<usize, VisualError> {
        let bg = self.render_background_at(frame, camera, times)?;
        let fg = self.render_foreground_at(frame, camera, times)?;
        Ok(bg + fg)
    }

    pub fn render_background(
        &self, frame: &mut Framebuffer, camera: Camera,
    ) -> Result<usize, VisualError> {
        self.render_background_at(frame, camera, EnvelopeTimes::default())
    }

    pub fn render_background_at(
        &self, frame: &mut Framebuffer, camera: Camera, times: EnvelopeTimes,
    ) -> Result<usize, VisualError> {
        self.render_pass(frame, camera, false, true, times)
    }

    pub fn render_foreground(
        &self, frame: &mut Framebuffer, camera: Camera,
    ) -> Result<usize, VisualError> {
        self.render_foreground_at(frame, camera, EnvelopeTimes::default())
    }

    pub fn render_foreground_at(
        &self, frame: &mut Framebuffer, camera: Camera, times: EnvelopeTimes,
    ) -> Result<usize, VisualError> {
        self.render_pass(frame, camera, true, false, times)
    }

    fn render_pass(
        &self, frame: &mut Framebuffer, camera: Camera,
        foreground: bool, clear: bool, times: EnvelopeTimes,
    ) -> Result<usize, VisualError> {
        if !(0.05..=16.0).contains(&camera.scale) || !camera.scale.is_finite()
            || !camera.center.x.is_finite() || !camera.center.y.is_finite()
        {
            return Err(VisualError::Render(RenderError::InvalidCamera));
        }
        if clear { frame.clear(BACKGROUND); }
        let mut tile_index = 0usize;
        let mut quad_index = 0usize;
        let mut drawn = 0usize;
        // Both lists are sorted by their original map-layer ordinal. Merge
        // without per-frame allocations so alpha quads and tile art retain
        // the same z-order as the upstream layer list.
        while tile_index < self.layers.len() || quad_index < self.quad_layers.len() {
            let next_tile = self.layers.get(tile_index).map_or(usize::MAX, |layer| layer.order);
            let next_quad = self.quad_layers.get(quad_index).map_or(usize::MAX, |layer| layer.order);
            if next_tile <= next_quad {
                let layer = &self.layers[tile_index];
                tile_index += 1;
                if layer.foreground == foreground {
                    drawn += self.render_tile_layer(frame, camera, layer, times)?;
                }
            } else {
                let layer = &self.quad_layers[quad_index];
                quad_index += 1;
                if layer.foreground == foreground {
                    drawn += crate::quad_render::draw_quads_at(
                        layer, &self.images, frame, camera, &self.envelopes, times,
                    )?;
                }
            }
        }
        Ok(drawn)
    }

    fn render_tile_layer(
        &self, frame: &mut Framebuffer, camera: Camera,
        layer: &TexturedLayer, times: EnvelopeTimes,
    ) -> Result<usize, VisualError> {
        let mut drawn = 0usize;
        let texture = self.images.get(layer.image).ok_or(VisualError::MissingImage)?;
        let color = self.envelopes.evaluate(
            layer.color_env, layer.color_env_offset, times, [1.0; 4],
        );
        let tint: [u8; 4] = std::array::from_fn(|i| {
            (f32::from(layer.tint[i]) * color[i]).clamp(0.0, 255.0) as u8
        });

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
                    blit_tile(frame, texture, tile, tint,
                        (start_x, start_y, end_x, end_y), (cell_w, cell_h),
                        layer.transform.screen_clip(camera, frame.width as f32,
                            frame.height as f32));
                    drawn += 1;
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
    clip: Option<[f32; 4]>,
) {
    let (left, top, right, bottom) = bounds;
    let src_x = usize::from(tile.index % 16) * cell.0;
    let src_y = usize::from(tile.index / 16) * cell.1;
    let dst_width = (right - left) as f32;
    let dst_height = (bottom - top) as f32;
    let mut clipped_left = left.clamp(0, i64::from(frame.width));
    let mut clipped_top = top.clamp(0, i64::from(frame.height));
    let mut clipped_right = right.clamp(0, i64::from(frame.width));
    let mut clipped_bottom = bottom.clamp(0, i64::from(frame.height));
    if let Some([x0,y0,x1,y1]) = clip {
        if x1 <= x0 || y1 <= y0 { return; }
        // DDNet's Graphics::ClipEnable uses rounded framebuffer coordinates.
        clipped_left = clipped_left.max(x0.round() as i64);
        clipped_top = clipped_top.max(y0.round() as i64);
        clipped_right = clipped_right.min(x1.round() as i64);
        clipped_bottom = clipped_bottom.min(y1.round() as i64);
    }
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
                    ((u32::from(texture.rgba[src]) * u32::from(tint[0]) / 255) << 16)
                    | ((u32::from(texture.rgba[src + 1]) * u32::from(tint[1]) / 255) << 8)
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
    const MAP_ENVELOPE: u16 = 3;

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
    fn group_version_two_clipping_is_loaded_and_transformed() {
        let (_image, layer, _atlas, _tiles) = make_fixture();
        let group = words(&[2, 0, 0, 100, 100, 0, 1, 1, 8, 4, 16, 12]);
        let file = Datafile {
            version: 3, item_types: vec![],
            items: vec![
                Item { type_id: MAP_GROUP, id: 0, payload: &group },
                Item { type_id: MAP_LAYER, id: 0, payload: &layer },
            ],
            raw_blocks: vec![],
        };
        // Group metadata is parsed before fetching tile or texture assets.
        let groups = collect_groups(&file, 1).unwrap();
        assert_eq!(groups[0].clip, Some(GroupClip {
            x: 8.0, y: 4.0, width: 16.0, height: 12.0,
        }));
        let camera = Camera { center: Vec2::new(16.0, 16.0), scale: 1.0 };
        assert_eq!(groups[0].screen_clip(camera, 32.0, 32.0),
            Some([8.0, 4.0, 24.0, 16.0]));
    }

    #[test]
    fn clipped_tile_blit_respects_original_world_region() {
        let tex = Texture::from_rgba(16,16,vec![255; 16*16*4]).unwrap();
        let mut frame = Framebuffer::new(32,32).unwrap();
        frame.clear(0);
        let tile = Tile { index: 1, flags: 0, skip: 0, reserved: 0 };
        blit_tile(&mut frame, &tex, tile, [255;4],
            (0,0,32,32), (1,1), Some([8.0, 4.0, 24.0, 16.0]));
        assert_eq!(frame.pixel(7,7), Some(0));
        assert_eq!(frame.pixel(8,7), Some(0xffffff));
        assert_eq!(frame.pixel(23,15), Some(0xffffff));
        assert_eq!(frame.pixel(24,15), Some(0));
    }

    fn raw_quad(color: [i32; 4]) -> Vec<u8> {
        let mut bytes = vec![0u8; QUAD_BYTES];
        let positions = [(0, 0), (32, 0), (0, 32), (32, 32), (16, 16)];
        for (index, (x, y)) in positions.into_iter().enumerate() {
            bytes[index * 8..index * 8 + 4]
                .copy_from_slice(&(x * 1024i32).to_le_bytes());
            bytes[index * 8 + 4..index * 8 + 8]
                .copy_from_slice(&(y * 1024i32).to_le_bytes());
        }
        for i in 0..4 {
            for (channel, value) in color.into_iter().enumerate() {
                let start = 40 + i * 16 + channel * 4;
                bytes[start..start + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        bytes[136..140].copy_from_slice(&(-1i32).to_le_bytes());
        bytes[144..148].copy_from_slice(&(-1i32).to_le_bytes());
        bytes
    }

    #[test]
    fn decoded_map_envelopes_drive_actual_quad_and_tile_pixels() {
        // Original map uses envelope objects (type=3) and one contiguous
        // array of 24-byte envpoints (type=6).
        let pos_env = words(&[2, 3, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let color_env = words(&[2, 4, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let raw_points = words(&[
            0, 1, 0, 0, 0, 0,
            1000, 1, 16 * 1024, 0, 0, 0,
            0, 1, 1024, 1024, 1024, 1024,
            1000, 1, 512, 512, 512, 1024,
        ]);
        let mut quad = raw_quad([255, 0, 0, 255]);
        quad[136..140].copy_from_slice(&0i32.to_le_bytes());
        quad[144..148].copy_from_slice(&1i32.to_le_bytes());
        let raw_layer = words(&[0, QUADMAP, 0, 1, 1, 0, -1]);
        let file = Datafile {
            version: 3, item_types: vec![],
            items: vec![
                Item { type_id: MAP_ENVELOPE, id: 0, payload: &pos_env },
                Item { type_id: MAP_ENVELOPE, id: 1, payload: &color_env },
                Item { type_id: 6, id: 0, payload: &raw_points },
                Item { type_id: MAP_LAYER, id: 0, payload: &raw_layer },
            ],
            raw_blocks: vec![RawBlock { encoded: &quad, uncompressed_size: None }],
        };
        let map = VisualMap::from_datafile(&file, Path::new(".")).unwrap();
        assert_eq!(map.envelopes.envelopes.len(), 2);
        let mut frame = Framebuffer::new(48, 32).unwrap();
        let camera = Camera { center: Vec2::new(24.0, 16.0), scale: 1.0 };
        map.render_at(&mut frame, camera, EnvelopeTimes {
            game_ms: 0.0, local_ms: 999.0,
        }).unwrap();
        assert_eq!(frame.pixel(4, 4), Some(0xff0000));
        map.render_at(&mut frame, camera, EnvelopeTimes {
            game_ms: 500.0, local_ms: 0.0,
        }).unwrap();
        assert_eq!(frame.pixel(4, 4), Some(BACKGROUND));
        assert_eq!(frame.pixel(12, 4), Some(0xbf0000));
        // Time wraps at the final keyframe.
        map.render_at(&mut frame, camera, EnvelopeTimes {
            game_ms: 1000.0, local_ms: 0.0,
        }).unwrap();
        assert_eq!(frame.pixel(4, 4), Some(0xff0000));
    }

    #[test]
    fn static_tile_tint_evaluates_animated_color_envelopes() {
        let (image_payload, mut layer_payload, atlas, tiles) = make_fixture();
        layer_payload[44..48].copy_from_slice(&0i32.to_le_bytes());
        let env = words(&[2, 4, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let points = words(&[
            0, 1, 1024, 1024, 1024, 1024,
            1000, 1, 0, 0, 0, 1024,
        ]);
        let file = Datafile {
            version: 3, item_types: vec![],
            items: vec![
                Item { type_id: MAP_IMAGE, id: 0, payload: &image_payload },
                Item { type_id: MAP_LAYER, id: 0, payload: &layer_payload },
                Item { type_id: MAP_ENVELOPE, id: 0, payload: &env },
                Item { type_id: 6, id: 0, payload: &points },
            ],
            raw_blocks: vec![
                RawBlock { encoded: b"synthetic\0", uncompressed_size: None },
                RawBlock { encoded: &atlas, uncompressed_size: None },
                RawBlock { encoded: &tiles, uncompressed_size: None },
            ],
        };
        let map = VisualMap::from_datafile(&file, Path::new(".")).unwrap();
        let camera = Camera { center: Vec2::new(32.0, 16.0), scale: 1.0 };
        let mut frame = Framebuffer::new(64, 32).unwrap();
        map.render_at(&mut frame, camera, EnvelopeTimes::default()).unwrap();
        assert_eq!(frame.pixel(16, 16), Some(0xff0000));
        map.render_at(&mut frame, camera, EnvelopeTimes {
            game_ms: 500.0, local_ms: 0.0,
        }).unwrap();
        assert_eq!(frame.pixel(16, 16), Some(0x7f0000));
    }

    #[test]
    fn loads_quads_from_map_file_with_original_game_layer_z_order() {
        // Quad background (red), physics Game layer (not visible as visual
        // art), quad foreground (blue): geometry comes from raw CQuad data.
        let first_layer = words(&[0, QUADMAP, 0, 1, 1, 0, -1]);
        let game = words(&[
            0, TILEMAP, 0, 2, 1, 1, 1,
            255, 255, 255, 255, -1, 0, -1, 0,
        ]);
        let last_layer = words(&[0, QUADMAP, 0, 1, 1, 1, -1]);
        let red = raw_quad([255, 0, 0, 255]);
        let blue = raw_quad([0, 0, 255, 255]);
        let file = Datafile {
            version: 3, item_types: vec![],
            items: vec![
                Item { type_id: MAP_LAYER, id: 0, payload: &first_layer },
                Item { type_id: MAP_LAYER, id: 1, payload: &game },
                Item { type_id: MAP_LAYER, id: 2, payload: &last_layer },
            ],
            raw_blocks: vec![
                RawBlock { encoded: &red, uncompressed_size: None },
                RawBlock { encoded: &blue, uncompressed_size: None },
            ],
        };
        let map = VisualMap::from_datafile(&file, Path::new(".")).unwrap();
        assert!(map.layers.is_empty());
        assert_eq!(map.quad_layers.len(), 2);
        assert!(!map.quad_layers[0].foreground);
        assert!(map.quad_layers[1].foreground);
        assert_eq!(map.quad_layers[0].order, 0);
        assert_eq!(map.quad_layers[1].order, 2);
        let mut frame = Framebuffer::new(32, 32).unwrap();
        let camera = Camera { center: Vec2::new(16.0, 16.0), scale: 1.0 };
        assert_eq!(map.render_background(&mut frame, camera), Ok(1));
        assert_eq!(frame.pixel(8, 8), Some(0xff0000));
        frame.pixels[8 * 32 + 8] = 0x00ff00;
        assert_eq!(map.render_foreground(&mut frame, camera), Ok(1));
        assert_eq!(frame.pixel(8, 8), Some(0x0000ff));
        assert_eq!(map.render(&mut frame, camera), Ok(2));
    }

    #[test]
    fn preserves_original_tile_and_quad_interleaving() {
        // A solid quad first, then an ordinary green image-backed tile.
        // Rendering quads in a separate post-pass would incorrectly cover it.
        let image = words(&[1, 16, 16, 0, 2, 3]);
        let quad_layer = words(&[0, QUADMAP, 0, 1, 1, 0, -1]);
        let tile_layer = words(&[
            0, TILEMAP, 0, 2, 1, 1, 0,
            255, 255, 255, 255, -1, 0, 0, 1,
        ]);
        let rgba = [0u8, 255, 0, 255].repeat(16 * 16);
        let quad = raw_quad([255, 0, 0, 255]);
        let tile = [1u8, 0, 0, 0];
        let file = Datafile {
            version: 3, item_types: vec![],
            items: vec![
                Item { type_id: MAP_IMAGE, id: 0, payload: &image },
                Item { type_id: MAP_LAYER, id: 0, payload: &quad_layer },
                Item { type_id: MAP_LAYER, id: 1, payload: &tile_layer },
            ],
            raw_blocks: vec![
                RawBlock { encoded: &quad, uncompressed_size: None },
                RawBlock { encoded: &tile, uncompressed_size: None },
                RawBlock { encoded: b"green\0", uncompressed_size: None },
                RawBlock { encoded: &rgba, uncompressed_size: None },
            ],
        };
        let map = VisualMap::from_datafile(&file, Path::new(".")).unwrap();
        assert_eq!(map.layers.len(), 1);
        assert_eq!(map.quad_layers.len(), 1);
        let camera = Camera { center: Vec2::new(16.0, 16.0), scale: 1.0 };
        let mut frame = Framebuffer::new(32, 32).unwrap();
        assert_eq!(map.render_background(&mut frame, camera), Ok(2));
        assert_eq!(frame.pixel(8, 8), Some(0x00ff00));
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
            color_env: -1, color_env_offset: 0,
            foreground, order: usize::from(foreground),
        };
        let map = VisualMap {
            images: vec![image], layers: vec![layer(false), layer(true)],
            quad_layers: vec![], envelopes: EnvelopeSet::default(),
        };
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
        let map = VisualMap {
            images: vec![], layers: vec![], quad_layers: vec![],
            envelopes: EnvelopeSet::default(),
        };
        let mut frame = Framebuffer::new(16, 16).unwrap();
        assert_eq!(map.render(&mut frame, Camera {
            center: Vec2::default(), scale: f32::NAN,
        }), Err(VisualError::Render(RenderError::InvalidCamera)));
    }
}
