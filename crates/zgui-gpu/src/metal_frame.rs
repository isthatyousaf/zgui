//! Frames encoded with Metal directly (macOS, presenting through the layer).
//!
//! wgpu-core validates and tracks every render pass command, records it,
//! then replays it into Metal, and its submission and wait add bookkeeping
//! of their own: about half of an animating window's presentation CPU, for
//! a frame of a handful of draws. The common frame (quads and text sampling
//! the atlas, no filters) needs none of it: the same pipelines, built here
//! from the same WGSL through naga, record straight into a command buffer on
//! wgpu's own queue, so ordering against wgpu's submissions holds. Frames
//! needing anything else take the wgpu path.
use crate::{GpuError, Shading};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLCompileOptions,
    MTLDevice, MTLLibrary, MTLLoadAction, MTLPixelFormat, MTLPrimitiveType,
    MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
    MTLRenderPipelineState, MTLScissorRect, MTLStoreAction, MTLTexture, MTLVertexDescriptor,
    MTLVertexFormat, MTLVertexStepFunction,
};
use wgpu::naga;

type Pipeline = Retained<ProtocolObject<dyn MTLRenderPipelineState>>;
pub(crate) type Texture = Retained<ProtocolObject<dyn MTLTexture>>;
pub(crate) type CommandBuffer = Retained<ProtocolObject<dyn MTLCommandBuffer>>;
type Encoder = Retained<ProtocolObject<dyn MTLRenderCommandEncoder>>;

/// The vertex buffer's slot; slot 0 holds the viewport.
const VERTICES: usize = 1;

pub(crate) struct MetalFrame {
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    full: Pipeline,
    full_clear: Pipeline,
    basic: Pipeline,
    opaque: Pipeline,
    copy_offset: Pipeline,
}

impl MetalFrame {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self, GpuError> {
        // SAFETY: The HAL guards only borrow the live device and queue.
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
            .ok_or_else(|| GpuError("not a Metal device".into()))?;
        let raw = hal.raw_device().clone();
        drop(hal);
        let hal_queue = unsafe { queue.as_hal::<wgpu::hal::api::Metal>() }
            .ok_or_else(|| GpuError("not a Metal queue".into()))?;
        let queue = {
            use objc2::Message;
            hal_queue.as_raw().retain()
        };
        drop(hal_queue);
        let draw = Library::new(&raw, include_str!("draw.wgsl"), true)?;
        let copy = Library::new(&raw, include_str!("copy_offset.wgsl"), false)?;
        let pipeline = |library: &Library, vertex: &str, fragment: &str, blend: bool| {
            library.pipeline(&raw, vertex, fragment, blend)
        };
        Ok(Self {
            queue,
            full: pipeline(&draw, "vs", "fs", true)?,
            full_clear: pipeline(&draw, "vs", "fs", false)?,
            basic: pipeline(&draw, "vs_basic", "fs_basic", true)?,
            opaque: pipeline(&draw, "vs_basic", "fs_basic", false)?,
            copy_offset: pipeline(&copy, "vs", "fs", false)?,
        })
    }

    /// Open a frame drawing into `target`, its pixels kept (loaded).
    pub(crate) fn begin(
        &self,
        target: &ProtocolObject<dyn MTLTexture>,
        vertices: &ProtocolObject<dyn objc2_metal::MTLBuffer>,
        viewport: [f32; 4],
    ) -> Frame {
        let buffer = self
            .queue
            .commandBuffer()
            .expect("a command buffer");
        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: Index 0 of a render pass descriptor's colour attachments.
        let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        color.setTexture(Some(target));
        color.setLoadAction(MTLLoadAction::Load);
        color.setStoreAction(MTLStoreAction::Store);
        let encoder = buffer
            .renderCommandEncoderWithDescriptor(&pass)
            .expect("a render encoder");
        let bytes = std::ptr::NonNull::from(&viewport).cast();
        // SAFETY: Sixteen bytes of viewport for slot 0 of both stages, and the
        // frame's live vertex buffer.
        unsafe {
            encoder.setVertexBytes_length_atIndex(bytes, size_of_val(&viewport), 0);
            encoder.setFragmentBytes_length_atIndex(bytes, size_of_val(&viewport), 0);
            encoder.setVertexBuffer_offset_atIndex(Some(vertices), 0, VERTICES);
        }
        Frame {
            buffer,
            encoder,
            current: None,
        }
    }

    fn pipeline(&self, shading: Shading) -> &Pipeline {
        match shading {
            Shading::Full => &self.full,
            Shading::Basic => &self.basic,
            Shading::Opaque => &self.opaque,
        }
    }
}

/// What a frame's encoder draws with now.
#[derive(Clone, Copy, PartialEq)]
enum Bound {
    Quads(Shading),
    FullClear,
    CopyOffset,
}

/// One frame's command buffer and its single render pass.
pub(crate) struct Frame {
    buffer: CommandBuffer,
    encoder: Encoder,
    current: Option<Bound>,
}

impl Frame {
    fn bind(&mut self, frame: &MetalFrame, bound: Bound) {
        if self.current == Some(bound) {
            return;
        }
        self.current = Some(bound);
        let pipeline = match bound {
            Bound::Quads(shading) => frame.pipeline(shading),
            Bound::FullClear => &frame.full_clear,
            Bound::CopyOffset => &frame.copy_offset,
        };
        self.encoder.setRenderPipelineState(pipeline);
    }
    /// The texture fragments sample: the atlas, or for catch-up copies the
    /// shown surface.
    pub(crate) fn texture(&mut self, texture: &ProtocolObject<dyn MTLTexture>) {
        // SAFETY: A live texture for slot 0, as both libraries bind it (the
        // copy's vertex stage reads its size).
        unsafe {
            self.encoder.setVertexTexture_atIndex(Some(texture), 0);
            self.encoder.setFragmentTexture_atIndex(Some(texture), 0);
        }
    }
    pub(crate) fn scissor(&mut self, (x, y, width, height): (u32, u32, u32, u32)) {
        self.encoder.setScissorRect(MTLScissorRect {
            x: x as usize,
            y: y as usize,
            width: width as usize,
            height: height as usize,
        });
    }
    /// Draw `instances` quads of `shading`; `clear` for the pipeline that
    /// replaces rather than blends (a region's background).
    pub(crate) fn quads(
        &mut self,
        frame: &MetalFrame,
        shading: Shading,
        clear: bool,
        instances: std::ops::Range<u32>,
    ) {
        let bound = match (shading, clear) {
            (Shading::Full, true) => Bound::FullClear,
            (Shading::Basic | Shading::Opaque, true) => Bound::Quads(Shading::Opaque),
            (shading, false) => Bound::Quads(shading),
        };
        self.bind(frame, bound);
        self.draw(instances);
    }
    /// Draw `instances` shifted copies from the texture bound with `texture`.
    pub(crate) fn copies(&mut self, frame: &MetalFrame, instances: std::ops::Range<u32>) {
        self.bind(frame, Bound::CopyOffset);
        self.draw(instances);
    }
    fn draw(&mut self, instances: std::ops::Range<u32>) {
        // SAFETY: Four-vertex strips over instances the vertex buffer holds.
        unsafe {
            self.encoder
                .drawPrimitives_vertexStart_vertexCount_instanceCount_baseInstance(
                    MTLPrimitiveType::TriangleStrip,
                    0,
                    4,
                    instances.len(),
                    instances.start as usize,
                )
        };
    }
    /// End the pass and commit: the frame then runs after everything already
    /// submitted to wgpu's queue.
    pub(crate) fn commit(self) -> CommandBuffer {
        self.encoder.endEncoding();
        self.buffer.commit();
        self.buffer
    }
}

/// A WGSL module compiled for Metal with zgui's fixed slots: the viewport
/// uniform in buffer 0, the sampled texture in texture 0, the atlas sampler
/// inline (linear and clamped, as `GpuRenderer::texture_bind`'s).
struct Library {
    library: Retained<ProtocolObject<dyn MTLLibrary>>,
    names: std::collections::HashMap<String, String>,
    /// Draw quads (eight attributes) rather than copies (two).
    quads: bool,
}

impl Library {
    fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        source: &str,
        quads: bool,
    ) -> Result<Self, GpuError> {
        let failed = |what: String| GpuError(format!("direct Metal shaders: {what}"));
        let module = naga::front::wgsl::parse_str(source).map_err(|e| failed(e.to_string()))?;
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&module)
        .map_err(|e| failed(e.to_string()))?;
        let binding = |group, binding| naga::ResourceBinding { group, binding };
        let mut resources = naga::back::msl::BindingMap::new();
        if quads {
            resources.insert(
                binding(1, 0),
                naga::back::msl::BindTarget {
                    buffer: Some(0),
                    ..Default::default()
                },
            );
            resources.insert(
                binding(0, 1),
                naga::back::msl::BindTarget {
                    texture: Some(0),
                    ..Default::default()
                },
            );
            resources.insert(
                binding(0, 2),
                naga::back::msl::BindTarget {
                    sampler: Some(naga::back::msl::BindSamplerTarget::Inline(0)),
                    ..Default::default()
                },
            );
        } else {
            resources.insert(
                binding(0, 0),
                naga::back::msl::BindTarget {
                    texture: Some(0),
                    ..Default::default()
                },
            );
        }
        let per_entry_point_map = module
            .entry_points
            .iter()
            .map(|entry| {
                (
                    entry.name.clone(),
                    naga::back::msl::EntryPointResources {
                        resources: resources.clone(),
                        ..Default::default()
                    },
                )
            })
            .collect();
        let options = naga::back::msl::Options {
            lang_version: (2, 4),
            per_entry_point_map,
            inline_samplers: vec![naga::back::msl::sampler::InlineSampler {
                coord: naga::back::msl::sampler::Coord::Normalized,
                address: [naga::back::msl::sampler::Address::ClampToEdge; 3],
                border_color: naga::back::msl::sampler::BorderColor::TransparentBlack,
                mag_filter: naga::back::msl::sampler::Filter::Linear,
                min_filter: naga::back::msl::sampler::Filter::Linear,
                mip_filter: None,
                lod_clamp: None,
                max_anisotropy: None,
                compare_func: naga::back::msl::sampler::CompareFunc::Never,
            }],
            fake_missing_bindings: false,
            zero_initialize_workgroup_memory: false,
            ..Default::default()
        };
        let pipeline_options = naga::back::msl::PipelineOptions {
            entry_point: None,
            allow_and_force_point_size: false,
            vertex_pulling_transform: false,
            vertex_buffer_mappings: Vec::new(),
        };
        let (msl, translated) =
            naga::back::msl::write_string(&module, &info, &options, &pipeline_options)
                .map_err(|e| failed(e.to_string()))?;
        let names = module
            .entry_points
            .iter()
            .zip(translated.entry_point_names)
            .filter_map(|(entry, name)| Some((entry.name.clone(), name.ok()?)))
            .collect();
        let library = device
            .newLibraryWithSource_options_error(
                &NSString::from_str(&msl),
                Some(&MTLCompileOptions::new()),
            )
            .map_err(|e| failed(e.to_string()))?;
        Ok(Self {
            library,
            names,
            quads,
        })
    }

    fn pipeline(
        &self,
        device: &ProtocolObject<dyn MTLDevice>,
        vertex: &str,
        fragment: &str,
        blend: bool,
    ) -> Result<Pipeline, GpuError> {
        let function = |name: &str| {
            self.names
                .get(name)
                .and_then(|name| self.library.newFunctionWithName(&NSString::from_str(name)))
                .ok_or_else(|| GpuError(format!("direct Metal shaders: no `{name}`")))
        };
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&*function(vertex)?));
        descriptor.setFragmentFunction(Some(&*function(fragment)?));
        // Per-instance quads, 128 bytes each: eight vec4s, as `Quad`.
        let vertices = MTLVertexDescriptor::vertexDescriptor();
        let attributes = if self.quads { 8 } else { 2 };
        // SAFETY: Attribute and layout indices within Metal's limits.
        unsafe {
            for index in 0..attributes {
                let attribute = vertices.attributes().objectAtIndexedSubscript(index);
                attribute.setFormat(MTLVertexFormat::Float4);
                attribute.setOffset(16 * index);
                attribute.setBufferIndex(VERTICES);
            }
            let layout = vertices.layouts().objectAtIndexedSubscript(VERTICES);
            layout.setStride(128);
            layout.setStepFunction(MTLVertexStepFunction::PerInstance);
            layout.setStepRate(1);
        }
        descriptor.setVertexDescriptor(Some(&vertices));
        // SAFETY: Index 0 of the pipeline's colour attachments.
        let color = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
        color.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        if blend {
            // Premultiplied alpha, as wgpu's PREMULTIPLIED_ALPHA_BLENDING.
            color.setBlendingEnabled(true);
            color.setSourceRGBBlendFactor(MTLBlendFactor::One);
            color.setSourceAlphaBlendFactor(MTLBlendFactor::One);
            color.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            color.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        }
        device
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|e| GpuError(format!("direct Metal pipeline: {e}")))
    }
}
