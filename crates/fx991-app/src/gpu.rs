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
//! * the texture is `Rgba8Unorm`, **not** `Rgba8UnormSrgb`.  Compositing already
//!   happened in sRGB bytes on the CPU, and an sRGB texture would linearise them
//!   a second time and come out too dark;
//! * the sampler is `Nearest`, because the display is a dot matrix and
//!   interpolating it makes the edges mush.

use std::sync::Arc;

use fx991_ui::{NATURAL_HEIGHT, NATURAL_WIDTH};
use winit::window::Window;

/// The GPU side: a surface, a pipeline, and one texture we overwrite per frame.
pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    texture: wgpu::Texture,
    sampler: wgpu::Sampler,
    /// The largest texture the device allows, per side.
    ///
    /// The zoom is capped by this: a window taller than the limit makes
    /// `Surface:configure` panic with a validation error, which is a crash, not
    /// a graceful failure.  It is 2048 only if the device was asked for
    /// `downlevel_defaults`, which this app deliberately does not do.
    pub max_texture_dimension: u32,
}

impl Gpu {
    pub fn new(window: Arc<Window>) -> Result<Self, Box<dyn std::error::Error>> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window)?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))?;
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
                memory_hints: wgpu::MemoryHints::default(),
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

        let config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("the adapter cannot present to this surface")?;
        surface.configure(&device, &config);

        let texture = create_frame_texture(&device, NATURAL_WIDTH, NATURAL_HEIGHT);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
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
        let bind_group = make_bind_group(&device, &bind_group_layout, &view, &sampler);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
            multiview: None,
            cache: None,
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group_layout,
            bind_group,
            texture,
            sampler,
            max_texture_dimension,
        })
    }

    /// Upload a composed frame and draw it.
    pub fn draw(&mut self, frame: &fx991_ui::Frame) -> Result<(), wgpu::SurfaceError> {
        // `write_texture` needs no 256-byte row padding (verified in
        //), so the frame goes up as-is.
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

        let surface_texture = self.surface.get_current_texture()?;
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
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
        surface_texture.present();
        Ok(())
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
        // NOT Srgb: the CPU already composited in sRGB bytes.
        format: wgpu::TextureFormat::Rgba8Unorm,
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
