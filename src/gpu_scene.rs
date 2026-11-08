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
        for v in &self.vertices {
            for n in v.clip.iter().chain(v.uv.iter()) {
                result.extend_from_slice(&n.to_le_bytes());
            }
            result.extend_from_slice(&v.color);
        }
        result
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
        for y in y0.min(layer.height)..y1.saturating_add(1).min(layer.height) {
            for x in x0.min(layer.width)..x1.saturating_add(1).min(layer.width) {
                let tile = layer.tiles[y * layer.width + x];
                if tile.index == 0 { continue; }
                let sx = (x as f32 * 32.0 - cx) * camera.scale + self.w / 2.0;
                let sy = (y as f32 * 32.0 - cy) * camera.scale + self.h / 2.0;
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
                self.add_quad(pts, layer.image, layer.foreground)?;
            }
        }
        Ok(())
    }

    fn quads(
        &mut self, map: &VisualMap, layer: &QuadLayer,
        camera: Camera, times: EnvelopeTimes,
    ) -> Result<(), VisualError> {
        let cx = camera.center.x * layer.transform.parallax_x / 100.0
            - layer.transform.offset_x;
        let cy = camera.center.y * layer.transform.parallax_y / 100.0
            - layer.transform.offset_y;
        let tex = layer.image.unwrap_or(map.images.len());
        for quad in &layer.quads {
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
    if width == 0 || height == 0
        || !(0.05..=16.0).contains(&camera.scale)
        || !camera.center.x.is_finite() || !camera.center.y.is_finite()
    {
        return Err(VisualError::Render(RenderError::InvalidCamera));
    }
    let mut builder = Builder {
        scene: GpuScene::default(), w: width as f32, h: height as f32,
    };
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
                    builder.tile_layer(map, layer, camera, times)?;
                }
            } else {
                let layer = &map.quad_layers[quad];
                quad += 1;
                if layer.foreground == foreground {
                    builder.quads(map, layer, camera, times)?;
                }
            }
        }
    }
    Ok(builder.scene)
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
