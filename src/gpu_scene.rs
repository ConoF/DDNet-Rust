//! CPU-side *geometry generation*, not CPU pixel rasterization.
//! Every frame, emit only visible 0.6 map tile/quad triangles in original
//! layer order. A GPU backend can draw all pixels with batched textures.
//! This is independent of wgpu so map geometry has deterministic unit tests.

use crate::envelope::EnvelopeTimes;
use crate::graphics::{Camera, RenderError};
use crate::map_visual::{VisualError, VisualMap};
use crate::quad_render::QuadLayer;

pub const VERTEX_STRIDE: usize = 20;
pub const MAX_FRAME_VERTICES: usize = 500_000;

/// Cached world-space bounds for static quads, indexed by quad layer and quad.
/// Dynamic quads keep None so their animated positions are evaluated normally.
pub type QuadBounds = Vec<Vec<Option<[f32; 4]>>>;

/// Sorted occupied tile columns for each row in each tile layer. Built when
/// the visual map is loaded, so rendering skips empty tiles without scanning
/// entire visible rows on every camera movement.
pub type OccupiedTileRows = Vec<Vec<Vec<usize>>>;

pub fn cache_occupied_tiles(map: &VisualMap) -> OccupiedTileRows {
    map.layers.iter().map(|layer| {
        if layer.width == 0 || layer.width.checked_mul(layer.height) != Some(layer.tiles.len()) {
            return Vec::new();
        }
        layer.tiles.chunks_exact(layer.width).map(|row| {
            row.iter().enumerate().filter_map(|(x, tile)| {
                (tile.index != 0).then_some(x)
            }).collect()
        }).collect()
    }).collect()
}

pub fn cache_quad_bounds(map: &VisualMap) -> QuadBounds {
    map.quad_layers.iter().map(|layer| {
        layer.quads.iter().map(|quad| {
            if quad.pos_env >= 0 { return None; }
            let xs = quad.vertices.map(|v| v.position[0]);
            let ys = quad.vertices.map(|v| v.position[1]);
            if xs.iter().chain(ys.iter()).any(|n| !n.is_finite()) {
                return None;
            }
            Some([
                xs.into_iter().fold(f32::INFINITY, f32::min),
                xs.into_iter().fold(f32::NEG_INFINITY, f32::max),
                ys.into_iter().fold(f32::INFINITY, f32::min),
                ys.into_iter().fold(f32::NEG_INFINITY, f32::max),
            ])
        }).collect()
    }).collect()
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuVertex {
    /// NDC: (-1,-1) bottom-left, (+1,+1) top-right.
    pub clip: [f32; 2],
    pub uv: [f32; 2],
    pub color: [u8; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuBatch {
    /// Texture index into VisualMap.images, or images.len() for white.
    pub texture: usize,
    pub start: u32,
    pub count: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuScene {
    pub vertices: Vec<GpuVertex>,
    pub background: Vec<GpuBatch>,
    pub foreground: Vec<GpuBatch>,
}

impl GpuScene {
    pub fn byte_len(&self) -> usize { self.vertices.len() * VERTEX_STRIDE }

    /// Explicit byte layout, avoiding unsafe casts and alignment assumptions.
    pub fn vertex_bytes(&self) -> Vec<u8> {
        let mut result = Vec::with_capacity(self.byte_len());
        self.write_vertex_bytes(&mut result);
        result
    }

    /// Serialize into a reusable upload buffer. The GPU frame loop keeps this
    /// allocation between frames instead of allocating up to 10 MiB of
    /// temporary vertex data on every redraw.
    pub fn write_vertex_bytes(&self, result: &mut Vec<u8>) {
        self.write_vertex_bytes_from(0, result);
    }

    /// Serialize only dynamic vertices following a previously uploaded map
    /// prefix. The prefix must remain unchanged in the GPU vertex buffer.
    pub fn write_vertex_bytes_from(&self, first: usize, result: &mut Vec<u8>) {
        result.clear();
        let vertices = self.vertices.get(first..).unwrap_or(&[]);
        result.reserve(vertices.len() * VERTEX_STRIDE);
        for v in vertices {
            for n in v.clip.iter().chain(v.uv.iter()) {
                result.extend_from_slice(&n.to_le_bytes());
            }
            result.extend_from_slice(&v.color);
        }
    }
}

#[derive(Clone, Copy)]
struct V {
    x: f32,
    y: f32,
    uv: [f32; 2],
    color: [u8; 4],
}

struct Builder {
    scene: GpuScene,
    w: f32,
    h: f32,
}

impl Builder {
    fn add_quad(
        &mut self, pts: [V; 4], texture: usize, foreground: bool,
    ) -> Result<(), VisualError> {
        if pts.iter().any(|v| !v.x.is_finite() || !v.y.is_finite()) { return Ok(()); }
        let min_x = pts.iter().map(|v| v.x).fold(f32::INFINITY, f32::min);
        let max_x = pts.iter().map(|v| v.x).fold(f32::NEG_INFINITY, f32::max);
        let min_y = pts.iter().map(|v| v.y).fold(f32::INFINITY, f32::min);
        let max_y = pts.iter().map(|v| v.y).fold(f32::NEG_INFINITY, f32::max);
        if min_x >= self.w || max_x <= 0.0 || min_y >= self.h || max_y <= 0.0 {
            return Ok(());
        }
        if self.scene.vertices.len() > MAX_FRAME_VERTICES - 6 {
            return Err(VisualError::TooLarge);
        }
        let start = u32::try_from(self.scene.vertices.len())
            .map_err(|_| VisualError::TooLarge)?;
        for corner in [0, 1, 2, 1, 3, 2] {
            let v = pts[corner];
            self.scene.vertices.push(GpuVertex {
                clip: [v.x * 2.0 / self.w - 1.0, 1.0 - v.y * 2.0 / self.h],
                uv: v.uv, color: v.color,
            });
        }
        let batches = if foreground {
            &mut self.scene.foreground
        } else { &mut self.scene.background };
        if let Some(last) = batches.last_mut() {
            if last.texture == texture && last.start + last.count == start {
                last.count += 6;
                return Ok(());
            }
        }
        batches.push(GpuBatch { texture, start, count: 6 });
        Ok(())
    }

    fn tile_layer(
        &mut self, map: &VisualMap, layer: &crate::map_visual::TexturedLayer,
        camera: Camera, times: EnvelopeTimes,
        occupied_rows: Option<&[Vec<usize>]>,
    ) -> Result<(), VisualError> {
        let image = map.images.get(layer.image).ok_or(VisualError::MissingImage)?;
        if image.width % 16 != 0 || image.height % 16 != 0 {
            return Err(VisualError::InvalidImage);
        }
        if layer.width.checked_mul(layer.height) != Some(layer.tiles.len()) {
            return Err(VisualError::InvalidMap);
        }
        let tint = map.envelopes.evaluate(
            layer.color_env, layer.color_env_offset, times, [1.0; 4],
        );
        let color = std::array::from_fn(|i| {
            (f32::from(layer.tint[i]) * tint[i]).clamp(0.0, 255.0) as u8
        });
        let cx = camera.center.x * layer.transform.parallax_x / 100.0
            - layer.transform.offset_x;
        let cy = camera.center.y * layer.transform.parallax_y / 100.0
            - layer.transform.offset_y;
        let radius_x = self.w / (2.0 * camera.scale);
        let radius_y = self.h / (2.0 * camera.scale);
        let x0 = ((cx - radius_x) / 32.0).floor().max(0.0) as usize;
        let x1 = ((cx + radius_x) / 32.0).ceil().max(0.0) as usize;
        let y0 = ((cy - radius_y) / 32.0).floor().max(0.0) as usize;
        let y1 = ((cy + radius_y) / 32.0).ceil().max(0.0) as usize;
        let from_x = x0.min(layer.width);
        let to_x = x1.saturating_add(1).min(layer.width);
        for y in y0.min(layer.height)..y1.saturating_add(1).min(layer.height) {
            // Cached rows are ordered left-to-right, exactly like the original
            // dense scan. Fall back to the dense scan for missing cache rows.
            let cached = occupied_rows.and_then(|rows| rows.get(y));
            let draw_tile = |x: usize, this: &mut Builder| -> Result<(), VisualError> {
                let tile = layer.tiles[y * layer.width + x];
                if tile.index == 0 { return Ok(()); }
                let sx = (x as f32 * 32.0 - cx) * camera.scale + this.w / 2.0;
                let sy = (y as f32 * 32.0 - cy) * camera.scale + this.h / 2.0;
                let size = 32.0 * camera.scale;
                let tile_u = f32::from(tile.index % 16);
                let tile_v = f32::from(tile.index / 16);
                let uv = |mut u: f32, mut v: f32| {
                    if tile.flags & 8 != 0 { (u, v) = (v, 1.0 - u); }
                    if tile.flags & 1 != 0 { u = 1.0 - u; }
                    if tile.flags & 2 != 0 { v = 1.0 - v; }
                    [(tile_u + u) / 16.0, (tile_v + v) / 16.0]
                };
                let pts = [
                    V { x: sx, y: sy, uv: uv(0.0,0.0), color },
                    V { x: sx + size, y: sy, uv: uv(1.0,0.0), color },
                    V { x: sx, y: sy + size, uv: uv(0.0,1.0), color },
                    V { x: sx + size, y: sy + size, uv: uv(1.0,1.0), color },
                ];
                this.add_quad(pts, layer.image, layer.foreground)
            };
            if let Some(columns) = cached {
                let start = columns.partition_point(|&x| x < from_x);
                let end = columns.partition_point(|&x| x < to_x);
                for &x in &columns[start..end] {
                    draw_tile(x, self)?;
                }
            } else {
                for x in from_x..to_x {
                    draw_tile(x, self)?;
                }
            }
        }
        Ok(())
    }

    fn quads(
        &mut self, map: &VisualMap, layer: &QuadLayer,
        camera: Camera, times: EnvelopeTimes, cached_bounds: Option<&[Option<[f32; 4]>]>,
    ) -> Result<(), VisualError> {
        let cx = camera.center.x * layer.transform.parallax_x / 100.0
            - layer.transform.offset_x;
        let cy = camera.center.y * layer.transform.parallax_y / 100.0
            - layer.transform.offset_y;
        let tex = layer.image.unwrap_or(map.images.len());
        // Skip static quads in map space before evaluating envelopes,
        // applying vertex transforms, or issuing GPU geometry. Animated
        // position envelopes are excluded: they may move into view.
        let half_w = self.w / (2.0 * camera.scale);
        let half_h = self.h / (2.0 * camera.scale);
        for (index, quad) in layer.quads.iter().enumerate() {
            if quad.pos_env < 0 {
                let bounds = cached_bounds.and_then(|b| b.get(index).copied().flatten())
                    .or_else(|| {
                        let xs = quad.vertices.map(|v| v.position[0]);
                        let ys = quad.vertices.map(|v| v.position[1]);
                        if xs.iter().chain(ys.iter()).any(|n| !n.is_finite()) {
                            return None;
                        }
                        Some([
                            xs.into_iter().fold(f32::INFINITY, f32::min),
                            xs.into_iter().fold(f32::NEG_INFINITY, f32::max),
                            ys.into_iter().fold(f32::INFINITY, f32::min),
                            ys.into_iter().fold(f32::NEG_INFINITY, f32::max),
                        ])
                    });
                if let Some([min_x, max_x, min_y, max_y]) = bounds {
                    if max_x <= cx-half_w || min_x >= cx+half_w
                        || max_y <= cy-half_h || min_y >= cy+half_h
                    {
                        continue;
                    }
                }
            }
            let position = map.envelopes.evaluate(
                quad.pos_env, quad.pos_env_offset, times, [0.0; 4],
            );
            let color = map.envelopes.evaluate(
                quad.color_env, quad.color_env_offset, times, [1.0; 4],
            );
            let (sin, cos) = position[2].to_radians().sin_cos();
            let pts = quad.vertices.map(|vertex| {
                let rx = vertex.position[0] - quad.pivot[0];
                let ry = vertex.position[1] - quad.pivot[1];
                let x = if quad.pos_env >= 0 {
                    quad.pivot[0] + rx * cos - ry * sin + position[0]
                } else { vertex.position[0] };
                let y = if quad.pos_env >= 0 {
                    quad.pivot[1] + rx * sin + ry * cos + position[1]
                } else { vertex.position[1] };
                V {
                    x: (x - cx) * camera.scale + self.w / 2.0,
                    y: (y - cy) * camera.scale + self.h / 2.0,
                    uv: vertex.uv,
                    color: std::array::from_fn(|i| {
                        (f32::from(vertex.color[i]) * color[i]).clamp(0.0, 255.0) as u8
                    }),
                }
            });
            self.add_quad(pts, tex, layer.foreground)?;
        }
        Ok(())
    }
}

pub fn build_gpu_scene(
    map: &VisualMap, camera: Camera,
    times: EnvelopeTimes, width: u32, height: u32,
) -> Result<GpuScene, VisualError> {
    let mut scene = GpuScene::default();
    build_gpu_scene_into(map, camera, times, width, height, &mut scene)?;
    Ok(scene)
}

/// Rebuild only visible geometry while preserving Vec capacities from the
/// previous frame. No stale foreground/background batches survive a redraw.
pub fn build_gpu_scene_into(
    map: &VisualMap, camera: Camera,
    times: EnvelopeTimes, width: u32, height: u32,
    scene: &mut GpuScene,
) -> Result<(), VisualError> {
    build_gpu_scene_into_cached(map, camera, times, width, height, scene, None, None)
}

/// The renderer supplies bounds computed only when a new map is loaded.
/// Animation, layer order and the returned scene remain identical.
pub fn build_gpu_scene_into_cached(
    map: &VisualMap, camera: Camera,
    times: EnvelopeTimes, width: u32, height: u32,
    scene: &mut GpuScene, bounds: Option<&QuadBounds>,
    occupied_tiles: Option<&OccupiedTileRows>,
) -> Result<(), VisualError> {
    if width == 0 || height == 0
        || !(0.05..=16.0).contains(&camera.scale)
        || !camera.center.x.is_finite() || !camera.center.y.is_finite()
    {
        return Err(VisualError::Render(RenderError::InvalidCamera));
    }
    scene.vertices.clear();
    scene.background.clear();
    scene.foreground.clear();
    // Reborrow existing storage without reallocating the largest scene arrays.
    let mut builder = Builder {
        scene: std::mem::take(scene), w: width as f32, h: height as f32,
    };
    let result = (|| {
        for foreground in [false, true] {
            let mut tile = 0;
            let mut quad = 0;
            while tile < map.layers.len() || quad < map.quad_layers.len() {
                let next_tile = map.layers.get(tile).map_or(usize::MAX, |l| l.order);
                let next_quad = map.quad_layers.get(quad).map_or(usize::MAX, |l| l.order);
                if next_tile <= next_quad {
                    let layer = &map.layers[tile];
                    tile += 1;
                    if layer.foreground == foreground {
                        builder.tile_layer(map, layer, camera, times,
                            occupied_tiles.and_then(|rows| rows.get(tile - 1))
                                .map(Vec::as_slice))?;
                    }
                } else {
                    let layer = &map.quad_layers[quad];
                    quad += 1;
                    if layer.foreground == foreground {
                        builder.quads(map, layer, camera, times,
                            bounds.and_then(|b| b.get(quad - 1)).map(Vec::as_slice))?;
                    }
                }
            }
        }
        Ok(())
    })();
    // Always hand the allocation back, including on invalid maps. The next
    // frame clears incomplete geometry before generating fresh batches.
    *scene = builder.scene;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Envelope, EnvelopePoint, EnvelopeSet};
    use crate::map::Tile;
    use crate::map_visual::{GroupTransform, TexturedLayer};
    use crate::quad_render::{MapQuad, QuadLayer, QuadVertex};
    use crate::physics::Vec2;
    use crate::texture::Texture;

    fn map() -> VisualMap {
        let vertices = [
            [0.0, 0.0], [32.0, 0.0], [0.0, 32.0], [32.0, 32.0],
        ].map(|position| QuadVertex {
            position, uv: [0.5, 0.5], color: [255, 0, 0, 255],
        });
        let q = MapQuad {
            vertices, pivot: [16.0, 16.0],
            pos_env: -1, pos_env_offset: 0, color_env: -1, color_env_offset: 0,
        };
        let tile = TexturedLayer {
            width: 1, height: 1, image: 0, tint: [255; 4],
            transform: GroupTransform::default(),
            tiles: vec![Tile { index: 1, flags: 0, skip: 0, reserved: 0 }],
            color_env: -1, color_env_offset: 0,
            foreground: false, order: 1,
        };
        VisualMap {
            images: vec![Texture::from_rgba(16, 16, vec![255; 1024]).unwrap()],
            layers: vec![tile],
            quad_layers: vec![QuadLayer {
                order: 0, foreground: false,
                transform: GroupTransform::default(), image: None, quads: vec![q],
            }],
            envelopes: EnvelopeSet::default(),
        }
    }
    fn camera() -> Camera {
        Camera { center: Vec2::new(16.0, 16.0), scale: 1.0 }
    }

    #[test]
    fn quad_tile_order_and_texture_batches_match_original_layer_indices() {
        let scene = build_gpu_scene(&map(), camera(),
            EnvelopeTimes::default(), 32, 32).unwrap();
        assert_eq!(scene.vertices.len(), 12);
        assert_eq!(scene.background.len(), 2);
        assert_eq!(scene.background[0].texture, 1); // untextured white atlas
        assert_eq!(scene.background[1].texture, 0);
        assert_eq!(scene.background[0].start, 0);
        assert_eq!(scene.background[1].start, 6);
        assert!(scene.foreground.is_empty());
        let bytes = scene.vertex_bytes();
        assert_eq!(bytes.len(), 12 * VERTEX_STRIDE);
        assert_eq!(&bytes[..4], &(-1.0f32).to_le_bytes());
    }

    #[test]
    fn reusable_vertex_upload_matches_fresh_serialization_and_clears_old_data() {
        let scene = build_gpu_scene(&map(), camera(),
            EnvelopeTimes::default(), 32, 32).unwrap();
        let expected = scene.vertex_bytes();
        let mut buffer = vec![0xff; expected.len() + 50];
        scene.write_vertex_bytes(&mut buffer);
        assert_eq!(buffer, expected);
        let capacity = buffer.capacity();
        scene.write_vertex_bytes(&mut buffer);
        assert_eq!(buffer, expected);
        assert_eq!(buffer.capacity(), capacity);
        GpuScene::default().write_vertex_bytes(&mut buffer);
        assert!(buffer.is_empty());
    }

    #[test]
    fn partial_vertex_serialization_matches_complete_stream() {
        let scene = build_gpu_scene(&map(), camera(),
            EnvelopeTimes::default(), 32, 32).unwrap();
        let complete = scene.vertex_bytes();
        let mut suffix = Vec::new();
        scene.write_vertex_bytes_from(6, &mut suffix);
        assert_eq!(suffix, complete[6 * VERTEX_STRIDE..]);
        scene.write_vertex_bytes_from(scene.vertices.len(), &mut suffix);
        assert!(suffix.is_empty());
    }

    #[test]
    fn in_place_scene_rebuild_reuses_capacity_and_drops_stale_batches() {
        let m = map();
        let mut reused = GpuScene::default();
        build_gpu_scene_into(&m, camera(), EnvelopeTimes::default(),
            32, 32, &mut reused).unwrap();
        let previous_capacity = reused.vertices.capacity();
        let expected = reused.clone();
        build_gpu_scene_into(&m, camera(), EnvelopeTimes::default(),
            32, 32, &mut reused).unwrap();
        assert_eq!(reused, expected);
        assert_eq!(reused.vertices.capacity(), previous_capacity);
        let mut foreground = m.clone();
        foreground.quad_layers[0].foreground = true;
        build_gpu_scene_into(&foreground, camera(), EnvelopeTimes::default(),
            32, 32, &mut reused).unwrap();
        assert_eq!(reused.background.len(), 1);
        assert_eq!(reused.foreground.len(), 1);
        assert_eq!(reused.vertices.len(), 12);
    }

    #[test]
    fn invalid_map_keeps_reusable_scene_allocation_for_recovery() {
        let mut m = map();
        let mut reusable = GpuScene::default();
        build_gpu_scene_into(&m, camera(), EnvelopeTimes::default(),
            32, 32, &mut reusable).unwrap();
        let capacity = reusable.vertices.capacity();
        m.layers[0].width = 2; // Inconsistent tile count: error after first quad.
        assert_eq!(build_gpu_scene_into(&m, camera(), EnvelopeTimes::default(),
            32, 32, &mut reusable), Err(VisualError::InvalidMap));
        assert_eq!(reusable.vertices.capacity(), capacity);
        m.layers[0].width = 1;
        build_gpu_scene_into(&m, camera(), EnvelopeTimes::default(),
            32, 32, &mut reusable).unwrap();
        assert_eq!(reusable, build_gpu_scene(&m, camera(),
            EnvelopeTimes::default(), 32, 32).unwrap());
    }

    #[test]
    fn precomputed_quad_bounds_match_uncached_across_camera_movements() {
        let mut map = map();
        let quad_cache = cache_quad_bounds(&map);
        assert_eq!(quad_cache.len(), 1);
        assert!(quad_cache[0][0].is_some());
        for x in [16.0, 0.0, 10000.0] {
            let c = Camera { center: Vec2::new(x, 16.0), scale: 1.0 };
            let direct = build_gpu_scene(&map, c, EnvelopeTimes::default(), 32, 32).unwrap();
            let mut cached = GpuScene::default();
            build_gpu_scene_into_cached(&map, c, EnvelopeTimes::default(),
                32, 32, &mut cached, Some(&quad_cache), None).unwrap();
            assert_eq!(cached, direct);
        }
        map.quad_layers[0].quads[0].pos_env = 0;
        assert!(cache_quad_bounds(&map)[0][0].is_none());
    }

    #[test]
    fn sparse_tile_cache_matches_dense_renderer_at_multiple_positions() {
        let mut m = map();
        m.layers[0].width = 64;
        m.layers[0].height = 2;
        m.layers[0].tiles = vec![Tile {
            index: 0, flags: 0, skip: 0, reserved: 0,
        }; 128];
        for (index, tile) in [(0usize, 1u8), (7, 5), (62, 9),
            (64, 11), (90, 3), (127, 14)] {
            m.layers[0].tiles[index].index = tile;
        }
        m.layers[0].tiles[7].flags = 1 | 8;
        let cache = cache_occupied_tiles(&m);
        assert_eq!(cache[0][0], vec![0, 7, 62]);
        assert_eq!(cache[0][1], vec![0, 26, 63]);
        for x in [-100.0, 16.0, 250.0, 900.0, 1900.0, 4000.0] {
            let c = Camera { center: Vec2::new(x, 32.0), scale: 1.0 };
            let expected = build_gpu_scene(&m, c, EnvelopeTimes::default(), 256, 96)
                .unwrap();
            let mut actual = GpuScene::default();
            build_gpu_scene_into_cached(&m, c, EnvelopeTimes::default(),
                256, 96, &mut actual, None, Some(&cache)).unwrap();
            assert_eq!(actual, expected, "camera x={x}");
        }
    }

    #[test]
    fn foreground_geometry_is_drawn_after_background() {
        let mut scene_map = map();
        scene_map.quad_layers[0].foreground = true;
        let scene = build_gpu_scene(&scene_map, camera(),
            EnvelopeTimes::default(), 32, 32).unwrap();
        assert_eq!(scene.background.len(), 1);
        assert_eq!(scene.foreground.len(), 1);
        assert_eq!(scene.background[0].start, 0);
        assert_eq!(scene.foreground[0].start, 6);
    }

    #[test]
    fn culls_offscreen_quads_and_preserves_flipped_tile_uvs() {
        let mut m = map();
        for v in &mut m.quad_layers[0].quads[0].vertices {
            v.position[0] += 50000.0;
        }
        m.layers[0].tiles[0].flags = 1;
        let scene = build_gpu_scene(&m, camera(),
            EnvelopeTimes::default(), 32, 32).unwrap();
        assert_eq!(scene.vertices.len(), 6);
        assert_eq!(scene.vertices[0].uv[0], 2.0 / 16.0);
        assert_eq!(scene.vertices[1].uv[0], 1.0 / 16.0);
        assert!(build_gpu_scene(&m, camera(), EnvelopeTimes::default(), 0, 32).is_err());
    }

    #[test]
    fn offscreen_static_quads_are_excluded_but_animated_positions_are_kept() {
        let mut scene_map = map();
        // This quad is far outside the camera and must not emit geometry.
        for v in &mut scene_map.quad_layers[0].quads[0].vertices {
            v.position[0] += 10_000.0;
        }
        let static_scene = build_gpu_scene(&scene_map, camera(),
            EnvelopeTimes::default(), 32, 32).unwrap();
        assert_eq!(static_scene.vertices.len(), 6); // only the tile
        // A position envelope can move it back into view. The conservative
        // first-pass check cannot cull such quads.
        scene_map.quad_layers[0].quads[0].pos_env = 0;
        scene_map.envelopes.envelopes.push(Envelope {
            channels: 3, synchronized: true,
            points: vec![EnvelopePoint {
                time_ms: 0, curve: 1, values: [-10_240_000, 0, 0, 0],
                bezier: None,
            }],
        });
        let animated_scene = build_gpu_scene(&scene_map, camera(),
            EnvelopeTimes::default(), 32, 32).unwrap();
        assert_eq!(animated_scene.vertices.len(), 12);
    }

    #[test]
    fn envelopes_transform_gpu_geometry_without_cpu_pixel_rasterization() {
        let mut m = map();
        m.quad_layers[0].quads[0].pos_env = 0;
        m.quad_layers[0].quads[0].color_env = 1;
        m.envelopes.envelopes = vec![
            Envelope {
                channels: 3, synchronized: true, points: vec![
                    EnvelopePoint {time_ms:0,curve:1,values:[0;4],bezier:None},
                    EnvelopePoint {time_ms:1000,curve:1,values:[8192,0,0,0],bezier:None},
                ],
            },
            Envelope {
                channels: 4, synchronized: true, points: vec![
                    EnvelopePoint {time_ms:0,curve:1,values:[1024;4],bezier:None},
                    EnvelopePoint {time_ms:1000,curve:1,values:[512,1024,1024,1024],bezier:None},
                ],
            },
        ];
        let start = build_gpu_scene(&m, camera(),
            EnvelopeTimes::default(), 32, 32).unwrap();
        let moved = build_gpu_scene(&m, camera(), EnvelopeTimes {
            game_ms: 500.0, local_ms: 0.0,
        }, 32, 32).unwrap();
        assert_ne!(start.vertices[0].clip, moved.vertices[0].clip);
        assert_eq!(moved.vertices[0].color[0], 191);
    }
}
