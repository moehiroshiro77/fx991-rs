// SPDX-License-Identifier: GPL-3.0-only
//
// Copyright (C) 2026 fx991-rs contributors
//
// This program is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the Free Software
// Foundation, version 3.  It is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU General
// Public License in LICENSE for more details.

//! The GPU side: a surface, a render pipeline, and one texture per frame.
//!
//! The face is composed on the CPU and uploaded whole, so the pipeline is a
//! single textured triangle.
//!
//! Two details matter:
//!
//! * the frame texture is `Rgba8Unorm`, **not** `Rgba8UnormSrgb`, because the CPU
//!   already composited in sRGB bytes and an sRGB *texture* would linearise them
//!   on read;
//! * the sampler is `Nearest`, because the display is a dot matrix and
//!   interpolating it makes the edges mush.
//!
//! # Why the surface stays sRGB
//!
//! A linear texture and an sRGB surface look like a mismatch and are not: they are
//! two halves of one round trip.  The GPU reads the composited sRGB bytes as if
//! they were linear, and the sRGB target then encodes them on write -- the encode
//! undoes the read's misinterpretation, so the display receives exactly what the
//! CPU composed.
//!
//! Making the surface non-sRGB "for consistency" breaks the pair.  The composited
//! byte for the calculator's grey body is 77; against a non-sRGB target it reaches
//! the screen as 77, where the source image implies 149, and the whole window goes
//! dark.  A test at the bottom of this file holds the pair together.

use std::sync::Arc;

use fx991_ui::{NATURAL_HEIGHT, NATURAL_WIDTH};
use winit::window::Window;

/// Why a frame could not be presented.
///
/// These are the surface conditions wgpu reports as *states* rather than errors:
/// a minimised window, a resize the surface has not caught up with, and so on.
/// None of them is a bug, so they are returned rather than propagated as an
/// error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceProblem {
    /// No frame was ready in time; skip it and try again.
    Timeout,
    /// The window is minimised or hidden.
    Occluded,
    /// The surface configuration no longer matches the window.
    Outdated,
    /// The surface itself is gone and must be recreated.
    Lost,
}

/// A draw failure that is the program's fault rather than the window's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrawError {
    /// wgpu rejected the draw call.
    Validation(&'static str),
}

impl std::fmt::Display for DrawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DrawError::Validation(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for DrawError {}

/// The format the composed frame is uploaded as.
///
/// **Not** the sRGB variant, and that is load-bearing: the CPU composited in sRGB
/// bytes, and an sRGB texture would linearise them on read.  The sRGB surface
/// target then encodes them again, so the pair is the identity and the display
/// receives what was composed.  See the test at the bottom of this file.
const FRAME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The backends to try, in order, until one yields an adapter.
///
/// Vulkan comes first because opening it is by far the cheapest: its instance
/// and swapchain together account for a fraction of what the other backends
/// cost, and nothing this window does benefits from the difference.
///
/// `GL` is listed separately because `PRIMARY` does not contain it: `PRIMARY` is
/// Vulkan, Metal, DX12 and WebGPU, and GL is the second-tier set on its own.  A
/// Linux machine with no Vulkan driver -- an older card, a software-only Mesa
/// install, some containers and remote desktops -- reaches wgpu through GL and
/// nothing else, so leaving it out would turn a working window into a startup
/// failure.
const BACKEND_PREFERENCE: [wgpu::Backends; 3] = [
    wgpu::Backends::VULKAN,
    wgpu::Backends::PRIMARY,
    wgpu::Backends::GL,
];

/// Open a surface and an adapter, trying each backend in preference order.
///
/// The surface is created from the instance that produced the adapter, so the
/// two cannot be paired up by the caller after the fact.  The instance is not
/// returned: every handle it creates keeps its backend alive on its own.
///
/// A backend that cannot offer a surface is treated the same as one that cannot
/// offer an adapter -- the loop moves on -- because both are exactly what the
/// fallback exists for.  Only the last backend's failure is reported.
/// Open an instance, a surface and an adapter, trying each backend in preference
/// order.
///
/// The surface is created from the instance that produced the adapter, so the
/// two cannot be paired up by the caller after the fact.  The instance is handed
/// back because it is what every later window needs to make its own surface.
///
/// A backend that cannot offer a surface is treated the same as one that cannot
/// offer an adapter -- the loop moves on -- because both are exactly what the
/// fallback exists for.  Only the last backend's failure is reported.
fn open_surface_and_adapter(
    window: &Arc<Window>,
) -> Result<(wgpu::Instance, wgpu::Surface<'static>, wgpu::Adapter), Box<dyn std::error::Error>> {
    let mut last_error = None;
    for backends in BACKEND_PREFERENCE {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        let instance = wgpu::Instance::new(desc);
        let surface = match instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(err) => {
                last_error = Some(err.to_string());
                continue;
            }
        };
        match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        })) {
            Ok(adapter) => return Ok((instance, surface, adapter)),
            Err(err) => last_error = Some(err.to_string()),
        }
    }
    Err(last_error
        .unwrap_or_else(|| "no backend offered an adapter".to_string())
        .into())
}

/// The GPU resources both windows share.
///
/// Opening a device is the expensive part -- adapter selection, the instance, the
/// pipeline layout and the shader -- and none of it depends on which window is
/// being drawn to.  The calculator and the debugger are two windows onto one
/// machine, so they share all of it and differ only in their surface, their
/// swapchain configuration and the texture holding the frame they are showing.
///
/// Every `wgpu` handle here is reference-counted internally, so the per-window
/// side holds clones rather than borrowing this; the device stays alive as long
/// as any window does.
pub struct GpuDevice {
    instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    sampler: wgpu::Sampler,
    bind_group_layout: wgpu::BindGroupLayout,
    /// The largest texture the device allows, per side.
    ///
    /// The zoom and the debugger's window size are capped by this: a window
    /// taller than the limit makes `Surface:configure` panic with a validation
    /// error, which is a crash, not a graceful failure.  It is 2048 only if the
    /// device was asked for `downlevel_defaults`, which this app deliberately
    /// does not do.
    pub max_texture_dimension: u32,
}

impl GpuDevice {
    /// Open a device, using `window` to pick a backend that can present.
    ///
    /// The window is needed because backend selection is a question about
    /// presentation -- a Vulkan instance that cannot make a surface for this
    /// window is no use however fast it is -- so the first window decides, and
    /// every later window reuses the answer.
    pub fn new(window: &Arc<Window>) -> Result<Self, Box<dyn std::error::Error>> {
        let (instance, surface, adapter) = open_surface_and_adapter(window)?;
        // The surface was only needed to prove the adapter can present to this
        // window; the window's own view creates the one it keeps.
        drop(surface);

        // Ask for the adapter's own limits, not `downlevel_defaults`.
        //
        // `downlevel_defaults` is a WebGL-compatible baseline that pins
        // `max_texture_dimension_2d` to 2048.  `Surface:configure` validates the
        // window size against the *device's* limits, so requesting that baseline
        // made a window taller than 2048px a hard panic -- even though the
        // adapter here supports 32768.  This app needs nothing exotic, but it
        // does need a window bigger than 2048px when zoomed.
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("fx991"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                // `Performance` (the default) lets the driver sub-allocate
                // resources into large blocks.  These windows hold one small
                // texture each, so that strategy buys nothing and commits a great
                // deal of memory; `MemoryUsage` keeps the blocks small.
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                ..Default::default()
            }))?;

        // `Surface:configure` validates against the *device's* limits, so read
        // the limit back from the device rather than from the adapter.
        let max_texture_dimension = device.limits().max_texture_dimension_2d;
        println!(
            "renderer: {:?} ({:?}), max texture {}px",
            adapter.get_info().name,
            adapter.get_info().backend,
            max_texture_dimension
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        Ok(Self {
            instance,
            device,
            queue,
            sampler,
            bind_group_layout,
            max_texture_dimension,
        })
    }

    /// Build the per-window half: a surface, its swapchain, and a pipeline that
    /// targets that swapchain's format.
    ///
    /// The pipeline is per window rather than shared because its colour target
    /// format comes from the surface, and two surfaces on one adapter can report
    /// different preferences.  Building one is a shader compile, so this is done
    /// once per window rather than per frame.
    pub fn view(&self, window: &Arc<Window>) -> Result<Gpu, Box<dyn std::error::Error>> {
        let size = window.inner_size();
        let surface = self.instance.create_surface(window.clone())?;
        let adapter = self.adapter()?;

        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("the adapter cannot present to this surface")?;
        // The format is left at the surface's own preference, which is the sRGB
        // variant.  That is not a detail to "fix": the frame texture is
        // `Rgba8Unorm`, so the GPU reads the composited sRGB bytes as if they were
        // linear, and an sRGB target encodes them back.  The two operations are
        // inverses, so the bytes reach the display exactly as composed.
        //
        // Forcing a non-sRGB target breaks that pairing and darkens everything:
        // the composited byte for the calculator's grey body is 77, and it would
        // reach the screen as 77 where the source image implies 149.
        //
        // A queued (vsync) swapchain costs many times the driver memory of an
        // immediate one, and a queue only helps a producer that runs ahead of
        // the display.  These windows redraw solely when something changes,
        // which is far slower than a refresh, so the queue would sit empty while
        // still being paid for.
        //
        // `AutoNoVsync` resolves to `Immediate`, else `Mailbox`, else `Fifo`,
        // so it is valid on every platform.
        config.present_mode = wgpu::PresentMode::AutoNoVsync;
        surface.configure(&self.device, &config);

        let texture = create_frame_texture(&self.device, NATURAL_WIDTH, NATURAL_HEIGHT);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group =
            make_bind_group(&self.device, &self.bind_group_layout, &view, &self.sampler);

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("blit"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("blit"),
                bind_group_layouts: &[Some(&self.bind_group_layout)],
                immediate_size: 0,
            });
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("blit"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        Ok(Gpu {
            device: self.device.clone(),
            queue: self.queue.clone(),
            surface,
            config,
            pipeline,
            bind_group_layout: self.bind_group_layout.clone(),
            bind_group,
            sampler: self.sampler.clone(),
            texture,
            needs_reconfigure: false,
        })
    }

    /// The adapter the device was opened on.
    ///
    /// `wgpu` does not hand the adapter back from a device, so it is asked of the
    /// instance again.  This is what `get_capabilities` and `get_default_config`
    /// need, and it costs an enumeration rather than a device creation.
    fn adapter(&self) -> Result<wgpu::Adapter, Box<dyn std::error::Error>> {
        pollster::block_on(self.instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|err| err.to_string().into())
    }
}

/// One window's GPU side: a surface, a pipeline, and a texture per frame.
pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    /// Kept alongside the bind group so a resize can build a new one: `wgpu`
    /// handles are reference-counted, so these are clones of the shared ones
    /// rather than copies of the resources.
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    texture: wgpu::Texture,
    /// Set when wgpu reported the surface as suboptimal, so the next resize
    /// reconfigures it even if the size did not change.
    needs_reconfigure: bool,
}

impl Gpu {
    /// Upload a composed frame and draw it.
    ///
    /// Returns `Ok(None)` when the frame was presented, and `Ok(Some(reason))`
    /// when the surface was not available this time -- a minimised window, or a
    /// surface that needs reconfiguring.  Neither is an error: the caller just
    /// skips the frame.  `Err` is reserved for an unusable device.
    pub fn draw(&mut self, frame: &fx991_ui::Frame) -> Result<Option<SurfaceProblem>, DrawError> {
        // `write_texture` needs no 256-byte row padding, so the frame goes up
        // as-is.
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(frame.width * 4),
                rows_per_image: Some(frame.height),
            },
            wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
        );

        // Since wgpu 30 the acquire step returns an enum rather than a
        // `Result`: the surface problems are ordinary conditions to report, not
        // errors to propagate.
        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => texture,
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                self.needs_reconfigure = true;
                texture
            }
            wgpu::CurrentSurfaceTexture::Timeout => return Ok(Some(SurfaceProblem::Timeout)),
            wgpu::CurrentSurfaceTexture::Occluded => return Ok(Some(SurfaceProblem::Occluded)),
            wgpu::CurrentSurfaceTexture::Outdated => return Ok(Some(SurfaceProblem::Outdated)),
            wgpu::CurrentSurfaceTexture::Lost => return Ok(Some(SurfaceProblem::Lost)),
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(DrawError::Validation(
                    "get_current_texture reported a validation error",
                ))
            }
        };
        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &surface_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
        // Since wgpu 30 presenting is the queue's job, not the texture's.
        self.queue.present(surface_texture);
        Ok(None)
    }

    /// Rebuild the texture and bind group for a new frame size.
    pub fn resize_texture(&mut self, width: u32, height: u32) {
        self.texture = create_frame_texture(&self.device, width, height);
        let view = self
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.bind_group =
            make_bind_group(&self.device, &self.bind_group_layout, &view, &self.sampler);
    }

    pub fn resize_surface(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.needs_reconfigure = false;
    }

    /// Whether wgpu asked for the surface to be reconfigured.
    ///
    /// A `Suboptimal` acquisition still gives a usable frame, so the draw
    /// succeeds; this records that the *next* one should reconfigure first.
    pub fn take_reconfigure_request(&mut self) -> bool {
        std::mem::take(&mut self.needs_reconfigure)
    }
}

fn create_frame_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("frame"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FRAME_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn make_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("frame"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// A full-screen triangle sampling the frame texture.
const SHADER: &str = r"
struct VertexOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) index: u32) -> VertexOut {
    // One oversized triangle covers the clip square with no vertex buffer.
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var out: VertexOut;
    let p = positions[index];
    out.pos = vec4<f32>(p, 0.0, 1.0);
    // Flip Y: clip space is bottom-up, texture rows are top-down.
    out.uv = vec2<f32>((p.x + 1.0) * 0.5, 1.0 - (p.y + 1.0) * 0.5);
    return out;
}

@group(0) @binding(0) var frame_tex: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;

@fragment
fn fs(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(frame_tex, frame_sampler, in.uv);
}
";

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame texture and the surface are a matched pair, and this is the
    /// property that must not be "fixed".
    ///
    /// The CPU composites in sRGB bytes.  An `Rgba8Unorm` texture hands those
    /// bytes to the shader as if they were linear, and an sRGB surface target
    /// encodes them on write -- so the display receives exactly what was
    /// composed.  Changing either half to look "correct" on its own breaks the
    /// pair and darkens the picture: the composited byte for the calculator's
    /// grey body is 77, and it would reach the screen as 77 where the source
    /// image implies 149.
    #[test]
    fn the_frame_texture_and_the_surface_are_a_matched_pair() {
        // The texture stays linear, so the GPU does not linearise twice.
        assert_eq!(FRAME_FORMAT, wgpu::TextureFormat::Rgba8Unorm);
        assert!(
            !FRAME_FORMAT.is_srgb(),
            "an sRGB texture would linearise the composited bytes on read"
        );
        // The surface stays sRGB, so the write encodes what the read left
        // linear.  The config takes its format from the adapter rather than from
        // a constant, so what is pinned here is which of the two variants pairs
        // with a linear texture.
        assert!(
            wgpu::TextureFormat::Bgra8UnormSrgb.is_srgb(),
            "the sRGB surface variant is the one that pairs with a linear texture"
        );
        assert_ne!(
            FRAME_FORMAT,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            "the two halves must not both be sRGB, nor both linear"
        );
    }
}
