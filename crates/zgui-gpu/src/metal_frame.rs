//! Frames encoded with Metal directly (macOS, presenting through the layer).
//!
//! wgpu-core validates and tracks every render pass command, records it,
//! then replays it into Metal, and its submission and wait add bookkeeping
//! of their own: about half of an animating window's presentation CPU, for
//! a frame of a handful of draws. The common frame (quads and text sampling
//! the atlas, no filters) needs none of it: the same pipelines, built here
//! from the same WGSL through naga, record straight into a command buffer on
//! wgpu's own queue, so ordering against wgpu's submissions holds. Backdrop
//! filters mixed by dual-source blending are drawn here too, their blur
//! passes splitting the frame's pass. Frames needing anything else take the
//! wgpu path.
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
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    full: Pipeline,
    full_clear: Pipeline,
    basic: Pipeline,
    opaque: Pipeline,
    copy_offset: Pipeline,
    /// Compiled with the first filter (None: not here).
    blur: std::cell::OnceCell<Option<Blur>>,
}

/// The backdrop blur pipelines (blur.wgsl), as `BlurPipelines`' mixed ones.
struct Blur {
    reduce: Pipeline,
    across: Pipeline,
    down: Pipeline,
    mix: Pipeline,
}

/// The blur's slots: its source in texture 1 (2 holds the area's original
/// pixels where copied), its filters and slots in buffers 2 and 3, and those
/// two records' lengths in buffer 4 (naga reads runtime-sized arrays'
/// lengths from there).
const BLUR_SOURCE: usize = 1;
const BLUR_RECORDS: usize = 2;
const BLUR_SIZES: usize = 4;

/// A batch of filters' records (`BlurRecord`s and `SlotRecord`s, as bytes).
pub(crate) struct BlurRecords<'a> {
    pub(crate) blurs: &'a [u8],
    pub(crate) slots: &'a [u8],
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
        let draw = Library::new(&raw, include_str!("draw.wgsl"), Kind::Quads)?;
        let copy = Library::new(&raw, include_str!("copy_offset.wgsl"), Kind::Copies)?;
        let pipeline = |library: &Library, vertex: &str, fragment: &str, blend: Blend| {
            library.pipeline(&raw, vertex, fragment, MTLPixelFormat::BGRA8Unorm, blend)
        };
        Ok(Self {
            full: pipeline(&draw, "vs", "fs", Blend::Over)?,
            full_clear: pipeline(&draw, "vs", "fs", Blend::Replace)?,
            basic: pipeline(&draw, "vs_basic", "fs_basic", Blend::Over)?,
            opaque: pipeline(&draw, "vs_basic", "fs_basic", Blend::Replace)?,
            copy_offset: pipeline(&copy, "vs", "fs", Blend::Replace)?,
            device: raw,
            queue,
            blur: std::cell::OnceCell::new(),
        })
    }

    /// Whether filters can be drawn here: their pipelines, compiled now if
    /// they are not yet.
    pub(crate) fn blurs(&self) -> bool {
        self.blur().is_some()
    }

    fn blur(&self) -> Option<&Blur> {
        self.blur
            .get_or_init(|| {
                let source = format!(
                    "enable dual_source_blending;\n{}\n{}",
                    include_str!("blur.wgsl"),
                    crate::BLUR_MIX
                );
                let library = Library::new(&self.device, &source, Kind::Blur).ok()?;
                let pipeline = |fragment: &str, format, blend| {
                    library.pipeline(&self.device, "vs", fragment, format, blend).ok()
                };
                let half = MTLPixelFormat::RGBA16Float;
                Some(Blur {
                    reduce: pipeline("fs_reduce", half, Blend::Replace)?,
                    across: pipeline("fs_across", half, Blend::Replace)?,
                    down: pipeline("fs_down", half, Blend::Replace)?,
                    mix: pipeline("fs_mix", MTLPixelFormat::BGRA8Unorm, Blend::Mix)?,
                })
            })
            .as_ref()
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
        let encoder = pass(&buffer, target, MTLLoadAction::Load);
        let mut frame = Frame {
            buffer,
            encoder,
            current: None,
            vertices: {
                use objc2::Message;
                vertices.retain()
            },
            viewport,
        };
        frame.quad_state();
        frame
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

/// A render pass of `buffer` into `target`, stored.
fn pass(
    buffer: &ProtocolObject<dyn MTLCommandBuffer>,
    target: &ProtocolObject<dyn MTLTexture>,
    load: MTLLoadAction,
) -> Encoder {
    let pass = MTLRenderPassDescriptor::new();
    // SAFETY: Index 0 of a render pass descriptor's colour attachments.
    let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
    color.setTexture(Some(target));
    color.setLoadAction(load);
    color.setStoreAction(MTLStoreAction::Store);
    buffer
        .renderCommandEncoderWithDescriptor(&pass)
        .expect("a render encoder")
}

/// One frame's command buffer and its render pass into the target: one,
/// unless backdrop filters split it.
pub(crate) struct Frame {
    buffer: CommandBuffer,
    encoder: Encoder,
    current: Option<Bound>,
    vertices: Retained<ProtocolObject<dyn objc2_metal::MTLBuffer>>,
    viewport: [f32; 4],
}

impl Frame {
    /// The viewport and vertex buffer quads draw with.
    fn quad_state(&mut self) {
        let bytes = std::ptr::NonNull::from(&self.viewport).cast();
        // SAFETY: Sixteen bytes of viewport for slot 0 of both stages, and the
        // frame's live vertex buffer.
        unsafe {
            self.encoder
                .setVertexBytes_length_atIndex(bytes, size_of_val(&self.viewport), 0);
            self.encoder
                .setFragmentBytes_length_atIndex(bytes, size_of_val(&self.viewport), 0);
            self.encoder
                .setVertexBuffer_offset_atIndex(Some(&self.vertices), 0, VERTICES);
        }
    }
    /// Bind a batch's records for the blur's fragments: as bytes where Metal
    /// takes them inline, else in a buffer.
    fn records(&self, frame: &MetalFrame, records: &BlurRecords<'_>) {
        let sizes = [records.blurs.len() as u32, records.slots.len() as u32];
        // SAFETY: Eight bytes of lengths, copied by Metal.
        unsafe {
            self.encoder.setFragmentBytes_length_atIndex(
                std::ptr::NonNull::from(&sizes).cast(),
                size_of_val(&sizes),
                BLUR_SIZES,
            );
        }
        for (index, bytes) in [records.blurs, records.slots].into_iter().enumerate() {
            let slot = BLUR_RECORDS + index;
            let pointer = std::ptr::NonNull::from(bytes).cast();
            // SAFETY: The bytes outlive the call (Metal copies them), or a
            // buffer made from them is retained by the encoder.
            unsafe {
                if bytes.len() <= 4096 {
                    self.encoder
                        .setFragmentBytes_length_atIndex(pointer, bytes.len(), slot);
                } else {
                    let buffer = frame
                        .device
                        .newBufferWithBytes_length_options(
                            pointer,
                            bytes.len(),
                            objc2_metal::MTLResourceOptions::StorageModeShared,
                        )
                        .expect("a records buffer");
                    self.encoder
                        .setFragmentBuffer_offset_atIndex(Some(&buffer), 0, slot);
                }
            }
        }
    }
    /// A batch of backdrop filters' blur passes (`GpuRenderer::plan_blur`):
    /// `target` averaged down into `atlas[0]`, blurred across into
    /// `atlas[1]` and down into `atlas[0]`, each slot (atlas x, y, width,
    /// height) drawn in each. The pass into the target ends; `resume` opens
    /// the next.
    pub(crate) fn blur(
        &mut self,
        frame: &MetalFrame,
        target: &ProtocolObject<dyn MTLTexture>,
        atlas: [&ProtocolObject<dyn MTLTexture>; 2],
        records: &BlurRecords<'_>,
        slots: &[(u32, u32, u32, u32)],
    ) {
        let blur = frame.blur().expect("checked by `blurs`");
        let passes = [
            (&blur.reduce, target, atlas[0]),
            (&blur.across, atlas[0], atlas[1]),
            (&blur.down, atlas[1], atlas[0]),
        ];
        for (pipeline, source, into) in passes {
            self.encoder.endEncoding();
            self.encoder = pass(&self.buffer, into, MTLLoadAction::Clear);
            self.encoder.setRenderPipelineState(pipeline);
            // SAFETY: Live textures for the blur's source slot.
            unsafe {
                self.encoder
                    .setFragmentTexture_atIndex(Some(source), BLUR_SOURCE);
            }
            self.records(frame, records);
            for (index, &(x, y, width, height)) in slots.iter().enumerate() {
                if width == 0 || height == 0 {
                    continue;
                }
                self.scissor((x, y, width, height));
                self.triangle(index as u32);
            }
        }
        self.current = None;
    }
    /// Open the next pass into `target` after a batch's blur passes.
    pub(crate) fn resume(&mut self, target: &ProtocolObject<dyn MTLTexture>) {
        self.encoder.endEncoding();
        self.encoder = pass(&self.buffer, target, MTLLoadAction::Load);
        self.current = None;
        self.quad_state();
    }
    /// Draw a batch of filters back over the target, mixed by dual-source
    /// blending: per filter (its index in the batch) and output.
    pub(crate) fn mixes(
        &mut self,
        frame: &MetalFrame,
        atlas: &ProtocolObject<dyn MTLTexture>,
        records: &BlurRecords<'_>,
        draws: &[(u32, crate::Scissor)],
    ) {
        let blur = frame.blur().expect("checked by `blurs`");
        self.encoder.setRenderPipelineState(&blur.mix);
        self.current = None;
        // SAFETY: A live texture for the blur's source slot.
        unsafe {
            self.encoder
                .setFragmentTexture_atIndex(Some(atlas), BLUR_SOURCE);
        }
        self.records(frame, records);
        for &(index, scissor) in draws {
            self.scissor(scissor);
            self.triangle(index);
        }
    }
    /// A full-target triangle as instance `index` (blur.wgsl's `vs`).
    fn triangle(&mut self, index: u32) {
        // SAFETY: Three vertices made from their indices, no buffers.
        unsafe {
            self.encoder
                .drawPrimitives_vertexStart_vertexCount_instanceCount_baseInstance(
                    MTLPrimitiveType::Triangle,
                    0,
                    3,
                    1,
                    index as usize,
                )
        };
    }
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

/// What a library's shaders draw, which fixes their slots.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Quads: eight attributes, the viewport uniform in buffer 0, the
    /// sampled texture in texture 0, its sampler inline.
    Quads,
    /// Copies: two attributes, the texture in texture 0.
    Copies,
    /// Backdrop blur (see `Blur`): no attributes, its sampler inline.
    Blur,
}

/// How a pipeline writes its colour.
#[derive(Clone, Copy)]
enum Blend {
    Replace,
    /// Premultiplied alpha, as wgpu's PREMULTIPLIED_ALPHA_BLENDING.
    Over,
    /// The second output's complement keeps the target (`BLUR_MIX`).
    Mix,
}

/// A WGSL module compiled for Metal with zgui's fixed slots (`Kind`), any
/// sampler inline (linear and clamped, as `GpuRenderer::texture_bind`'s).
struct Library {
    library: Retained<ProtocolObject<dyn MTLLibrary>>,
    names: std::collections::HashMap<String, String>,
    kind: Kind,
}

impl Library {
    fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        source: &str,
        kind: Kind,
    ) -> Result<Self, GpuError> {
        let failed = |what: String| GpuError(format!("direct Metal shaders: {what}"));
        let module = naga::front::wgsl::parse_str(source).map_err(|e| failed(e.to_string()))?;
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::DUAL_SOURCE_BLENDING,
        )
        .validate(&module)
        .map_err(|e| failed(e.to_string()))?;
        let binding = |group, binding| naga::ResourceBinding { group, binding };
        let mut resources = naga::back::msl::BindingMap::new();
        let texture = |slot| naga::back::msl::BindTarget {
            texture: Some(slot),
            ..Default::default()
        };
        let buffer = |slot| naga::back::msl::BindTarget {
            buffer: Some(slot),
            ..Default::default()
        };
        if kind == Kind::Blur {
            resources.insert(binding(0, 0), texture(BLUR_SOURCE as u8));
            resources.insert(binding(0, 1), texture(BLUR_SOURCE as u8 + 1));
            resources.insert(binding(0, 2), buffer(BLUR_RECORDS as u8));
            resources.insert(binding(0, 3), buffer(BLUR_RECORDS as u8 + 1));
            resources.insert(
                binding(0, 4),
                naga::back::msl::BindTarget {
                    sampler: Some(naga::back::msl::BindSamplerTarget::Inline(0)),
                    ..Default::default()
                },
            );
        } else if kind == Kind::Quads {
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
                        sizes_buffer: (kind == Kind::Blur).then_some(BLUR_SIZES as u8),
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
            kind,
        })
    }

    fn pipeline(
        &self,
        device: &ProtocolObject<dyn MTLDevice>,
        vertex: &str,
        fragment: &str,
        format: MTLPixelFormat,
        blend: Blend,
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
        let attributes = match self.kind {
            Kind::Quads => 8,
            Kind::Copies => 2,
            Kind::Blur => 0,
        };
        if attributes > 0 {
            let vertices = MTLVertexDescriptor::vertexDescriptor();
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
        }
        // SAFETY: Index 0 of the pipeline's colour attachments.
        let color = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
        color.setPixelFormat(format);
        let destination = match blend {
            Blend::Replace => None,
            Blend::Over => Some((
                MTLBlendFactor::OneMinusSourceAlpha,
                MTLBlendFactor::OneMinusSourceAlpha,
            )),
            Blend::Mix => Some((
                MTLBlendFactor::OneMinusSource1Color,
                MTLBlendFactor::OneMinusSource1Alpha,
            )),
        };
        if let Some((rgb, alpha)) = destination {
            color.setBlendingEnabled(true);
            color.setSourceRGBBlendFactor(MTLBlendFactor::One);
            color.setSourceAlphaBlendFactor(MTLBlendFactor::One);
            color.setDestinationRGBBlendFactor(rgb);
            color.setDestinationAlphaBlendFactor(alpha);
        }
        device
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|e| GpuError(format!("direct Metal pipeline: {e}")))
    }
}
