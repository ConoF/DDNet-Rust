//! DDNet 0.6 map quads (animated translation/rotation/RGBA).
//! Source: DDNet 20.2.1 mapitems.h (CQuad, 152 bytes) and render_map.cpp.
//! Software triangles are viewport-clipped; GPU batching is not yet ported.

use crate::envelope::{EnvelopeSet, EnvelopeTimes};
use crate::graphics::{Camera, Framebuffer};
use crate::map_visual::{GroupTransform, VisualError};
use crate::texture::Texture;

pub const QUAD_BYTES: usize = 152;
pub const MAX_QUADS_PER_LAYER: usize = 32768;
pub const MAX_TOTAL_QUADS: usize = 65536;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuadVertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
    pub color: [u8; 4],
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapQuad {
    /// Original DDNet order is TL, TR, BL, BR.
    pub vertices: [QuadVertex; 4],
    pub pivot: [f32; 2],
    pub pos_env: i32,
    pub pos_env_offset: i32,
    pub color_env: i32,
    pub color_env_offset: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuadLayer {
    pub order: usize,
    pub foreground: bool,
    pub transform: GroupTransform,
    /// -1 in CMapItemLayerQuads is a solid-color (non-textured) quad.
    pub image: Option<usize>,
    pub quads: Vec<MapQuad>,
}

fn word(source: &[u8], byte_index: usize) -> Result<i32, VisualError> {
    let bytes: [u8; 4] = source.get(byte_index..byte_index + 4)
        .ok_or(VisualError::InvalidMap)?
        .try_into().map_err(|_| VisualError::InvalidMap)?;
    Ok(i32::from_le_bytes(bytes))
}

pub fn decode_quads(bytes: &[u8]) -> Result<Vec<MapQuad>, VisualError> {
    if !bytes.len().is_multiple_of(QUAD_BYTES)
        || bytes.len() / QUAD_BYTES > MAX_QUADS_PER_LAYER
    {
        return Err(VisualError::TooLarge);
    }
    let mut decoded = Vec::with_capacity(bytes.len() / QUAD_BYTES);
    for src in bytes.chunks_exact(QUAD_BYTES) {
        let mut verts = [QuadVertex {
            position: [0.0; 2],
            uv: [0.0; 2],
            color: [255; 4],
        }; 4];
        for (i, vertex) in verts.iter_mut().enumerate() {
            vertex.position = [
                word(src, i * 8)? as f32 / 1024.0,
                word(src, i * 8 + 4)? as f32 / 1024.0,
            ];
            for channel in 0..4 {
                vertex.color[channel] = word(src, 40 + i * 16 + channel * 4)?
                    .clamp(0, 255) as u8;
            }
            vertex.uv = [
                word(src, 104 + i * 8)? as f32 / 1024.0,
                word(src, 108 + i * 8)? as f32 / 1024.0,
            ];
        }
        decoded.push(MapQuad {
            vertices: verts,
            pivot: [
                word(src, 32)? as f32 / 1024.0,
                word(src, 36)? as f32 / 1024.0,
            ],
            pos_env: word(src, 136)?,
            pos_env_offset: word(src, 140)?,
            color_env: word(src, 144)?,
            color_env_offset: word(src, 148)?,
        });
    }
    Ok(decoded)
}

#[derive(Clone, Copy)]
struct ScreenVertex {
    x: f32,
    y: f32,
    uv: [f32; 2],
    color: [u8; 4],
}

fn edge(a: ScreenVertex, b: ScreenVertex, x: f32, y: f32) -> f32 {
    (x - a.x) * (b.y - a.y) - (y - a.y) * (b.x - a.x)
}

fn sample_color(
    vertices: [ScreenVertex; 3], weights: [f32; 3],
    texture: Option<&Texture>,
) -> ([u32; 3], u32) {
    let mut c = [0.0f32; 4];
    for (channel, value) in c.iter_mut().enumerate() {
        for j in 0..3 {
            *value += f32::from(vertices[j].color[channel]) * weights[j];
        }
    }
    let sample = texture.map(|image| {
        let u = (vertices[0].uv[0] * weights[0]
            + vertices[1].uv[0] * weights[1]
            + vertices[2].uv[0] * weights[2]).rem_euclid(1.0);
        let v = (vertices[0].uv[1] * weights[0]
            + vertices[1].uv[1] * weights[1]
            + vertices[2].uv[1] * weights[2]).rem_euclid(1.0);
        let tx = (u * image.width as f32) as usize;
        let ty = (v * image.height as f32) as usize;
        let offset = (ty * image.width as usize + tx) * 4;
        &image.rgba[offset..offset + 4]
    });
    let rgb = std::array::from_fn(|i| {
        (c[i].clamp(0.0, 255.0) as u32
            * u32::from(sample.map_or(255, |px| px[i])) + 127) / 255
    });
    let alpha = (c[3].clamp(0.0, 255.0) as u32
        * u32::from(sample.map_or(255, |px| px[3])) + 127) / 255;
    (rgb, alpha)
}

/// Draw a triangle with interpolated vertex colors and UVs, clipped to the
/// actual framebuffer. Shared diagonal pixels belong to only one triangle to
/// avoid a visible dark seam with semitransparent quads.
fn triangle(
    frame: &mut Framebuffer, verts: [ScreenVertex; 3],
    tex: Option<&Texture>, second: bool, clip: Option<[f32; 4]>,
) {
    let [a, b, c] = verts;
    let determinant = edge(a, b, c.x, c.y);
    if !determinant.is_finite() || determinant.abs() < 0.0001 { return; }
    let mut min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as usize;
    let mut max_x = a.x.max(b.x).max(c.x).ceil().min(frame.width as f32) as usize;
    let mut min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as usize;
    let mut max_y = a.y.max(b.y).max(c.y).ceil().min(frame.height as f32) as usize;
    if let Some([left, top, right, bottom]) = clip {
        if right <= left || bottom <= top { return; }
        min_x = min_x.max(left.round().max(0.0) as usize);
        min_y = min_y.max(top.round().max(0.0) as usize);
        max_x = max_x.min(right.round().max(0.0) as usize);
        max_y = max_y.min(bottom.round().max(0.0) as usize);
    }
    if min_x >= max_x || min_y >= max_y { return; }

    // Flat-colored, untextured geometry is common in DDNet backgrounds.
    // Compute its RGBA once per triangle instead of interpolating four
    // channels three times for each covered pixel.
    let flat_color = if tex.is_none()
        && a.color == b.color && b.color == c.color
    {
        Some((
            std::array::from_fn(|i| u32::from(a.color[i])),
            u32::from(a.color[3]),
        ))
    } else {
        None
    };

    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let w = [
                edge(b, c, px, py) / determinant,
                edge(c, a, px, py) / determinant,
                edge(a, b, px, py) / determinant,
            ];
            if w.iter().any(|&weight| weight < -0.000001) { continue; }
            // In triangles (TL, TR, BL) and (TR, BR, BL), the shared edge
            // is between TR and BL: w[0] == 0 in the first and w[1] == 0
            // in the second. Give shared pixels to triangle one.
            if second && w[1].abs() <= 0.000001 { continue; }
            let (rgb, alpha) = flat_color
                .unwrap_or_else(|| sample_color(verts, w, tex));
            if alpha == 0 { continue; }
            let index = y * frame.width as usize + x;
            let out = if alpha == 255 {
                rgb[0] << 16 | rgb[1] << 8 | rgb[2]
            } else {
                let old = frame.pixels[index];
                let prev = [(old >> 16) & 255, (old >> 8) & 255, old & 255];
                let blend = |i: usize| {
                    (rgb[i] * alpha + prev[i] * (255 - alpha) + 127) / 255
                };
                blend(0) << 16 | blend(1) << 8 | blend(2)
            };
            frame.pixels[index] = out;
        }
    }
}

/// Backwards-compatible static frame for map previews and unit tests.
pub fn draw_quads(
    layer: &QuadLayer, images: &[Texture],
    frame: &mut Framebuffer, camera: Camera,
) -> Result<usize, VisualError> {
    draw_quads_at(layer, images, frame, camera,
        &EnvelopeSet::default(), EnvelopeTimes::default())
}

/// Animate position (offset + rotation) and vertex colors from original
/// DDNet map envelopes before clipping the quad to the viewport.
pub fn draw_quads_at(
    layer: &QuadLayer, images: &[Texture],
    frame: &mut Framebuffer, camera: Camera,
    envelopes: &EnvelopeSet, times: EnvelopeTimes,
) -> Result<usize, VisualError> {
    let tex = match layer.image {
        Some(index) => Some(images.get(index).ok_or(VisualError::MissingImage)?),
        None => None,
    };
    let cx = camera.center.x * layer.transform.parallax_x / 100.0
        - layer.transform.offset_x;
    let cy = camera.center.y * layer.transform.parallax_y / 100.0
        - layer.transform.offset_y;
    let mut count = 0;
    let clip = layer.transform.screen_clip(camera, frame.width as f32,
        frame.height as f32);
    for quad in &layer.quads {
        let pos = envelopes.evaluate(quad.pos_env, quad.pos_env_offset,
            times, [0.0; 4]);
        let tint = envelopes.evaluate(quad.color_env, quad.color_env_offset,
            times, [1.0; 4]);
        let angle = pos[2].to_radians();
        let (sin, cos) = angle.sin_cos();
        let v = quad.vertices.map(|vertex| {
            let rel_x = vertex.position[0] - quad.pivot[0];
            let rel_y = vertex.position[1] - quad.pivot[1];
            let x = if quad.pos_env >= 0 {
                quad.pivot[0] + rel_x * cos - rel_y * sin + pos[0]
            } else {
                vertex.position[0]
            };
            let y = if quad.pos_env >= 0 {
                quad.pivot[1] + rel_x * sin + rel_y * cos + pos[1]
            } else {
                vertex.position[1]
            };
            ScreenVertex {
                x: (x - cx) * camera.scale + frame.width as f32 / 2.0,
                y: (y - cy) * camera.scale + frame.height as f32 / 2.0,
                uv: vertex.uv,
                color: std::array::from_fn(|i| {
                    (f32::from(vertex.color[i]) * tint[i]).clamp(0.0, 255.0) as u8
                }),
            }
        });
        if v.iter().any(|v| !v.x.is_finite() || !v.y.is_finite()) {
            continue;
        }
        let min_x = v.iter().map(|v| v.x).fold(f32::INFINITY, f32::min);
        let max_x = v.iter().map(|v| v.x).fold(f32::NEG_INFINITY, f32::max);
        let min_y = v.iter().map(|v| v.y).fold(f32::INFINITY, f32::min);
        let max_y = v.iter().map(|v| v.y).fold(f32::NEG_INFINITY, f32::max);
        if min_x >= frame.width as f32 || max_x < 0.0
            || min_y >= frame.height as f32 || max_y < 0.0
        {
            continue;
        }
        triangle(frame, [v[0], v[1], v[2]], tex, false, clip);
        triangle(frame, [v[1], v[3], v[2]], tex, true, clip);
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::Vec2;

    fn solid_quad(color: [u8; 4]) -> MapQuad {
        let vertex = |x, y, u, v| QuadVertex {
            position: [x, y], uv: [u, v], color,
        };
        MapQuad {
            vertices: [
                vertex(0.0, 0.0, 0.0, 0.0),
                vertex(16.0, 0.0, 1.0, 0.0),
                vertex(0.0, 16.0, 0.0, 1.0),
                vertex(16.0, 16.0, 1.0, 1.0),
            ],
            pivot: [8.0, 8.0], pos_env: -1, pos_env_offset: 0,
            color_env: -1, color_env_offset: 0,
        }
    }

    fn layer(image: Option<usize>, quad: MapQuad) -> QuadLayer {
        QuadLayer {
            order: 0, foreground: false,
            transform: GroupTransform::default(),
            image, quads: vec![quad],
        }
    }

    #[test]
    fn original_group_scissor_limits_cpu_quad_rasterization() {
        let mut solid = layer(None, solid_quad([255, 0, 0, 255]));
        solid.transform.clip = Some(crate::map_visual::GroupClip {
            x: 4.0, y: 3.0, width: 7.0, height: 9.0,
        });
        let camera = Camera { center: Vec2::new(8.0, 8.0), scale: 1.0 };
        let mut frame = Framebuffer::new(16,16).unwrap();
        frame.clear(0);
        draw_quads(&solid, &[], &mut frame, camera).unwrap();
        assert_eq!(frame.pixel(3,5), Some(0));
        assert_eq!(frame.pixel(4,5), Some(0xff0000));
        assert_eq!(frame.pixel(10,11), Some(0xff0000));
        assert_eq!(frame.pixel(11,11), Some(0));
        assert_eq!(frame.pixel(5,12), Some(0));
    }

    #[test]
    fn position_and_color_envelopes_animate_quad_at_server_time() {
        use crate::envelope::{Envelope, EnvelopePoint};
        let mut quad = solid_quad([255, 0, 0, 255]);
        quad.pos_env = 0;
        quad.color_env = 1;
        let envelopes = EnvelopeSet { envelopes: vec![
            Envelope {
                channels: 3, synchronized: true,
                points: vec![
                    EnvelopePoint { time_ms: 0, curve: 1,
                        values: [0, 0, 0, 0], bezier: None },
                    EnvelopePoint { time_ms: 1000, curve: 1,
                        values: [16 * 1024, 0, 0, 0], bezier: None },
                ],
            },
            Envelope {
                channels: 4, synchronized: true,
                points: vec![
                    EnvelopePoint { time_ms: 0, curve: 1,
                        values: [1024; 4], bezier: None },
                    EnvelopePoint { time_ms: 1000, curve: 1,
                        values: [0, 1024, 1024, 1024], bezier: None },
                ],
            },
        ] };
        let map = layer(None, quad);
        let camera = Camera { center: Vec2::new(16.0, 8.0), scale: 1.0 };
        let mut frame = Framebuffer::new(32, 16).unwrap();
        frame.clear(0);
        draw_quads_at(&map, &[], &mut frame, camera, &envelopes,
            EnvelopeTimes { game_ms: 0.0, local_ms: 0.0 }).unwrap();
        assert_eq!(frame.pixel(4, 4), Some(0xff0000));
        frame.clear(0);
        draw_quads_at(&map, &[], &mut frame, camera, &envelopes,
            EnvelopeTimes { game_ms: 500.0, local_ms: 0.0 }).unwrap();
        assert_eq!(frame.pixel(4, 4), Some(0));
        assert_eq!(frame.pixel(14, 4), Some(0x7f0000));
    }

    #[test]
    fn raw_cquad_layout_is_original_152_bytes_fixed_point_and_colors() {
        let mut raw = vec![0u8; QUAD_BYTES];
        for (i, (x, y)) in [(0, 0), (16, 0), (0, 16), (16, 16), (8, 8)]
            .into_iter().enumerate()
        {
            raw[i * 8..i * 8 + 4].copy_from_slice(&(x * 1024i32).to_le_bytes());
            raw[i * 8 + 4..i * 8 + 8].copy_from_slice(&(y * 1024i32).to_le_bytes());
        }
        for i in 0..4 {
            for channel in 0..4 {
                raw[40 + 16 * i + 4 * channel..44 + 16 * i + 4 * channel]
                    .copy_from_slice(&(if channel == 0 { 100i32 } else { 255 }).to_le_bytes());
            }
            raw[104 + i * 8..108 + i * 8]
                .copy_from_slice(&((i % 2) as i32 * 1024).to_le_bytes());
        }
        raw[136..140].copy_from_slice(&(-1i32).to_le_bytes());
        raw[144..148].copy_from_slice(&(-1i32).to_le_bytes());
        let decoded = decode_quads(&raw).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].vertices[3].position, [16.0, 16.0]);
        assert_eq!(decoded[0].vertices[0].color, [100, 255, 255, 255]);
        assert_eq!(decoded[0].vertices[3].uv, [1.0, 0.0]);
        assert_eq!(decoded[0].pivot, [8.0, 8.0]);
        assert!(decode_quads(&raw[1..]).is_err());
        assert!(decode_quads(&vec![0; QUAD_BYTES * (MAX_QUADS_PER_LAYER + 1)]).is_err());
    }

    #[test]
    fn solid_and_transparent_quads_rasterize_without_diagonal_seam() {
        let camera = Camera { center: Vec2::new(8.0, 8.0), scale: 1.0 };
        let mut frame = Framebuffer::new(16, 16).unwrap();
        frame.clear(0x000000);
        assert_eq!(draw_quads(&layer(None, solid_quad([255, 0, 0, 255])),
            &[], &mut frame, camera), Ok(1));
        assert_eq!(frame.pixel(6, 7), Some(0xff0000));
        frame.clear(0);
        draw_quads(&layer(None, solid_quad([255, 0, 0, 128])),
            &[], &mut frame, camera).unwrap();
        assert_eq!(frame.pixel(6, 7), Some(0x800000));
        assert_eq!(frame.pixel(8, 8), Some(0x800000));
    }

    #[test]
    fn textured_quads_sample_atlas_and_cull_offscreen() {
        let camera = Camera { center: Vec2::new(8.0, 8.0), scale: 1.0 };
        let image = Texture::from_rgba(2, 2, vec![
            255, 0, 0, 255, 0, 255, 0, 255,
            0, 0, 255, 255, 255, 255, 255, 255,
        ]).unwrap();
        let mut frame = Framebuffer::new(16, 16).unwrap();
        let result = draw_quads(&layer(Some(0), solid_quad([255; 4])),
            &[image], &mut frame, camera);
        assert_eq!(result, Ok(1));
        assert_eq!(frame.pixel(3, 3), Some(0xff0000));
        assert_eq!(frame.pixel(12, 3), Some(0x00ff00));
        let mut far = solid_quad([255, 0, 0, 255]);
        for vertex in &mut far.vertices {
            vertex.position[0] += 50_000.0;
        }
        frame.clear(0);
        assert_eq!(draw_quads(&layer(None, far), &[], &mut frame, camera), Ok(0));
        assert!(frame.pixels.iter().all(|&pixel| pixel == 0));
    }

    #[test]
    fn vertex_gradient_and_parallax_affect_quad_pixels() {
        let mut quad = solid_quad([255, 255, 255, 255]);
        quad.vertices[0].color = [255, 0, 0, 255];
        quad.vertices[1].color = [0, 255, 0, 255];
        quad.vertices[2].color = [0, 0, 255, 255];
        let mut scene = layer(None, quad);
        let camera = Camera { center: Vec2::new(8.0, 8.0), scale: 1.0 };
        let mut frame = Framebuffer::new(16, 16).unwrap();
        draw_quads(&scene, &[], &mut frame, camera).unwrap();
        let top_left = frame.pixel(1, 1).unwrap();
        assert!((top_left >> 16) > ((top_left >> 8) & 255));
        scene.transform.parallax_x = 0.0;
        let shift = Camera { center: Vec2::new(100.0, 8.0), scale: 1.0 };
        frame.clear(0);
        draw_quads(&scene, &[], &mut frame, shift).unwrap();
        // Zero X parallax keeps world position fixed as the camera moves;
        // compared with the earlier centered view, the quad shifts right.
        assert_eq!(frame.pixel(1, 1), Some(0));
        assert_ne!(frame.pixel(10, 1), Some(0));
    }
}
