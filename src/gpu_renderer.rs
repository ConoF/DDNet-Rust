//! Hardware-accelerated DDNet map presentation using wgpu 0.20.
//!
//! The same minifb window retains input/OS handling; Vulkan/DX12/Metal/GLES
//! presents directly to the native surface. Visible tile/quad geometry is
//! uploaded once per frame, and map texture atlases only on map changes.
//! Only unsupported tee assets use the CPU fallback. HUD glyphs and panels
//! are native GPU geometry; original scalable DDNet fonts remain pending.

use minifb::Window;

use crate::envelope::EnvelopeTimes;
use crate::gpu_scene::{build_gpu_scene_into, GpuBatch, GpuScene, GpuVertex, VERTEX_STRIDE};
use crate::gpu_tees::{build_gpu_tees, GpuTeeScene, SpriteTextureSlots};
use crate::gpu_hud::{
    font_atlas_rgba, GpuHudScene, FONT_WIDTH, FONT_HEIGHT,
    HUD_FONT_SLOT, HUD_SOLID_SLOT,
};
use crate::sprite_render::{valid_sheet, GRID_SKIN, GRID_GAME};
use crate::snapshot_delta::SnapshotState;
use crate::texture::Texture;
use crate::online_ui::HudCanvas;
use crate::graphics::{Camera, Framebuffer, BACKGROUND};
use crate::map_visual::VisualMap;

pub const OVERLAY_KEY: u32 = 0x010203;
const INITIAL_VERTICES: usize = 4096;
const WHITE: [u8; 4] = [255, 255, 255, 255];
const OVERLAY_VERTEX_COUNT: u32 = 6;

struct ImageBind {
    _texture: wgpu::Texture,
    group: wgpu::BindGroup,
}

/// Native sprite context for one frame. The legacy CPU overlay remains an
/// option for missing skins or unsupported visual map data.
pub struct TeeFrame<'a> {
    pub state: Option<&'a SnapshotState>,
    pub tick: u64,
    pub skin: Option<&'a Texture>,
    pub game: Option<&'a Texture>,
}

pub struct GpuRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    images: Vec<ImageBind>,
    map_image_count: usize,
    skin_ready: bool,
    last_sprite_vertices: usize,
    last_hud_vertices: usize,
    overlay_tee: ImageBind,
    vertices: wgpu::Buffer,
    capacity: usize,
    overlay_vertices: wgpu::Buffer,
    // Reused every frame; prevents large temporary CPU upload allocations.
    vertex_upload: Vec<u8>,
    scene_cache: GpuScene,
}

impl GpuRenderer {
    /// Safety invariant: the minifb Window owns the native window/display
    /// handles and must outlive GpuRenderer. The GPU is constructed *after*
    /// Window and dropped *before* Window by the online game loop.
    pub fn new(window: &Window, width: u32, height: u32) -> Result<Self, String> {
        let instance = wgpu::Instance::default();
        let raw = unsafe {
            wgpu::SurfaceTargetUnsafe::from_window(window)
        }.map_err(|e| format!("window GPU handles: {e}"))?;
        let surface = unsafe { instance.create_surface_unsafe(raw) }
            .map_err(|e| format!("GPU surface: {e}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        })).ok_or_else(|| String::from("No GPU adapter compatible with the native window"))?;
        let info = adapter.get_info();
        eprintln!("DDNet-Rust GPU renderer: {} ({:?}, {:?})",
            info.name, info.backend, info.device_type);

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("DDNet-Rust GPU device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )).map_err(|e| format!("GPU device: {e}"))?;
        let mut config = surface.get_default_config(&adapter, width, height)
            .ok_or_else(|| String::from("Surface does not support DDNet framebuffer format"))?;
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("DDNet sampled map atlas"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("DDNet pixel nearest atlas sampling"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("DDNet textured vertex/fragment shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu_shader.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("DDNet map pipeline"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("DDNet-Rust batched sprite/tile/quad pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader, entry_point: "vs_main",
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: VERTEX_STRIDE as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0, shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8, shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Unorm8x4,
                            offset: 16, shader_location: 2,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader, entry_point: "fs_main",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("DDNet dynamic map geometry"),
            size: (INITIAL_VERTICES * VERTEX_STRIDE) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let overlay_vertices = {
            let corners: [GpuVertex; 6] = [
                GpuVertex { clip: [-1.0, 1.0], uv: [0.0,0.0], color: WHITE },
                GpuVertex { clip: [1.0, 1.0], uv: [1.0,0.0], color: WHITE },
                GpuVertex { clip: [-1.0, -1.0], uv: [0.0,1.0], color: WHITE },
                GpuVertex { clip: [1.0, 1.0], uv: [1.0,0.0], color: WHITE },
                GpuVertex { clip: [1.0,-1.0], uv: [1.0,1.0], color: WHITE },
                GpuVertex { clip: [-1.0,-1.0], uv: [0.0,1.0], color: WHITE },
            ];
            let scene = GpuScene {
                vertices: corners.to_vec(), ..Default::default()
            };
            use wgpu::util::DeviceExt;
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("DDNet onscreen overlay vertices"),
                contents: &scene.vertex_bytes(),
                usage: wgpu::BufferUsages::VERTEX,
            })
        };
        let overlay_tee = Self::make_texture(
            &device, &queue, &layout, &sampler,
            (width, height), &vec![0; (width * height * 4) as usize],
            "DDNet tee overlay",
        );
        Ok(Self {
            surface, device, queue, config, pipeline, layout, sampler,
            images: Vec::new(), map_image_count: 0, skin_ready: false,
            last_sprite_vertices: 0, last_hud_vertices: 0,
            overlay_tee,
            vertices, capacity: INITIAL_VERTICES, overlay_vertices,
            vertex_upload: Vec::with_capacity(INITIAL_VERTICES * VERTEX_STRIDE),
            scene_cache: GpuScene::default(),
        })
    }

    fn make_texture(
        device: &wgpu::Device, queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler,
        (width, height): (u32, u32), rgba: &[u8], label: &'static str,
    ) -> ImageBind {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::ImageDataLayout {
                offset: 0, bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label), layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0, resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1, resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        ImageBind { _texture: texture, group }
    }

    /// Upload atlases only when the actual map or visual assets change.
    /// Slots: map images (0..N), white (N), skin (N+1), game (N+2),
    /// single-channel-like RGBA font atlas (N+3).
    /// Keeping those indices stable avoids texture binding churn per frame.
    pub fn sync_assets(
        &mut self, map: Option<&VisualMap>,
        skin: Option<&Texture>, game: Option<&Texture>,
    ) {
        self.images.clear();
        self.map_image_count = map.map_or(0, |m| m.images.len());
        if let Some(map) = map {
            for image in &map.images {
                self.images.push(Self::make_texture(
                    &self.device, &self.queue, &self.layout, &self.sampler,
                    (image.width, image.height), &image.rgba, "DDNet map atlas",
                ));
            }
        }
        self.images.push(Self::make_texture(
            &self.device, &self.queue, &self.layout, &self.sampler,
            (1, 1), &WHITE, "DDNet solid white quad texture",
        ));
        self.skin_ready = skin.is_some_and(|s| valid_sheet(s, GRID_SKIN));
        let skin = skin.filter(|s| valid_sheet(s, GRID_SKIN));
        let game = game.filter(|s| valid_sheet(s, GRID_GAME));
        for (image, label) in [
            (skin, "DDNet default skin atlas"),
            (game, "DDNet weapons and hook atlas"),
        ] {
            let (dimensions, bytes) = if let Some(image) = image {
                ((image.width, image.height), image.rgba.as_slice())
            } else {
                ((1, 1), &[0u8; 4][..])
            };
            self.images.push(Self::make_texture(
                &self.device, &self.queue, &self.layout, &self.sampler,
                dimensions, bytes, label,
            ));
        }
        let glyphs = font_atlas_rgba();
        self.images.push(Self::make_texture(
            &self.device, &self.queue, &self.layout, &self.sampler,
            (FONT_WIDTH, FONT_HEIGHT), &glyphs, "DDNet HUD glyph atlas",
        ));
    }

    /// True when a compatible skin atlas was loaded. The caller may bypass
    /// CPU sprite rasterization if the map itself also has visual layers.
    pub fn native_tees_ready(&self) -> bool {
        self.skin_ready
    }

    /// Non-zero only if the most recent GPU frame actually submitted sprite
    /// geometry. Used by the real-map smoke test, not an estimated FPS metric.
    pub fn last_native_tee_vertices(&self) -> usize {
        self.last_sprite_vertices
    }

    /// Native HUD geometry submitted in the last rendered frame.
    pub fn last_native_hud_vertices(&self) -> usize {
        self.last_hud_vertices
    }

    fn upload_overlay(queue: &wgpu::Queue, image: &wgpu::Texture, frame: &Framebuffer) {
        let data = rgba_overlay(frame);
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: image, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(frame.width * 4),
                rows_per_image: Some(frame.height),
            },
            wgpu::Extent3d {
                width: frame.width, height: frame.height, depth_or_array_layers: 1,
            },
        );
    }

    fn draw_batches<'a>(
        pass: &mut wgpu::RenderPass<'a>,
        batches: &[GpuBatch], images: &'a [ImageBind],
    ) {
        for batch in batches {
            if let Some(image) = images.get(batch.texture) {
                pass.set_bind_group(0, &image.group, &[]);
                pass.draw(batch.start..batch.start + batch.count, 0..1);
            }
        }
    }

    /// Presents GPU background, GPU tees (or CPU fallback), GPU foreground
    /// and native GPU glyph/panel geometry. No fullscreen HUD upload occurs.
    pub fn render(
        &mut self, map: Option<&VisualMap>, camera: Camera, times: EnvelopeTimes,
        tee: TeeFrame<'_>, tees: &Framebuffer, hud: &GpuHudScene,
    ) -> Result<(), String> {
        if tees.width != self.config.width || tees.height != self.config.height
            || hud.dimensions() != (self.config.width as usize, self.config.height as usize)
        {
            return Err("GPU frame dimensions changed without surface reconfiguration".into());
        }
        // Reuse map geometry allocations even when the camera moves and
        // envelopes require a full rebuild of the visible triangles.
        let mut scene = std::mem::take(&mut self.scene_cache);
        if let Some(map) = map {
            build_gpu_scene_into(map, camera, times,
                self.config.width, self.config.height, &mut scene)
                .map_err(|e| format!("GPU scene geometry: {e:?}"))?;
        } else {
            scene.vertices.clear();
            scene.background.clear();
            scene.foreground.clear();
        }
        // For maps with graphics and usable skins, all tee sprites are now
        // actual native vertices. No tee framebuffer or full-screen tee
        // texture is uploaded in this path.
        let native = self.skin_ready && map.is_some();
        let mut sprite = if native {
            tee.state.map(|state| build_gpu_tees(
                state, camera, tee.tick,
                (self.config.width, self.config.height),
                tee.skin, tee.game, SpriteTextureSlots {
                    white: self.map_image_count,
                    skin: self.map_image_count + 1,
                    game: self.map_image_count + 2,
                },
            )).transpose().map_err(|e| format!("GPU tee geometry: {e:?}"))?
                .unwrap_or_default()
        } else {
            GpuTeeScene::default()
        };
        self.last_sprite_vertices = sprite.vertices.len();
        let tee_base = u32::try_from(scene.vertices.len())
            .map_err(|_| String::from("GPU tee vertex offset overflow"))?;
        for batch in &mut sprite.batches {
            batch.start = batch.start.checked_add(tee_base)
                .ok_or("GPU tee batch offset overflow")?;
        }
        scene.vertices.extend(sprite.vertices);

        self.last_hud_vertices = hud.vertices.len();
        let hud_base = u32::try_from(scene.vertices.len())
            .map_err(|_| String::from("GPU HUD vertex offset overflow"))?;
        let mut hud_batches = Vec::with_capacity(hud.batches.len());
        for batch in &hud.batches {
            let index = match batch.texture {
                HUD_SOLID_SLOT => self.map_image_count,
                HUD_FONT_SLOT => self.map_image_count + 3,
                _ => return Err("unknown GPU HUD atlas slot".to_owned()),
            };
            hud_batches.push(GpuBatch {
                texture: index,
                start: batch.start.checked_add(hud_base)
                    .ok_or("GPU HUD batch offset overflow")?,
                count: batch.count,
            });
        }
        scene.vertices.extend_from_slice(&hud.vertices);
        if scene.vertices.len() > crate::gpu_scene::MAX_FRAME_VERTICES {
            return Err("GPU map, tee and HUD vertex count exceeds frame limit".into());
        }
        scene.write_vertex_bytes(&mut self.vertex_upload);
        if scene.vertices.len() > self.capacity {
            self.capacity = scene.vertices.len().next_power_of_two();
            self.vertices = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("DDNet growing GPU geometry"),
                size: (self.capacity * VERTEX_STRIDE) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !self.vertex_upload.is_empty() {
            self.queue.write_buffer(&self.vertices, 0, &self.vertex_upload);
        }
        if !native {
            Self::upload_overlay(&self.queue, &self.overlay_tee._texture, tees);
        }
        let output = match self.surface.get_current_texture() {
            Ok(output) => output,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                match self.surface.get_current_texture() {
                    Ok(output) => output,
                    // Temporary compositor stalls must not disconnect the game.
                    Err(wgpu::SurfaceError::Timeout) => return Ok(()),
                    Err(error) => return Err(format!("GPU surface recovery: {error}")),
                }
            }
            // A one-frame timeout is recoverable; the next frame retries.
            Err(wgpu::SurfaceError::Timeout) => return Ok(()),
            Err(error) => return Err(format!("GPU presentation: {error}")),
        };
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("DDNet GPU frame encoder"),
        });
        {
            let background = BACKGROUND;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("DDNet maps, tees and HUD"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view, resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from((background >> 16) & 255) / 255.0,
                            g: f64::from((background >> 8) & 255) / 255.0,
                            b: f64::from(background & 255) / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None, occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            Self::draw_batches(&mut pass, &scene.background, &self.images);
            if native {
                Self::draw_batches(&mut pass, &sprite.batches, &self.images);
            } else {
                pass.set_vertex_buffer(0, self.overlay_vertices.slice(..));
                pass.set_bind_group(0, &self.overlay_tee.group, &[]);
                pass.draw(0..OVERLAY_VERTEX_COUNT, 0..1);
            }
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            Self::draw_batches(&mut pass, &scene.foreground, &self.images);
            Self::draw_batches(&mut pass, &hud_batches, &self.images);
        }
        self.queue.submit(Some(encoder.finish()));
        output.present();
        self.scene_cache = scene;
        Ok(())
    }
}

/// Convert the legacy RGB tee fallback into an alpha overlay.
 /// This is only used if native tee atlas assets are missing.
pub fn rgba_overlay(frame: &Framebuffer) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(frame.pixels.len() * 4);
    for &pixel in &frame.pixels {
        if pixel == OVERLAY_KEY {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            rgba.extend_from_slice(&[
                ((pixel >> 16) & 255) as u8,
                ((pixel >> 8) & 255) as u8,
                (pixel & 255) as u8,
                255,
            ]);
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_chroma_key_is_transparent_and_real_tee_pixels_are_opaque() {
        let mut frame = Framebuffer::new(2, 1).unwrap();
        frame.pixels = vec![OVERLAY_KEY, 0x00ff83];
        assert_eq!(rgba_overlay(&frame), vec![
            0,0,0,0, 0,255,131,255,
        ]);
    }
}
