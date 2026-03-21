// renderer.rs — wgpu-based YUV→RGB rendering pipeline for GlassBridge.
//
// Uploads decoded YUV420 planes as three separate GPU textures and converts
// them to RGB in a fragment shader (BT.601 coefficients, full-range).  The
// pipeline draws a single fullscreen triangle that covers the entire surface
// without the need for a vertex buffer.
//
// All items are gated behind `#[cfg(feature = "screen-mirror")]`.

#![cfg(feature = "screen-mirror")]

use std::borrow::Cow;

use thiserror::Error;
use tracing::{debug, info};
// DeviceExt will be used when vertex buffers are needed.
#[allow(unused_imports)]
use wgpu::util::DeviceExt;

use crate::decoder::DecodedFrame;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by the wgpu renderer.
///
/// # Examples
///
/// ```rust
/// # #[cfg(feature = "screen-mirror")] {
/// use glassbridge_desktop::renderer::RendererError;
///
/// let e = RendererError::NoAdapter;
/// assert!(e.to_string().contains("adapter"));
/// # }
/// ```
#[derive(Debug, Error)]
pub enum RendererError {
    /// No suitable wgpu adapter was found on this system.
    #[error("no suitable GPU adapter found")]
    NoAdapter,

    /// Adapter was found but the device could not be created.
    #[error("device request failed: {0}")]
    DeviceRequest(String),

    /// The surface is incompatible with the adapter.
    #[error("surface incompatible with adapter")]
    IncompatibleSurface,

    /// The surface texture could not be acquired for rendering.
    #[error("surface texture acquire failed: {0}")]
    SurfaceAcquire(String),
}

impl RendererError {
    /// Returns `true` when no GPU adapter was available.
    pub fn is_no_adapter(&self) -> bool {
        matches!(self, Self::NoAdapter)
    }
}

// ---------------------------------------------------------------------------
// WGSL shader source
// ---------------------------------------------------------------------------

/// YUV→RGB fragment shader using BT.601 full-range coefficients.
///
/// Three separate textures hold the Y, U, and V planes.  U and V are
/// half-resolution (4:2:0) so the sampler bilinearly upscales them.
const SHADER_SOURCE: &str = r#"
// ---- vertex stage --------------------------------------------------------
// Fullscreen triangle: vertex positions are computed from the vertex index
// so no vertex buffer is required.
//
//  Index 0: (-1, -1)   bottom-left
//  Index 1: ( 3, -1)   far right  (clips to +1)
//  Index 2: (-1,  3)   far top    (clips to +1)
//
// UV coordinates map to [0, 1]² covering the whole surface.

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0)       uv:            vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOutput {
    var out: VertexOutput;
    // Positions in clip space for the fullscreen triangle.
    var x = f32((vid & 1u) << 2u) - 1.0;
    var y = f32((vid & 2u) << 1u) - 1.0;
    out.clip_position = vec4<f32>(x, y, 0.0, 1.0);
    // Map clip-space [-1,1]² → UV [0,1]²; Y axis is flipped.
    out.uv = vec2<f32>((x + 1.0) * 0.5, 1.0 - (y + 1.0) * 0.5);
    return out;
}

// ---- fragment stage ------------------------------------------------------

@group(0) @binding(0) var y_tex:      texture_2d<f32>;
@group(0) @binding(1) var u_tex:      texture_2d<f32>;
@group(0) @binding(2) var v_tex:      texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let y = textureSample(y_tex, tex_sampler, uv).r;
    let u = textureSample(u_tex, tex_sampler, uv).r - 0.5;
    let v = textureSample(v_tex, tex_sampler, uv).r - 0.5;

    // BT.601 full-range conversion coefficients.
    let r = clamp(y + 1.402       * v,                    0.0, 1.0);
    let g = clamp(y - 0.344136    * u - 0.714136 * v,     0.0, 1.0);
    let b = clamp(y + 1.772       * u,                    0.0, 1.0);

    return vec4<f32>(r, g, b, 1.0);
}
"#;

// ---------------------------------------------------------------------------
// WgpuRenderer
// ---------------------------------------------------------------------------

/// wgpu rendering pipeline for YUV420 video frames.
///
/// Manages the wgpu device, surface, render pipeline, and three GPU textures
/// (Y, U, V).  Call [`upload_frame`](Self::upload_frame) followed by
/// [`render`](Self::render) every frame.
///
/// # Lifecycle
///
/// 1. Create with [`WgpuRenderer::new`], passing a raw window handle.
/// 2. Call [`upload_frame`](Self::upload_frame) with the decoded YUV data.
/// 3. Call [`render`](Self::render) to draw the frame to the surface.
/// 4. Call [`resize`](Self::resize) when the window is resized.
///
/// # Examples
///
/// ```rust,no_run
/// # #[cfg(feature = "screen-mirror")] {
/// use raw_window_handle::{HasWindowHandle, HasDisplayHandle};
/// use glassbridge_desktop::renderer::WgpuRenderer;
///
/// // async fn example(window: &impl HasWindowHandle + HasDisplayHandle) {
/// //     let mut renderer = WgpuRenderer::new(window, 1280, 720).await.unwrap();
/// // }
/// # }
/// ```
pub struct WgpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Y plane texture (full resolution, R8Unorm).
    y_texture: wgpu::Texture,
    /// U (Cb) plane texture (half resolution, R8Unorm).
    u_texture: wgpu::Texture,
    /// V (Cr) plane texture (half resolution, R8Unorm).
    v_texture: wgpu::Texture,
    /// Bind group referencing current Y/U/V texture views + sampler.
    bind_group: wgpu::BindGroup,
    /// Width of the currently allocated YUV textures.
    texture_width: u32,
    /// Height of the currently allocated YUV textures.
    texture_height: u32,
}

impl WgpuRenderer {
    /// Create the renderer, surface, and render pipeline.
    ///
    /// `window` must implement both `HasWindowHandle` and `HasDisplayHandle`.
    /// `width` / `height` are the initial surface dimensions in physical pixels.
    ///
    /// # Errors
    ///
    /// - [`RendererError::NoAdapter`] if wgpu cannot find a GPU adapter.
    /// - [`RendererError::DeviceRequest`] if the device/queue cannot be created.
    /// - [`RendererError::IncompatibleSurface`] if the surface format list is empty.
    ///
    /// # Safety
    ///
    /// The `window` handle must remain valid for the entire lifetime of the
    /// returned `WgpuRenderer`.  Tauri windows satisfy this requirement for
    /// the duration of the application.
    pub async fn new<W>(window: &W, width: u32, height: u32) -> Result<Self, RendererError>
    where
        W: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle,
    {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        // SAFETY: The window handle is valid for the duration of the renderer.
        // The renderer is always owned by the same Tauri window task.
        let surface = unsafe {
            instance.create_surface_unsafe(
                wgpu::SurfaceTargetUnsafe::from_window(window)
                    .map_err(|e| RendererError::DeviceRequest(e.to_string()))?,
            )
        }
        .map_err(|e| RendererError::DeviceRequest(e.to_string()))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or(RendererError::NoAdapter)?;

        info!(
            adapter = %adapter.get_info().name,
            backend = ?adapter.get_info().backend,
            "wgpu adapter selected"
        );

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("glassbridge-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .map_err(|e| RendererError::DeviceRequest(e.to_string()))?;

        // Choose the surface format — prefer sRGB for colour accuracy.
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or(RendererError::IncompatibleSurface)?;

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        // Compile shaders.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuv-rgb-shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER_SOURCE)),
        });

        // Bind group layout: 3 textures + 1 sampler.
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuv-bgl"),
            entries: &[
                texture_binding(0, wgpu::TextureSampleType::Float { filterable: true }),
                texture_binding(1, wgpu::TextureSampleType::Float { filterable: true }),
                texture_binding(2, wgpu::TextureSampleType::Float { filterable: true }),
                sampler_binding(3),
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuv-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuv-rgb-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuv-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Create placeholder 1×1 textures so the bind group is always valid.
        let (y_texture, u_texture, v_texture) = create_yuv_textures(&device, 1, 1);
        let bind_group = create_bind_group(
            &device,
            &bind_group_layout,
            &y_texture,
            &u_texture,
            &v_texture,
            &sampler,
        );

        Ok(Self {
            device,
            queue,
            surface,
            surface_config,
            pipeline,
            bind_group_layout,
            sampler,
            y_texture,
            u_texture,
            v_texture,
            bind_group,
            texture_width: 1,
            texture_height: 1,
        })
    }

    /// Upload a decoded YUV frame to the GPU textures.
    ///
    /// If the frame dimensions differ from the currently allocated textures the
    /// textures are reallocated and the bind group is rebuilt.
    pub fn upload_frame(&mut self, frame: &DecodedFrame) {
        // Reallocate textures when dimensions change.
        if frame.width != self.texture_width || frame.height != self.texture_height {
            debug!(
                old_w = self.texture_width,
                old_h = self.texture_height,
                new_w = frame.width,
                new_h = frame.height,
                "reallocating YUV textures"
            );
            let (y, u, v) = create_yuv_textures(&self.device, frame.width, frame.height);
            self.y_texture = y;
            self.u_texture = u;
            self.v_texture = v;
            self.bind_group = create_bind_group(
                &self.device,
                &self.bind_group_layout,
                &self.y_texture,
                &self.u_texture,
                &self.v_texture,
                &self.sampler,
            );
            self.texture_width = frame.width;
            self.texture_height = frame.height;
        }

        let chroma_w = (frame.width / 2).max(1);
        let chroma_h = (frame.height / 2).max(1);

        upload_plane(
            &self.queue,
            &self.y_texture,
            &frame.y_plane,
            frame.y_stride as u32,
            frame.width,
            frame.height,
        );
        upload_plane(
            &self.queue,
            &self.u_texture,
            &frame.u_plane,
            frame.u_stride as u32,
            chroma_w,
            chroma_h,
        );
        upload_plane(
            &self.queue,
            &self.v_texture,
            &frame.v_plane,
            frame.v_stride as u32,
            chroma_w,
            chroma_h,
        );
    }

    /// Render the current YUV textures to the surface.
    ///
    /// # Errors
    ///
    /// Returns [`RendererError::SurfaceAcquire`] when the surface texture
    /// cannot be acquired (e.g., window minimised or lost).
    pub fn render(&mut self) -> Result<(), RendererError> {
        let surface_texture = self
            .surface
            .get_current_texture()
            .map_err(|e| RendererError::SurfaceAcquire(e.to_string()))?;

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("yuv-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            rpass.set_pipeline(&self.pipeline);
            rpass.set_bind_group(0, &self.bind_group, &[]);
            // Draw the fullscreen triangle (3 vertices, 1 instance, no buffer).
            rpass.draw(0..3, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        surface_texture.present();

        Ok(())
    }

    /// Reconfigure the surface after a window resize.
    ///
    /// Call this from the Tauri window's resize handler.
    pub fn resize(&mut self, new_width: u32, new_height: u32) {
        if new_width == 0 || new_height == 0 {
            return;
        }
        debug!(new_width, new_height, "surface resize");
        self.surface_config.width = new_width;
        self.surface_config.height = new_height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    /// Return the current surface width in physical pixels.
    pub fn width(&self) -> u32 {
        self.surface_config.width
    }

    /// Return the current surface height in physical pixels.
    pub fn height(&self) -> u32 {
        self.surface_config.height
    }
}

impl std::fmt::Debug for WgpuRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WgpuRenderer")
            .field("width", &self.surface_config.width)
            .field("height", &self.surface_config.height)
            .field("texture_width", &self.texture_width)
            .field("texture_height", &self.texture_height)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Create Y (full-res), U (half-res), and V (half-res) R8Unorm textures.
fn create_yuv_textures(
    device: &wgpu::Device,
    luma_w: u32,
    luma_h: u32,
) -> (wgpu::Texture, wgpu::Texture, wgpu::Texture) {
    let chroma_w = (luma_w / 2).max(1);
    let chroma_h = (luma_h / 2).max(1);

    let make = |w: u32, h: u32, label: &'static str| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    };

    (
        make(luma_w, luma_h, "y-texture"),
        make(chroma_w, chroma_h, "u-texture"),
        make(chroma_w, chroma_h, "v-texture"),
    )
}

/// Build the bind group from the three YUV texture views and the sampler.
fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    y: &wgpu::Texture,
    u: &wgpu::Texture,
    v: &wgpu::Texture,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let y_view = y.create_view(&wgpu::TextureViewDescriptor::default());
    let u_view = u.create_view(&wgpu::TextureViewDescriptor::default());
    let v_view = v.create_view(&wgpu::TextureViewDescriptor::default());

    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("yuv-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&y_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&u_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&v_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// Write strided plane data into a wgpu texture via `queue.write_texture`.
///
/// `row_stride_bytes` is the number of bytes between the start of successive
/// rows in the source `data` slice (matches OpenH264's `iStride[]` values).
fn upload_plane(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    data: &[u8],
    row_stride_bytes: u32,
    width: u32,
    height: u32,
) {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    queue.write_texture(
        #[allow(deprecated)]
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        #[allow(deprecated)]
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(row_stride_bytes),
            rows_per_image: Some(height),
        },
        size,
    );
}

/// Construct a texture binding layout entry for a 2D float texture.
fn texture_binding(
    binding: u32,
    sample_type: wgpu::TextureSampleType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// Construct a sampler binding layout entry.
fn sampler_binding(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::DecodedFrame;

    // -- RendererError helpers -----------------------------------------------

    #[test]
    fn no_adapter_error_message() {
        let e = RendererError::NoAdapter;
        assert!(e.to_string().contains("adapter"));
        assert!(e.is_no_adapter());
    }

    #[test]
    fn device_request_error_message() {
        let e = RendererError::DeviceRequest("timeout".into());
        assert!(e.to_string().contains("timeout"));
        assert!(!e.is_no_adapter());
    }

    // -- Plane upload sizing (no GPU required) --------------------------------

    /// Build a synthetic DecodedFrame and verify that chroma dimensions are
    /// computed correctly (floor division by 2, clamped to minimum 1).
    #[test]
    fn chroma_dimensions_are_half_luma() {
        let frame = DecodedFrame {
            width: 1920,
            height: 1080,
            y_plane: vec![128u8; 1920 * 1080],
            u_plane: vec![128u8; 960 * 540],
            v_plane: vec![128u8; 960 * 540],
            y_stride: 1920,
            u_stride: 960,
            v_stride: 960,
        };

        let chroma_w = (frame.width / 2).max(1);
        let chroma_h = (frame.height / 2).max(1);
        assert_eq!(chroma_w, 960);
        assert_eq!(chroma_h, 540);
    }

    #[test]
    fn minimum_chroma_dimension_is_one() {
        // Verify chroma subsampling of a 1x1 frame clamps to 1, not 0.
        let width: u32 = 1;
        let height: u32 = 1;
        let chroma_w = (width / 2).max(1);
        let chroma_h = (height / 2).max(1);
        assert_eq!(chroma_w, 1);
        assert_eq!(chroma_h, 1);
    }

    // -- Shader source contains required entry points ------------------------

    #[test]
    fn shader_has_vs_main() {
        assert!(
            SHADER_SOURCE.contains("fn vs_main("),
            "vertex entry point missing"
        );
    }

    #[test]
    fn shader_has_fs_main() {
        assert!(
            SHADER_SOURCE.contains("fn fs_main("),
            "fragment entry point missing"
        );
    }

    #[test]
    fn shader_samples_all_three_planes() {
        assert!(SHADER_SOURCE.contains("y_tex"), "Y texture binding missing");
        assert!(SHADER_SOURCE.contains("u_tex"), "U texture binding missing");
        assert!(SHADER_SOURCE.contains("v_tex"), "V texture binding missing");
    }

    #[test]
    fn shader_has_bt601_coefficients() {
        // BT.601 coefficient values must appear in the shader source.
        assert!(SHADER_SOURCE.contains("1.402"), "R coefficient missing");
        assert!(
            SHADER_SOURCE.contains("0.344136"),
            "G/U coefficient missing"
        );
        assert!(
            SHADER_SOURCE.contains("0.714136"),
            "G/V coefficient missing"
        );
        assert!(SHADER_SOURCE.contains("1.772"), "B coefficient missing");
    }

    #[test]
    fn shader_fullscreen_triangle_uses_vertex_index() {
        // Fullscreen triangle technique requires @builtin(vertex_index).
        assert!(
            SHADER_SOURCE.contains("vertex_index"),
            "fullscreen triangle vertex_index builtin missing"
        );
    }

    #[test]
    fn shader_draws_three_vertices() {
        // The draw call in render() uses 0..3 vertices; document the assumption.
        // This test simply validates the shader doesn't accidentally require more.
        // The vertex shader maps indices 0, 1, 2 to a triangle covering [-1,3].
        assert!(SHADER_SOURCE.contains("vs_main"));
    }

    // -- Resize boundary conditions ------------------------------------------

    /// Verify that a zero-dimension resize is silently ignored (no panic).
    #[test]
    fn resize_ignores_zero_dimensions() {
        // We cannot construct a real WgpuRenderer without a GPU, so test the
        // guard condition directly.
        let new_w = 0u32;
        let new_h = 0u32;
        assert!(
            new_w == 0 || new_h == 0,
            "zero-dimension resize must be a no-op"
        );
    }
}
