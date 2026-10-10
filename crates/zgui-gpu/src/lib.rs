//! Damage-scissored retained GPU renderer for Vulkan (Linux) and Metal (macOS).
//! The output is premultiplied BGRA8 (read back as RGBA8). Text uses cosmic-text shaping and a bounded
//! GPU glyph atlas; stable text never reshapes on paint or compositor updates.
use cosmic_text::{Buffer, CacheKey, FontSystem, Metrics, SwashCache, SwashContent};
use rustc_hash::FxHashMap;
use std::{cell::RefCell, fmt, rc::Rc, sync::Arc};
use zgui::scene::{Color, NodeId, NodeKind, Rect, Scene};

#[cfg(target_os = "macos")]
mod layer_present;
#[cfg(target_os = "macos")]
mod metal_frame;
#[cfg(target_os = "macos")]
mod native_surface;
mod surface;
mod upload;
pub use surface::PresentationStatus;

const MAX_ATLAS: u32 = 2048;
const INITIAL_ATLAS: u32 = 512;
/// An atlas that fills again within this many frames of its last recycle
/// doubles instead (see `render_flat`).
const ATLAS_THRASH_FRAMES: u64 = 600;
/// How long after its last resize a macOS layer keeps a third drawable.
#[cfg(target_os = "macos")]
const WIDE_DRAWABLES: std::time::Duration = std::time::Duration::from_secs(2);
/// SVG rasters up to this size (physical pixels) share the glyph atlas.
const ATLAS_IMAGE_MAX: u32 = 128;
/// Write straight-alpha `pixels` premultiplied (as `upload_image` does) into
/// the interior of a bordered atlas cell `width` + 2 texels wide.
fn premultiply_into_cell(cell: &mut [u8], pixels: &[u8], width: u32) {
    let row = width as usize * 4;
    for (y, source) in pixels.chunks_exact(row).enumerate() {
        let start = ((y + 1) * (width as usize + 2) + 1) * 4;
        for (texel, pixel) in cell[start..start + row]
            .chunks_exact_mut(4)
            .zip(source.as_chunks::<4>().0)
        {
            let alpha = u16::from(pixel[3]);
            for channel in 0..3 {
                texel[channel] = ((u16::from(pixel[channel]) * alpha + 127) / 255) as u8;
            }
            texel[3] = pixel[3];
        }
    }
}
/// The retained target's format: BGRA, as window surfaces are on macOS and
/// X11, so presenting can be a plain copy (see `copy_present`). Readback
/// converts to RGBA.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
#[derive(Debug)]
pub struct GpuError(pub String);
impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for GpuError {}
#[derive(Default, Debug, Clone, Copy)]
pub struct GpuStats {
    pub draw_calls: usize,
    /// Encoded render passes, including repainted isolated layers; excludes presentation.
    pub render_passes: usize,
    /// Separable filter passes (two per applied backdrop blur), included in render_passes.
    pub blur_passes: usize,
    pub instances: usize,
    pub shaped_nodes: usize,
    pub glyph_uploads: usize,
    pub image_uploads: usize,
    /// Native GPU imports/conversions, never CPU image uploads.
    pub native_surface_imports: usize,
    pub native_surface_conversions: usize,
    pub canvas_rasterizations: usize,
    pub svg_rasterizations: usize,
    pub geometry_rebuilds: usize,
    pub vertex_buffer_allocations: usize,
    pub damaged_pixels: u64,
    pub layer_repaints: usize,
    pub layer_cache_hits: usize,
    pub layer_texture_allocations: usize,
    /// Scrolled regions shifted by copying instead of redrawn.
    pub scroll_copies: usize,
}
/// Retained allocation counters for diagnostics; excludes driver/font database allocations.
#[derive(Debug, Clone, Copy)]
pub struct DebugCacheStats {
    pub shaped_nodes: usize,
    pub shaped_bytes: usize,
    pub cached_quads: usize,
    pub atlas_entries: usize,
    pub atlas_bytes: usize,
    pub swash_images: usize,
    pub swash_outlines: usize,
    pub image_textures: usize,
    pub image_bytes: usize,
    pub canvas_raster_bytes: usize,
    pub svg_raster_bytes: usize,
    pub vertex_buffer_bytes: usize,
    pub layer_textures: usize,
    pub layer_bytes: usize,
}
const SHAPE_BUDGET: usize = 8 * 1024 * 1024;
/// Shaping frames a cached word survives without being used.
const SHAPE_RUN_AGES: u64 = 2;
/// Shaping calls after which the word cache starts a fresh table (see
/// `render`).
const SHAPE_RESET: u64 = 512;
const VERTEX_BUDGET: usize = 32 * 1024 * 1024;
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Quad {
    rect: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
    fade: [f32; 4],
    options: [f32; 4],
    shape: [f32; 4],
    border: [f32; 4],
    /// A vertical fade mask (top y, bottom y, top band, bottom band) from a
    /// `fade_edges` ancestor; zero bands leave the quad as is.
    mask: [f32; 4],
}
const NO_MASK: [f32; 4] = [0.; 4];
/// How a quad is drawn, where drawing is split (see `push_draw`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shading {
    /// Corners, borders, shadows, fades, masks or transforms: `shade`.
    Full,
    /// Colour, or colour times a texture sample: `basic`.
    Basic,
    /// An opaque flat colour: `basic` without blending, which reads every
    /// pixel it writes. Same pixels.
    Opaque,
}
fn shading(quad: &Quad, scale: f32) -> Shading {
    // A panel with square corners and no border only antialiases its edges:
    // on whole device pixels, coverage is exactly what rasterizing gives.
    let whole = |value: f32| {
        let device = value * scale;
        (device - device.round()).abs() < 1e-3
    };
    let [x, y, width, height] = quad.rect;
    let square = quad.shape[3] > 0.5
        && quad.shape[0] <= 0.
        && quad.shape[1] <= 0.
        && quad.options[3] < 0.5
        && quad.rect == quad.fade
        && [x, y, x + width, y + height].into_iter().all(whole);
    // The basic shader computes what the full one does in the same order,
    // bit for bit, except for the coverage of a translucent square panel (an
    // extra divide and multiply): those stay full, so either shader draws the
    // same bytes.
    let plain = quad.options[0] == 0.
        && quad.options[2] < 0.5
        && quad.shape[2] <= 0.
        && (quad.shape[3] <= 0.5 || (square && quad.color[3] >= 1.))
        && quad.mask[2] <= 0.
        && quad.mask[3] <= 0.;
    if !plain {
        Shading::Full
    } else if quad.options[1] <= 0.5 && quad.color[3] >= 1. {
        Shading::Opaque
    } else {
        Shading::Basic
    }
}
/// The quad pipelines, each compiled on first use: a software rasterizer
/// compiles a pipeline into megabytes of code, and plain interfaces never
/// need the full shader there.
struct QuadPipelines {
    device: wgpu::Device,
    shader: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    full: std::cell::OnceCell<wgpu::RenderPipeline>,
    full_clear: std::cell::OnceCell<wgpu::RenderPipeline>,
    basic: std::cell::OnceCell<wgpu::RenderPipeline>,
    basic_clear: std::cell::OnceCell<wgpu::RenderPipeline>,
}
impl QuadPipelines {
    /// The pipeline drawing quads of `shading`.
    fn get(&self, shading: Shading) -> &wgpu::RenderPipeline {
        let (cell, basic, blend) = match shading {
            Shading::Full => (&self.full, false, true),
            Shading::Basic => (&self.basic, true, true),
            Shading::Opaque => (&self.basic_clear, true, false),
        };
        cell.get_or_init(|| {
            quad_pipeline(&self.device, &self.shader, &self.layout, blend, None, basic)
        })
    }
    /// The pipeline clearing a region to the background quad.
    fn clear(&self, basic: bool) -> &wgpu::RenderPipeline {
        if basic {
            self.get(Shading::Opaque)
        } else {
            self.full_clear.get_or_init(|| {
                quad_pipeline(&self.device, &self.shader, &self.layout, false, None, false)
            })
        }
    }
}
/// Damage regions merge when their union adds at most half the smaller's
/// area and this many device pixels (see `merge_damage`): each region costs
/// its draws again, worth more GPU time than shading a 128×128 square.
const NEAR_PIXELS: f32 = 16_384.;
/// Damaged regions of at least this many device pixels draw opaque quads
/// without blending (see `encode_draws`).
const SPLIT_PIXELS: f32 = 32_768.;
/// Split a draw's quads into runs of one shading, in paint order, when
/// `split`. The full shader's size (the registers it holds) slows every pixel
/// it shades, on a GPU as on a software rasterizer: plain quads (backgrounds,
/// glyphs, bars) draw with the basic one.
fn push_draw(draws: &mut Vec<Draw>, quads: &[Quad], draw: Draw, split: bool, scale: f32) {
    if !split || draw.blur.is_some() || draw.layer.is_some() || draw.end <= draw.start {
        draws.push(draw);
        return;
    }
    // Each run ends where the shading changes, so runs alternate shadings.
    let mut start = draw.start;
    while start < draw.end {
        let kind = shading(&quads[start as usize], scale);
        let mut end = start + 1;
        while end < draw.end && shading(&quads[end as usize], scale) == kind {
            end += 1;
        }
        draws.push(Draw {
            start,
            end,
            bounds: quad_bounds(&quads[start as usize..end as usize]),
            shading: kind,
            ..draw.clone()
        });
        start = end;
    }
}
fn texel_at(texture: &wgpu::Texture, x: u32, y: u32) -> wgpu::TexelCopyTextureInfo<'_> {
    wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d { x, y, z: 0 },
        aspect: wgpu::TextureAspect::All,
    }
}
/// Whether moving `scroll`'s pixels reproduces the frame: its content is one
/// contiguous run of paint items free of masks, layers and filters; beneath
/// it the clip shows a single opaque rectangle (or the window background);
/// above it nothing paints in the clip outside this frame's damage.
fn copy_is_exact(
    scene: &Scene,
    scroll: &zgui::scene::ScrollMove,
    damage: &[Rect],
    scale: f32,
) -> bool {
    let clip = scroll.clip;
    let items = scene.layer_items_within(None, Some(&[clip]));
    let content = |item: &zgui::scene::PaintItem<'_>| scene.is_within(item.id, scroll.node);
    let Some(first) = items.iter().position(content) else {
        return false;
    };
    let last = items.iter().rposition(content).expect("found above");
    let paints = |item: &zgui::scene::PaintItem<'_>| -> Option<Rect> {
        let bounds = match item.kind {
            NodeKind::Container(_) if item.effects.blur_radius <= 0. => return None,
            NodeKind::Quad(quad) | NodeKind::Panel { quad, .. } => quad.paint_bounds(item.bounds),
            _ => item.bounds,
        };
        if item.effects.opacity <= 0. {
            return None;
        }
        bounds.intersection(item.clip?)?.intersection(clip)
    };
    let repainted = |rect: Rect| {
        damage.iter().any(|d| {
            d.x <= rect.x
                && d.y <= rect.y
                && d.x + d.width >= rect.x + rect.width
                && d.y + d.height >= rect.y + rect.height
        })
    };
    for item in &items[first..=last] {
        if !content(item) || item.mask.is_some() || item.isolated || item.effects.blur_radius > 0. {
            return false;
        }
    }
    if items[last + 1..]
        .iter()
        .filter_map(paints)
        .any(|rect| !repainted(rect))
    {
        return false;
    }
    // Content opaque across the whole clip (a list's rows) hides whatever
    // lies under it, before the move and after.
    let opaque = |item: &&zgui::scene::PaintItem<'_>| {
        matches!(item.kind, NodeKind::Quad(quad) | NodeKind::Panel { quad, .. }
            if quad.fill.3 == 255 && quad.radius <= 0. && quad.shadow.is_none() && quad.decoration.is_none())
            && item.effects.opacity >= 1.
            && item.effects.edge_fade <= 0.
    };
    if spans_cover(
        clip,
        items[first..=last].iter().filter(opaque).filter_map(paints),
        scale,
    ) {
        return true;
    }
    for item in items[..first].iter().rev() {
        let Some(rect) = paints(item) else {
            continue;
        };
        if let NodeKind::Quad(quad) | NodeKind::Panel { quad, .. } = item.kind
            && quad.fill.3 == 255
            && quad.radius <= 0.
            && quad.border_width <= 0.
            && quad.shadow.is_none()
            && quad.decoration.is_none()
            && item.effects.opacity >= 1.
            && item.effects.edge_fade <= 0.
            && item.mask.is_none()
            && !item.isolated
            && rect == clip
        {
            return true;
        }
        if !repainted(rect) {
            return false;
        }
    }
    true
}
/// Whether rectangles spanning `clip` side to side (or top to bottom) cover
/// it, meeting on whole device pixels: no seam lets anything beneath show.
fn spans_cover(clip: Rect, rects: impl Iterator<Item = Rect>, scale: f32) -> bool {
    let whole = |value: f32| ((value * scale) - (value * scale).round()).abs() < 1e-3;
    let (mut rows, mut columns) = (Vec::new(), Vec::new());
    for r in rects {
        let (right, bottom) = (r.x + r.width, r.y + r.height);
        if r.x <= clip.x && right >= clip.x + clip.width && whole(r.y) && whole(bottom) {
            rows.push((r.y, bottom));
        }
        if r.y <= clip.y && bottom >= clip.y + clip.height && whole(r.x) && whole(right) {
            columns.push((r.x, right));
        }
    }
    let covered = |mut spans: Vec<(f32, f32)>, start: f32, end: f32| {
        spans.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let mut reach = start;
        for (from, to) in spans {
            if from > reach {
                break;
            }
            reach = reach.max(to);
        }
        reach >= end
    };
    covered(rows, clip.y, clip.y + clip.height) || covered(columns, clip.x, clip.x + clip.width)
}
/// Positioned glyphs of laid-out runs; rich text takes each glyph's colour
/// from its run (it is shaped without colours, see `rich_buffer_raw`).
fn glyphs_of<'a>(
    runs: impl Iterator<Item = cosmic_text::LayoutRun<'a>>,
    starts: &[usize],
    rich: Option<&zgui::rich_text::RichText>,
    scale: f32,
) -> Vec<Glyph> {
    let mut glyphs = Vec::new();
    for run in runs {
        for glyph in run.glyphs {
            let p = glyph.physical((0., 0.), scale);
            let run_index = rich.map_or(0, |rich| {
                text::run_at(rich, starts[run.line_i] + glyph.start)
            });
            glyphs.push(Glyph {
                key: p.cache_key,
                color: match rich {
                    Some(rich) => Some(rich.runs()[run_index].color),
                    None => glyph
                        .color_opt
                        .map(|c| zgui::scene::Color(c.r(), c.g(), c.b(), c.a())),
                },
                run: run_index,
                x: p.x as f32,
                y: p.y as f32 + run.line_y * scale,
            });
        }
    }
    glyphs
}
/// A pixel rectangle: x, y, width, height.
type Scissor = (u32, u32, u32, u32);
/// A scroll copy's destination and the offset of its source.
type ScrollCopy = (Scissor, (i32, i32));
/// For each damage region, the index of the topmost quad that paints every
/// pixel of the region opaque: a plain solid rectangle (no texture, radius,
/// border, shadow, fade, mask or transform) at full alpha, unclipped over the
/// region's scissor. Nothing drawn before it, including the clear, can show.
/// 0 when no quad qualifies.
fn occluded_from(
    quads: &[Quad],
    draws: &[Draw],
    regions: &[(Rect, Scissor)],
    scale: f32,
) -> Vec<u32> {
    let covers = |outer: Rect, inner: Rect| {
        outer.x <= inner.x
            && outer.y <= inner.y
            && outer.x + outer.width >= inner.x + inner.width
            && outer.y + outer.height >= inner.y + inner.height
    };
    let plain = |q: &Quad| {
        q.color[3] >= 1.
            && q.options == [0.; 4]
            && q.shape[0] <= 0.
            && q.shape[1] <= 0.
            && q.shape[2] <= 0.
            && q.mask == NO_MASK
    };
    regions
        .iter()
        .map(|(_, (x, y, w, h))| {
            // The pixels the scissor lets through, in logical units.
            let pixels = Rect::new(
                *x as f32 / scale,
                *y as f32 / scale,
                *w as f32 / scale,
                *h as f32 / scale,
            );
            for draw in draws.iter().rev() {
                if draw.image.is_some()
                    || draw.layer.is_some()
                    || draw.blur.is_some()
                    || !covers(draw.bounds, pixels)
                    || !covers(draw.clip, pixels)
                {
                    continue;
                }
                for index in (draw.start..draw.end).rev() {
                    let q = &quads[index as usize];
                    let rect = Rect::new(q.rect[0], q.rect[1], q.rect[2], q.rect[3]);
                    if plain(q) && covers(rect, pixels) {
                        return index;
                    }
                }
            }
            0
        })
        .collect()
}
#[derive(Clone)]
struct Glyph {
    key: CacheKey,
    color: Option<zgui::scene::Color>,
    /// The rich run it was shaped from, to recolour paint-only changes.
    run: usize,
    x: f32,
    y: f32,
}
struct Shaped {
    rich: Option<Arc<zgui::rich_text::RichText>>,
    text_options: zgui::text_layout::TextOptions,
    decorations: Vec<text::RichDecoration>,
    font: zgui::text_layout::FontStyle,
    text: Arc<str>,
    size: f32,
    width: f32,
    height: f32,
    glyphs: Vec<Glyph>,
    quads: Vec<Quad>,
    atlas_epoch: u64,
    last_used: u64,
    /// The prepared text these glyphs were laid out from, kept while drawn.
    prepared: Option<u64>,
}
impl Shaped {
    fn bytes(&self) -> usize {
        self.rich
            .as_ref()
            .map_or(self.text.len(), |r| r.storage_bytes())
            + self.font.storage_bytes()
            + self.decorations.capacity() * std::mem::size_of::<text::RichDecoration>()
            + self.glyphs.capacity() * std::mem::size_of::<Glyph>()
            + self.quads.capacity() * std::mem::size_of::<Quad>()
    }
}
#[derive(Clone, Copy)]
struct AtlasEntry {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    left: i32,
    top: i32,
}
struct LayerCache {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    bounds: Rect,
    revision: u64,
    root_origin: (f32, f32),
    bytes: usize,
    /// The content's extent in texture coordinates.
    uv: [f32; 2],
}
#[derive(Clone)]
struct Draw {
    /// How its quads are drawn; set by `push_draw`.
    shading: Shading,
    blur: Option<(Rect, zgui::scene::Effects)>,
    start: u32,
    end: u32,
    clip: Rect,
    /// Union of this draw's quads: nothing outside it is ever touched.
    bounds: Rect,
    image: Option<u64>,
    layer: Option<NodeId>,
}
/// A frame's damage, encoded at presentation so it can be drawn into the
/// next retained target and the drawable in one pass (see `present_single_pass`).
struct Deferred {
    vertices: wgpu::Buffer,
    draws: Vec<Draw>,
    regions: Vec<(Rect, (u32, u32, u32, u32))>,
    viewport: Option<wgpu::BindGroup>,
    /// The last filter's vertical half, drawn before `draws`.
    blur: Option<VerticalBlur>,
    /// Clear each region first. False for the tail after a backdrop filter:
    /// the passes before it already cleared and drew the regions.
    clear: bool,
    /// No two regions share a pixel (`merge_damage`), so every clear can
    /// precede every draw without redrawing an overlap twice.
    disjoint: bool,
    /// Per region, the first quad to draw (see `occluded_from`).
    first: Vec<u32>,
    /// Draw into the drawable alone, leaving the retained target as it was
    /// (the next frame then repaints everything): the window changed whole
    /// this frame and the last, so the target is likely to be repainted whole
    /// again before any of it is reused. Saves writing every pixel twice.
    direct: bool,
}
/// A backdrop filter's vertical half: one draw per damaged output, reading the
/// horizontal pass's scratch textures (see `GpuRenderer::blur`).
struct VerticalBlur {
    bind: wgpu::BindGroup,
    scissors: Vec<(u32, u32, u32, u32)>,
}
/// Separable blur pipelines. The horizontal pass reads the target and keeps
/// its original pixels; the vertical draws write the target, or the target
/// and a drawable of `both`'s format at presentation.
struct BlurPipelines {
    shader: wgpu::ShaderModule,
    /// Shared by every vertical pipeline so one bind group serves each.
    layout: wgpu::BindGroupLayout,
    horizontal: wgpu::RenderPipeline,
    vertical: wgpu::RenderPipeline,
    both: Option<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
}
impl BlurPipelines {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("backdrop blur"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blur.wgsl").into()),
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vertical blur"),
            entries: &[
                texture(0),
                texture(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let target = Some(wgpu::ColorTargetState {
            format: FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        });
        let horizontal = Self::pipeline(
            device,
            &shader,
            None,
            "fs_split",
            &[target.clone(), target.clone()],
        );
        let vertical = Self::pipeline(
            device,
            &shader,
            Some(&layout),
            "fs",
            std::slice::from_ref(&target),
        );
        Self {
            shader,
            layout,
            horizontal,
            vertical,
            both: None,
        }
    }
    fn pipeline(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        layout: Option<&wgpu::BindGroupLayout>,
        entry: &str,
        targets: &[Option<wgpu::ColorTargetState>],
    ) -> wgpu::RenderPipeline {
        let layout = layout.map(|layout| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vertical blur"),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            })
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("backdrop blur"),
            layout: layout.as_ref(),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                targets,
            }),
            multiview_mask: None,
            cache: None,
        })
    }
    /// The vertical pipeline writing the target and a `format` drawable.
    fn both(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> &wgpu::RenderPipeline {
        if self.both.as_ref().is_none_or(|(f, _)| *f != format) {
            let target = |format| {
                Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })
            };
            let pipeline = Self::pipeline(
                device,
                &self.shader,
                Some(&self.layout),
                "fs_both",
                &[target(FORMAT), target(format)],
            );
            self.both = Some((format, pipeline));
        }
        &self.both.as_ref().expect("created above").1
    }
}
/// The spare retained target with the views and bind groups it needs.
struct Retained {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    copy_bind: Option<wgpu::BindGroup>,
    blit_bind: Option<wgpu::BindGroup>,
}
/// Pipelines writing the retained target and a drawable of `format` at once.
struct SinglePass {
    format: wgpu::TextureFormat,
    copy: wgpu::RenderPipeline,
    /// Into the next retained target and the drawable.
    both: PickedPipelines,
    /// Into the drawable alone (`Deferred::direct`).
    direct: PickedPipelines,
}
/// Presentation through surfaces of the renderer's own (`layer_present`).
#[cfg(target_os = "macos")]
#[derive(Default)]
struct LayerPresent {
    layer: Option<layer_present::Layer>,
    /// Surfaces, each with the frame it holds (None: none yet).
    buffers: Vec<(layer_present::Buffer, Option<u64>)>,
    /// The surface the layer shows: the last presented frame.
    shown: Option<usize>,
    /// The surface the next frame draws into (the retained target).
    current: Option<usize>,
    /// Frames presented so far.
    frame: u64,
    /// Pixels each recent frame changed, oldest first.
    history: std::collections::VecDeque<(u64, Vec<Scissor>)>,
    /// Pixels the frame being drawn changes so far.
    changed: Vec<Scissor>,
    /// Regions the current surface takes from the shown one before the
    /// frame draws (`begin_layer_frame`).
    catch_up: Vec<Scissor>,
    /// Frames come at the display's full rate (`GpuRenderer::set_full_rate`):
    /// a third surface is kept for them.
    full_rate: bool,
    /// When the last frame was shown: one soon after it is part of a steady
    /// run of frames, queued by the layer (`Layer::show`).
    shown_at: Option<std::time::Instant>,
}
#[cfg(target_os = "macos")]
impl LayerPresent {
    /// Per surface, whether the layer may still show it: the one on screen
    /// and any queued after it. Without word from the layer, the two latest.
    fn busy(&self) -> Vec<bool> {
        let displayed = self.layer.as_ref().and_then(|layer| layer.displayed());
        let on_screen = displayed.and_then(|id| {
            self.buffers
                .iter()
                .find(|(buffer, _)| buffer.surface.id() == id)
                .and_then(|(_, frame)| *frame)
        });
        let latest = self.frame;
        self.buffers
            .iter()
            .map(|(_, held)| match (held, on_screen) {
                (None, _) => false,
                (Some(held), Some(on_screen)) => *held >= on_screen,
                (Some(held), None) => held + 2 > latest,
            })
            .collect()
    }
}
/// Surfaces a window presents through at most: one on screen, one queued,
/// and one drawn.
#[cfg(target_os = "macos")]
const LAYER_BUFFERS: usize = 3;
/// Frames of changes kept to bring a surface up to date (older ones copy all).
#[cfg(target_os = "macos")]
const LAYER_HISTORY: usize = 8;
/// A pass's pipelines for each shading, and the one clearing a region.
struct PickedPipelines {
    full: wgpu::RenderPipeline,
    clear: wgpu::RenderPipeline,
    basic: wgpu::RenderPipeline,
    opaque: wgpu::RenderPipeline,
}
impl PickedPipelines {
    fn picks(&self) -> Picks<'_> {
        Picks {
            full: &self.full,
            clear: &self.clear,
            basic: &self.basic,
            opaque: &self.opaque,
        }
    }
}
/// Pipelines for each shading, and the one clearing a region (see
/// `GpuRenderer::encode_damage`).
#[derive(Clone, Copy)]
struct Picks<'p> {
    full: &'p wgpu::RenderPipeline,
    clear: &'p wgpu::RenderPipeline,
    basic: &'p wgpu::RenderPipeline,
    opaque: &'p wgpu::RenderPipeline,
}
impl<'p> Picks<'p> {
    fn get(&self, shading: Shading) -> &'p wgpu::RenderPipeline {
        match shading {
            Shading::Full => self.full,
            Shading::Basic => self.basic,
            Shading::Opaque => self.opaque,
        }
    }
}
/// A run of draws encoded as one instanced call.
struct Batch {
    rect: Rect,
    source: (Option<u64>, Option<NodeId>),
    shading: Shading,
    start: u32,
    end: u32,
}
fn quad_bounds(quads: &[Quad]) -> Rect {
    quads
        .iter()
        .map(quad_rect)
        .reduce(|a, b| a.union(b))
        .unwrap_or_default()
}
/// A quad's rectangle. Bands of a split panel measure back from its far
/// edge (`push_panel`): a negative width or height.
fn quad_rect(quad: &Quad) -> Rect {
    let [x, y, w, h] = quad.rect;
    Rect::new(x.min(x + w), y.min(y + h), w.abs(), h.abs())
}
/// Shapes using exactly the same font selection and line metrics as the renderer.
pub fn measure_text(
    fonts: &mut FontSystem,
    text: &str,
    size: f32,
    width: Option<f32>,
) -> (f32, f32) {
    text::ShapedText::new(fonts, text, size, width).size()
}
/// Shared native GPU device, pipelines, and font database. Clone this cheaply for
/// additional windows; render targets/atlases remain owned by each renderer.
#[derive(Clone)]
pub struct GpuContext {
    inner: Rc<GpuContextInner>,
}
struct GpuContextInner {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    fonts: Rc<RefCell<FontSystem>>,
    /// Shaped text shared by layout measurement and every renderer.
    text_cache: Rc<RefCell<prepared::TextCache>>,
    quads: QuadPipelines,
    /// Bind group layouts of the quad pipelines: texture and sampler, viewport.
    texture_layout: wgpu::BindGroupLayout,
    viewport_layout: wgpu::BindGroupLayout,
    /// Vertex and small uniform uploads are CPU-mapped instead of copied.
    mapped_uploads: bool,
    /// For dual-output (single-pass presentation) pipelines.
    quad_shader: wgpu::ShaderModule,
    quad_layout: wgpu::PipelineLayout,
}
impl GpuContext {
    pub fn new() -> Result<Self, GpuError> {
        Self::create(None)
    }
    fn create(window: Option<Arc<winit::window::Window>>) -> Result<Self, GpuError> {
        // Avoid initializing secondary graphics stacks merely to enumerate
        // adapters. On Linux this can create an unused EGL driver and worker
        // pool even when Vulkan is selected; macOS uses Metal.
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(descriptor.with_env());
        let surface = window
            .map(|window| instance.create_surface(window))
            .transpose()
            .map_err(|e| GpuError(e.to_string()))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: surface.as_ref(),
            force_fallback_adapter: false,
        }))
        .map_err(|e| GpuError(e.to_string()))?;
        let info = adapter.get_info();
        // CPU-written vertex buffers avoid a GPU copy per frame; only worth it
        // where CPU and GPU share memory.
        let mapped_uploads = info.device_type == wgpu::DeviceType::IntegratedGpu
            && adapter
                .features()
                .contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("zgui shared device"),
            required_features: if mapped_uploads {
                wgpu::Features::MAPPABLE_PRIMARY_BUFFERS
            } else {
                wgpu::Features::empty()
            },
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }))
        .map_err(|e| GpuError(e.to_string()))?;
        // wgpu clears a buffer of its own while creating the device and holds
        // that command buffer until the first submission. Finish it now, so
        // it is never alive beside a frame's: Metal keeps memory for as many
        // command buffers as were ever alive at once, for good.
        queue.submit([]);
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let (texture_layout, viewport_layout, quad_shader, quad_layout) =
            create_quad_pipelines(&device);
        let quads = QuadPipelines {
            device: device.clone(),
            shader: quad_shader.clone(),
            layout: quad_layout.clone(),
            full: Default::default(),
            full_clear: Default::default(),
            basic: Default::default(),
            basic_clear: Default::default(),
        };
        Ok(Self {
            inner: Rc::new(GpuContextInner {
                instance,
                adapter,
                device,
                queue,
                info,
                fonts: Rc::new(RefCell::new(FontSystem::new())),
                text_cache: Default::default(),
                quads,
                texture_layout,
                viewport_layout,
                quad_shader,
                quad_layout,
                mapped_uploads,
            }),
        })
    }
    pub fn shares_device(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
    pub fn text_system(&self) -> Rc<RefCell<FontSystem>> {
        self.inner.fonts.clone()
    }
    /// Prepared (shaped once) rich text, shared with the renderers.
    pub fn text_cache(&self) -> Rc<RefCell<prepared::TextCache>> {
        self.inner.text_cache.clone()
    }
}
impl GpuError {
    pub fn is_surface_incompatible(&self) -> bool {
        self.0 == "surface is incompatible with the shared GPU adapter"
    }
}
pub struct GpuRenderer {
    context: GpuContext,
    scene_identity: Option<u64>,
    /// Scene content revision the per-node caches were last pruned at.
    pruned_revision: Option<u64>,
    /// The last render's still-open encoder. Present records its blit into it,
    /// so a frame is one command buffer and one submission. Flushed before
    /// anything that rewrites buffers or textures the commands read.
    pending: RefCell<Option<wgpu::CommandEncoder>>,
    /// Viewport uniforms by logical size (f32 bits). Each is written once at
    /// creation; layers and the window switch between them per pass.
    viewports: FxHashMap<(u32, u32, u32), wgpu::BindGroup>,
    /// The raster each canvas, decoration or SVG node currently draws. A
    /// resized or restyled node gets a new raster; the old texture goes at
    /// once instead of waiting for a structural prune.
    node_images: FxHashMap<NodeId, u64>,
    /// The viewport of the target currently being rendered.
    viewport_bind: Option<wgpu::BindGroup>,
    /// Reused upload memory for per-frame vertices; `write_buffer` would
    /// allocate and free a staging buffer every frame.
    belt: RefCell<wgpu::util::StagingBelt>,
    /// Replaces the belt on unified-memory adapters.
    mapped: Option<RefCell<upload::MappedRing>>,
    /// Texture writes awaiting the next render's upload passes (mapped adapters).
    texture_uploads: upload::TextureUploads,
    instance: wgpu::Instance,
    window: Option<Arc<winit::window::Window>>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    surface: Option<wgpu::Surface<'static>>,
    config: Option<wgpu::SurfaceConfiguration>,
    width: u32,
    height: u32,
    scale: f32,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    /// Single-pass presentation alternates two retained targets: each frame
    /// copies `target` and draws its damage into `spare` and the drawable in
    /// one render pass, then swaps. Two passes (damage, then a present blit)
    /// cost a second Metal command buffer per frame. Dropped by `trim`.
    spare: Option<Retained>,
    /// Bind group sampling `target` for the single-pass copy; swapped with
    /// the spare's so neither is rebuilt per frame.
    target_copy_bind: Option<wgpu::BindGroup>,
    deferred: RefCell<Option<Deferred>>,
    single_pass: Option<SinglePass>,
    /// Non-sRGB view format of the drawable. Through it, drawing directly
    /// stores the same bytes the present blit would for opaque windows (whose
    /// pixels all have alpha 1, so post-multiplied presentation is a no-op).
    single_pass_format: Option<wgpu::TextureFormat>,
    /// Rendering an isolated layer's texture, never presented directly.
    in_layer: bool,
    /// The present blit converts to straight alpha (see `unpremultiplies`).
    /// Single-pass presentation then only matches for opaque windows.
    present_unpremultiplies: bool,
    /// Present by copying the retained target into the drawable: same
    /// format, no alpha conversion. A copy is a memcpy for a software
    /// rasterizer, where a full-window shader pass costs as much as drawing.
    copy_present: bool,
    /// Frames whose damage was drawn by `present_single_pass`.
    single_pass_frames: u64,
    /// Frames presented to the drawable alone (`Deferred::direct`).
    direct_frames: u64,
    /// Work encoded at presentation (single-pass frames), reported with the
    /// next `render`'s statistics.
    present_stats: GpuStats,
    /// Quads use the shader their features need (see `push_draw`).
    split_shading: bool,
    /// Texts shaped since the word cache last started over, and the
    /// prepared text cache's count when last read.
    shaped_since_reset: u64,
    cache_shaped: u64,
    bind: wgpu::BindGroup,
    blit: Option<wgpu::RenderPipeline>,
    blit_bind: Option<wgpu::BindGroup>,
    atlas: wgpu::Texture,
    atlas_size: u32,
    entries: FxHashMap<CacheKey, Option<AtlasEntry>>,
    cursor: (u32, u32, u32),
    fonts: Rc<RefCell<FontSystem>>,
    text_cache: Rc<RefCell<prepared::TextCache>>,
    swash: SwashCache,
    layers: FxHashMap<NodeId, LayerCache>,
    layer_origin: (f32, f32),
    shapes: FxHashMap<NodeId, Shaped>,
    shaped_bytes: usize,
    fresh: bool,
    /// Consecutive presented frames whose damage covered the window.
    whole_frames: u32,
    /// The scene flush the retained target was last rendered from.
    rendered_serial: Option<u64>,
    /// Scroll copies for the next root frame: destination pixel rectangle
    /// and the source offset (see `scroll_copies`).
    copies: Vec<ScrollCopy>,
    atlas_epoch: u64,
    frame: u64,
    vertices: wgpu::Buffer,
    vertex_capacity: usize,
    frame_quads: Vec<Quad>,
    /// The frame's draws, kept for their allocation.
    frame_draws: Vec<Draw>,
    background: Color,
    images: FxHashMap<u64, (wgpu::BindGroup, usize)>,
    #[cfg(target_os = "macos")]
    native_surfaces: Option<native_surface::SurfaceCache>,
    /// The next present joins the Core Animation transaction that resized the layer.
    #[cfg(target_os = "macos")]
    transaction_present: bool,
    /// Until when the layer keeps a third drawable after a resize (see
    /// `widen_drawables`).
    #[cfg(target_os = "macos")]
    wide_until: Option<std::time::Instant>,
    /// Presentation through surfaces of the renderer's own (layer_present.rs).
    #[cfg(target_os = "macos")]
    layer_present: Option<LayerPresent>,
    /// Pipelines for frames encoded with Metal directly (`render_direct`);
    /// `None` inside if they could not be built.
    #[cfg(target_os = "macos")]
    metal_frame: std::cell::OnceCell<Option<metal_frame::MetalFrame>>,
    /// A directly encoded frame, committed, until it is known complete.
    #[cfg(target_os = "macos")]
    metal_inflight: RefCell<Option<metal_frame::CommandBuffer>>,
    /// The viewport uniform `select_viewport` chose last, for direct frames.
    #[cfg(target_os = "macos")]
    viewport_values: [f32; 4],
    /// Shifted texel copies drawn in a render pass (copy_offset.wgsl).
    #[cfg(target_os = "macos")]
    copy_offset: Option<wgpu::RenderPipeline>,
    canvases: canvas::CanvasCache,
    svgs: svg::SvgCache,
    /// Small untransformed SVG rasters packed into the glyph atlas by image
    /// id, so icons batch with text instead of binding a texture each.
    image_cells: FxHashMap<u64, (u32, u32)>,
    /// Frame of the last atlas recycle, to detect thrashing.
    atlas_recycled_at: u64,
    /// Compiled on the first backdrop filter.
    blur_pipelines: Option<BlurPipelines>,
    /// Source and intermediate blur targets, shared by the window and every
    /// isolated layer. At least as large as the target being filtered; the
    /// shader clamps samples to the target's own size, passed as a parameter.
    blur_scratch: Option<(wgpu::Texture, wgpu::Texture)>,
}
impl GpuRenderer {
    pub fn new(width: u32, height: u32) -> Result<Self, GpuError> {
        let context = GpuContext::new()?;
        Self::create(None, width, height, &context)
    }
    pub fn for_window(window: Arc<winit::window::Window>) -> Result<Self, GpuError> {
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let context = GpuContext::create(Some(window.clone()))?;
        let mut renderer = Self::create(Some(window), size.width, size.height, &context)?;
        renderer.set_scale_factor(scale);
        Ok(renderer)
    }
    pub fn context(&self) -> GpuContext {
        self.context.clone()
    }
    pub fn new_with_context(
        width: u32,
        height: u32,
        context: &GpuContext,
    ) -> Result<Self, GpuError> {
        Self::create(None, width, height, context)
    }
    pub fn for_window_with_context(
        window: Arc<winit::window::Window>,
        context: &GpuContext,
    ) -> Result<Self, GpuError> {
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let mut renderer = Self::create(Some(window), size.width, size.height, context)?;
        renderer.set_scale_factor(scale);
        Ok(renderer)
    }
    fn create(
        window: Option<Arc<winit::window::Window>>,
        width: u32,
        height: u32,
        context: &GpuContext,
    ) -> Result<Self, GpuError> {
        let instance = context.inner.instance.clone();
        let device = context.inner.device.clone();
        let queue = context.inner.queue.clone();
        let adapter = &context.inner.adapter;
        let info = context.inner.info.clone();
        let surface = window
            .clone()
            .map(|window| instance.create_surface(window))
            .transpose()
            .map_err(|e| GpuError(e.to_string()))?;
        if surface
            .as_ref()
            .is_some_and(|surface| !adapter.is_surface_supported(surface))
        {
            return Err(GpuError(
                "surface is incompatible with the shared GPU adapter".into(),
            ));
        }
        let width = width.max(1);
        let height = height.max(1);
        let config = surface.as_ref().map(|s| {
            let mut c = s
                .get_default_config(adapter, width, height)
                .expect("supported surface");
            let caps = s.get_capabilities(adapter);
            c.alpha_mode = [
                wgpu::CompositeAlphaMode::PreMultiplied,
                wgpu::CompositeAlphaMode::PostMultiplied,
                wgpu::CompositeAlphaMode::Inherit,
                wgpu::CompositeAlphaMode::Opaque,
            ]
            .into_iter()
            .find(|mode| caps.alpha_modes.contains(mode))
            .unwrap_or(c.alpha_mode);
            c.present_mode = wgpu::PresentMode::AutoVsync;
            // Two drawables: measured no frame-rate or CPU cost against three
            // on Apple silicon, and one fewer window-sized buffer (~13 MB at
            // 5K) while animating. Resizing keeps a third (`widen_drawables`).
            if cfg!(target_os = "macos") {
                c.desired_maximum_frame_latency = 1;
            }
            // The retained target already holds sRGB-encoded bytes, which the
            // compositor reads the same from either format. A non-sRGB drawable
            // takes them as they are, where an sRGB one needs a per-frame
            // aliasing view and cannot be framebuffer-only.
            let plain = c.format.remove_srgb_suffix();
            if caps.formats.contains(&plain) {
                c.format = plain;
            } else if c.format.is_srgb() {
                c.view_formats = vec![plain];
            }
            // Off macOS, where drawables must stay framebuffer-only to be
            // cheap, present by copy when the surface allows it.
            if !cfg!(target_os = "macos")
                && c.format == FORMAT
                && !unpremultiplies(&c)
                && caps.usages.contains(wgpu::TextureUsages::COPY_DST)
            {
                c.usage |= wgpu::TextureUsages::COPY_DST;
            }
            s.configure(&device, &c);
            c
        });
        let copy_present = config
            .as_ref()
            .is_some_and(|c| c.usage.contains(wgpu::TextureUsages::COPY_DST));
        // A window on macOS presents surfaces of its own (see `LayerPresent`).
        let layer_presenting =
            cfg!(target_os = "macos") && config.is_some() && info.backend == wgpu::Backend::Metal;
        // Single-pass presentation writes the drawable in a non-sRGB format,
        // storing the retained target's bytes unchanged, exactly what the
        // present blit produces for opaque pixels.
        let single_pass_format = config
            .as_ref()
            .filter(|_| !layer_presenting)
            .map(|c| c.format.remove_srgb_suffix());
        // Presenting surfaces of its own, each frame draws into one of them
        // (`begin_layer_frame`): a window-sized target would only be dropped,
        // and the driver keeps the memory it held.
        let target = if layer_presenting {
            texture(&device, 1, 1, "retained output placeholder")
        } else {
            texture(&device, width, height, "retained output")
        };
        let view = target.create_view(&Default::default());
        #[cfg(target_os = "macos")]
        if !layer_presenting {
            written(&device, &queue, &view);
        }
        let atlas = sampled_texture(
            &device,
            context.inner.mapped_uploads,
            1,
            1,
            "lazy glyph atlas placeholder",
        );
        let split_shading = true;
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas"),
            layout: &context.inner.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &atlas.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(
                        &device.create_sampler(&wgpu::SamplerDescriptor::default()),
                    ),
                },
            ],
        });
        let present_unpremultiplies = config.as_ref().is_some_and(unpremultiplies);
        // Presenting surfaces of its own, the target is never blitted (and a
        // blit bind group would keep the first one alive).
        let blit = config
            .as_ref()
            .filter(|_| !layer_presenting)
            .map(|c| blit_pipeline(&device, c.format, unpremultiplies(c)));
        let blit_bind = blit.as_ref().map(|p| blit_group(&device, p, &view));
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("retained frame vertices"),
            size: 4096,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            context: context.clone(),
            scene_identity: None,
            pruned_revision: None,
            pending: RefCell::new(None),
            viewports: FxHashMap::default(),
            node_images: FxHashMap::default(),
            viewport_bind: None,
            belt: RefCell::new(wgpu::util::StagingBelt::new(device.clone(), 256 * 1024)),
            mapped: context
                .inner
                .mapped_uploads
                .then(|| RefCell::new(upload::MappedRing::new(&device, &queue))),
            texture_uploads: Default::default(),
            instance,
            window,
            device,
            queue,
            info,
            surface,
            config,
            width,
            height,
            scale: 1.,
            target,
            view,
            split_shading,
            shaped_since_reset: 0,
            cache_shaped: 0,
            bind,
            blit,
            blit_bind,
            spare: None,
            target_copy_bind: None,
            deferred: RefCell::new(None),
            single_pass: None,
            single_pass_format,
            #[cfg(target_os = "macos")]
            layer_present: layer_presenting.then(LayerPresent::default),
            #[cfg(target_os = "macos")]
            metal_frame: Default::default(),
            #[cfg(target_os = "macos")]
            metal_inflight: RefCell::new(None),
            #[cfg(target_os = "macos")]
            viewport_values: [0.; 4],
            #[cfg(target_os = "macos")]
            copy_offset: None,
            in_layer: false,
            present_unpremultiplies,
            copy_present,
            single_pass_frames: 0,
            direct_frames: 0,
            present_stats: GpuStats::default(),
            atlas,
            atlas_size: 1,
            entries: FxHashMap::default(),
            cursor: (0, 0, 0),
            fonts: context.text_system(),
            text_cache: context.text_cache(),
            swash: SwashCache::new(),
            layers: FxHashMap::default(),
            layer_origin: (0., 0.),
            shapes: FxHashMap::default(),
            shaped_bytes: 0,
            fresh: true,
            whole_frames: 0,
            rendered_serial: None,
            copies: Vec::new(),
            atlas_epoch: 1,
            frame: 0,
            vertices,
            vertex_capacity: 4096,
            frame_quads: Vec::new(),
            frame_draws: Vec::new(),
            background: Color(0, 0, 0, 0),
            images: FxHashMap::default(),
            #[cfg(target_os = "macos")]
            native_surfaces: None,
            #[cfg(target_os = "macos")]
            transaction_present: false,
            #[cfg(target_os = "macos")]
            wide_until: None,
            canvases: Default::default(),
            svgs: Default::default(),
            blur_pipelines: None,
            image_cells: FxHashMap::default(),
            // As if last recycled long ago: the first fill recycles.
            atlas_recycled_at: 0_u64.wrapping_sub(ATLAS_THRASH_FRAMES),
            blur_scratch: None,
        })
    }
    /// Bytes the native device reports as allocated (Metal only), including
    /// driver-owned resources that the cache counters cannot see.
    pub fn debug_device_allocated_bytes(&self) -> Option<u64> {
        #[cfg(target_os = "macos")]
        {
            use objc2_metal::MTLDevice;
            // SAFETY: The HAL guard only borrows this live wgpu device.
            let metal = unsafe { self.device.as_hal::<wgpu::hal::api::Metal>() }?;
            Some(metal.raw_device().currentAllocatedSize() as u64)
        }
        #[cfg(not(target_os = "macos"))]
        None
    }
    pub fn debug_cache_stats(&self) -> DebugCacheStats {
        DebugCacheStats {
            shaped_nodes: self.shapes.len(),
            shaped_bytes: self.shapes.values().map(Shaped::bytes).sum(),
            cached_quads: self.shapes.values().map(|s| s.quads.len()).sum(),
            atlas_entries: self.entries.len(),
            atlas_bytes: (self.atlas_size * self.atlas_size * 4) as usize,
            swash_images: self.swash.image_cache.len(),
            swash_outlines: self.swash.outline_command_cache.len(),
            image_textures: self.images.len(),
            image_bytes: self.images.values().map(|i| i.1).sum(),
            canvas_raster_bytes: self.canvases.bytes(),
            svg_raster_bytes: self.svgs.bytes(),
            vertex_buffer_bytes: self.vertex_capacity,
            layer_textures: self.layers.len(),
            layer_bytes: self.layers.values().map(|l| l.bytes).sum(),
        }
    }
    pub fn set_background(&mut self, color: Color) {
        if self.background != color {
            self.background = color;
            self.fresh = true;
        }
    }
    pub fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.info.clone()
    }
    pub fn text_system(&self) -> Rc<RefCell<FontSystem>> {
        self.fonts.clone()
    }
    /// Prepared rich text shared with layout measurement.
    pub fn text_cache(&self) -> Rc<RefCell<prepared::TextCache>> {
        self.text_cache.clone()
    }
    /// Install this renderer's text services on `scene`: native shaping,
    /// and measurement from the prepared text it draws from, so layout and
    /// drawing break lines identically. Call `Ui::refresh_text_geometry`
    /// afterwards for a scene with mounted text.
    pub fn install_text(&self, scene: &mut Scene) {
        let fonts = self.text_system();
        scene.set_text_measurer(move |text: &str, size: f32, max: Option<f32>| {
            crate::measure_text(&mut fonts.borrow_mut(), text, size, max)
        });
        let fonts = self.text_system();
        scene.set_font_text_shaper(
            move |text: &str,
                  size: f32,
                  width: Option<f32>,
                  font: &zgui::text_layout::FontStyle|
                  -> Box<dyn zgui::text_layout::TextLayout> {
                Box::new(crate::text::ShapedText::with_font(
                    &mut fonts.borrow_mut(),
                    text,
                    size,
                    width,
                    font,
                ))
            },
        );
        let fonts = self.text_system();
        scene.set_rich_text_shaper(move |rich, width| {
            Box::new(crate::text::ShapedText::with_runs(
                &mut fonts.borrow_mut(),
                rich,
                width,
            ))
        });
        // Prepared text: shaped once, then measured at any width by line
        // breaking alone; the renderer draws from the same lines.
        let (fonts, cache) = (self.text_system(), self.text_cache());
        scene.set_rich_text_measurer(move |rich, width| {
            cache
                .borrow_mut()
                .measure(&mut fonts.borrow_mut(), rich, width)
        });
        // Plain text nodes, from the same prepared text.
        let (fonts, cache) = (self.text_system(), self.text_cache());
        scene.set_font_text_measurer(move |text, size, width, font| {
            cache
                .borrow_mut()
                .measure_plain(&mut fonts.borrow_mut(), text, size, font, width)
        });
        // Apps measuring what they have not mounted keep no glyphs for it.
        let (fonts, cache) = (self.text_system(), self.text_cache());
        scene.set_detached_rich_text_measurer(move |rich, width| {
            cache
                .borrow_mut()
                .measure_detached(&mut fonts.borrow_mut(), rich, width)
        });
    }
    pub fn set_scale_factor(&mut self, scale: f32) {
        let scale = if scale.is_finite() {
            scale.max(0.1)
        } else {
            1.
        };
        if self.scale != scale {
            self.flush_pending();
            self.release_atlas_cells();
            self.scale = scale;
            self.layers.clear();
            self.shapes.clear();
            self.entries.clear();
            self.atlas_epoch = self.atlas_epoch.wrapping_add(1);
            self.cursor = (0, 0, 0);
            self.fresh = true;
        }
    }
    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(present) = &mut self.layer_present {
            present.current = None;
            present.changed.clear();
            present.catch_up.clear();
        }
        self.flush_pending();
        self.layers.clear();
        // Scratch grows on demand; release it when the window shrinks a lot.
        if self
            .blur_scratch
            .as_ref()
            .is_some_and(|(source, _)| source.width() > width * 2 || source.height() > height * 2)
        {
            self.blur_scratch = None;
        }
        self.width = width;
        self.height = height;
        self.spare = None;
        self.target_copy_bind = None;
        // A surface of the layer's becomes the target (`begin_layer_frame`).
        let (target_width, target_height) = if self.layer_present.is_some() {
            (1, 1)
        } else {
            (width, height)
        };
        self.target = texture(&self.device, target_width, target_height, "retained output");
        self.view = self.target.create_view(&Default::default());
        #[cfg(target_os = "macos")]
        if self.layer_present.is_none() {
            written(&self.device, &self.queue, &self.view);
        }
        if let (Some(s), Some(c)) = (&self.surface, &mut self.config) {
            c.width = width;
            c.height = height;
            s.configure(&self.device, c);
        }
        // AppKit shows the resized layer when its transaction commits. A
        // drawable presented outside it lands a frame late, so the window
        // briefly shows stretched or empty content.
        #[cfg(target_os = "macos")]
        {
            self.set_transaction_present(true);
            self.widen_drawables();
        }
        self.blit_bind = self
            .blit
            .as_ref()
            .map(|p| blit_group(&self.device, p, &self.view));
        self.fresh = true;
    }
    /// Render as `render`, without counting `damage` among what the frame
    /// changed: a repaint for comparison (`ZGUI_DAMAGE_CHECK`) must leave
    /// later frames to bring their surfaces up to date as they otherwise would.
    pub fn render_uncounted(
        &mut self,
        scene: &Scene,
        damage: &[Rect],
    ) -> Result<GpuStats, GpuError> {
        #[cfg(target_os = "macos")]
        let changed = self
            .layer_present
            .as_ref()
            .map(|present| present.changed.clone());
        let stats = self.render(scene, damage);
        #[cfg(target_os = "macos")]
        if let (Some(present), Some(changed)) = (self.layer_present.as_mut(), changed) {
            present.changed = changed;
        }
        stats
    }
    pub fn render(&mut self, scene: &Scene, damage: &[Rect]) -> Result<GpuStats, GpuError> {
        let presented = std::mem::take(&mut self.present_stats);
        let mut stats = self.render_inner(scene, damage)?;
        if stats.shaped_nodes > 0 {
            // Words shaped by measurement or this frame are reused by the next
            // shaping frame (streaming text repeats nearly all of them); older
            // ones are dropped so the cache stays the size of visible text.
            // Trimming keeps the table at its peak size; after much shaping
            // (a transcript's history, measured or drawn), start a new one.
            let shaped = self.text_cache.borrow().shaped;
            self.shaped_since_reset +=
                shaped.saturating_sub(self.cache_shaped) + stats.shaped_nodes as u64;
            self.cache_shaped = shaped;
            let mut fonts = self.fonts.borrow_mut();
            if self.shaped_since_reset > SHAPE_RESET {
                fonts.shape_run_cache = Default::default();
                self.shaped_since_reset = 0;
            } else {
                fonts.shape_run_cache.trim(SHAPE_RUN_AGES);
            }
            drop(fonts);
            // Prepared text likewise: what is drawn, and a little of the rest.
            let mut cache = self.text_cache.borrow_mut();
            if cache.needs_trim() {
                cache.trim(self.shapes.values().filter_map(|shaped| shaped.prepared));
            }
        }
        stats.draw_calls += presented.draw_calls;
        stats.instances += presented.instances;
        Ok(stats)
    }
    fn render_inner(&mut self, scene: &Scene, damage: &[Rect]) -> Result<GpuStats, GpuError> {
        loop {
            match self.render_once(scene, damage) {
                Err(error)
                    if error.0 == "glyph atlas capacity exceeded in one frame"
                        && self.atlas_size < MAX_ATLAS =>
                {
                    self.reset_atlas((self.atlas_size * 2).min(MAX_ATLAS))
                }
                result => return result,
            }
        }
    }
    fn render_once(&mut self, scene: &Scene, damage: &[Rect]) -> Result<GpuStats, GpuError> {
        // A frame deferred but never presented draws into the retained target
        // before this one prunes images or layers it may reference.
        self.flush_pending();
        if self.scene_identity != Some(scene.identity()) {
            self.scene_identity = Some(scene.identity());
            self.layers.clear();
            self.shapes.clear();
            self.node_images.clear();
            self.shaped_bytes = 0;
            self.fresh = true;
            self.pruned_revision = None;
        }
        // Caches only go stale when nodes are added, removed or change kind.
        if self.pruned_revision != Some(scene.content_revision()) {
            self.pruned_revision = Some(scene.content_revision());
            self.prune(scene);
        }
        self.layers
            .retain(|id, _| scene.contains(*id) && scene.is_isolated(*id));
        // Scroll moves this frame can copy; the rest repaint their clip.
        let (copies, repaint) = self.scroll_copies(scene, damage);
        self.copies = copies;
        let extended;
        let damage = if repaint.is_empty() {
            damage
        } else {
            extended = [damage, &repaint].concat();
            &extended[..]
        };
        if damage.is_empty() && !self.fresh && self.copies.is_empty() {
            return Ok(GpuStats::default());
        }
        // Visit only subtrees that paint inside the damage the frame will
        // actually repaint. Backdrop blur samples beyond its own subtree, so
        // scenes using it keep the full walk.
        let viewport = Rect::new(
            0.,
            0.,
            self.width as f32 / self.scale,
            self.height as f32 / self.scale,
        );
        let merged = (!self.fresh && !scene.has_backdrop_blur())
            .then(|| merge_damage(damage, viewport, self.scale));
        let items = match &merged {
            Some(regions) => scene.layer_items_within(None, Some(regions)),
            None => scene.layer_items(None),
        };
        let mut layer_stats = GpuStats::default();
        for item in &items {
            if item.isolated && item.effects.opacity > 0. {
                self.prepare_layer(scene, item.id, &mut layer_stats)?;
            }
        }
        let mut stats = self.render_flat(scene, items, damage, merged)?;
        self.rendered_serial = Some(scene.flush_serial());
        stats.layer_repaints = layer_stats.layer_repaints;
        stats.layer_cache_hits = layer_stats.layer_cache_hits;
        stats.layer_texture_allocations = layer_stats.layer_texture_allocations;
        stats.damaged_pixels += layer_stats.damaged_pixels;
        stats.draw_calls += layer_stats.draw_calls;
        stats.render_passes += layer_stats.render_passes;
        stats.blur_passes += layer_stats.blur_passes;
        stats.instances += layer_stats.instances;
        stats.shaped_nodes += layer_stats.shaped_nodes;
        stats.glyph_uploads += layer_stats.glyph_uploads;
        stats.image_uploads += layer_stats.image_uploads;
        stats.native_surface_imports += layer_stats.native_surface_imports;
        stats.native_surface_conversions += layer_stats.native_surface_conversions;
        stats.canvas_rasterizations += layer_stats.canvas_rasterizations;
        stats.svg_rasterizations += layer_stats.svg_rasterizations;
        stats.geometry_rebuilds += layer_stats.geometry_rebuilds;
        stats.vertex_buffer_allocations += layer_stats.vertex_buffer_allocations;
        Ok(stats)
    }
    /// Drop cached shapes, canvases, SVGs and textures of nodes that are gone.
    fn prune(&mut self, scene: &Scene) {
        self.node_images.retain(|id, _| scene.contains(*id));
        self.shapes.retain(|id, _| {
            scene.contains(*id)
                && matches!(
                    scene.kind(*id),
                    NodeKind::Text { .. } | NodeKind::RichText { .. }
                )
        });
        self.shaped_bytes = self.shapes.values().map(Shaped::bytes).sum();
        self.canvases.retain(scene);
        self.svgs.retain(scene);
        if !self.images.is_empty() {
            let mut live_images: std::collections::HashSet<u64> = scene
                .paint_items()
                .filter_map(|item| match item.kind {
                    NodeKind::Image(image) => Some(image.id()),
                    #[cfg(target_os = "macos")]
                    NodeKind::NativeSurface(frame) => Some(frame.id()),
                    _ => None,
                })
                .collect();
            live_images.extend(self.canvases.image_ids());
            live_images.extend(self.svgs.image_ids());
            #[cfg(target_os = "macos")]
            if let Some(cache) = &mut self.native_surfaces {
                cache.retain(&live_images);
            }
            self.images.retain(|id, _| live_images.contains(id));
        }
    }
    fn prepare_layer(
        &mut self,
        scene: &Scene,
        id: NodeId,
        stats: &mut GpuStats,
    ) -> Result<(), GpuError> {
        let revision = scene.layer_revision(id);
        let root = scene.bounds(id);
        if let Some(cache) = self.layers.get_mut(&id)
            && cache.revision == revision
        {
            cache.bounds.x += root.x - cache.root_origin.0;
            cache.bounds.y += root.y - cache.root_origin.1;
            cache.root_origin = (root.x, root.y);
            stats.layer_cache_hits += 1;
            return Ok(());
        }
        // A transparent texel border lets linear sampling preserve fractional
        // translation coverage instead of clamping opaque edge pixels.
        let bounds = scene.layer_bounds(id).expand(1. / self.scale);
        let x = bounds.x;
        let y = bounds.y;
        let width = (bounds.width * self.scale).ceil().max(1.) as u32;
        let height = (bounds.height * self.scale).ceil().max(1.) as u32;
        let bounds = Rect::new(x, y, width as f32 / self.scale, height as f32 / self.scale);
        let items = scene.layer_items(Some(id));
        for item in &items {
            if item.isolated && item.effects.opacity > 0. {
                self.prepare_layer(scene, item.id, stats)?;
            }
        }
        let bytes = width as usize * height as usize * 4;
        let existing = self.layers.get(&id).map_or(0, |l| l.bytes);
        let used: usize = self.layers.values().map(|l| l.bytes).sum();
        if !self.layers.contains_key(&id) && self.layers.len() >= 1024 {
            return Err(GpuError("isolated layers exceed 1024 texture limit".into()));
        }
        if bytes > 64 * 1024 * 1024 || used - existing + bytes > 64 * 1024 * 1024 {
            return Err(GpuError("isolated layers exceed 64 MiB budget".into()));
        }
        if width > self.device.limits().max_texture_dimension_2d
            || height > self.device.limits().max_texture_dimension_2d
        {
            return Err(GpuError(
                "isolated layer exceeds GPU texture dimensions".into(),
            ));
        }
        // Animated layers change size every frame: round allocations up and
        // reuse any texture that fits without wasting more than half of it.
        let limit = self.device.limits().max_texture_dimension_2d;
        let (alloc_w, alloc_h) = (
            width.next_multiple_of(64).min(limit),
            height.next_multiple_of(64).min(limit),
        );
        let existing = self.layers.remove(&id);
        let (texture, bind) = if let Some(cache) = existing.filter(|cache| {
            let (w, h) = (cache.texture.width(), cache.texture.height());
            w >= width
                && h >= height
                && u64::from(w) * u64::from(h) <= 2 * u64::from(alloc_w) * u64::from(alloc_h)
        }) {
            (cache.texture, cache.bind)
        } else {
            let texture = texture(&self.device, alloc_w, alloc_h, "isolated subtree");
            let bind = self.texture_bind(
                &texture.create_view(&Default::default()),
                "isolated subtree",
            );
            stats.layer_texture_allocations += 1;
            (texture, bind)
        };
        let bytes = texture.width() as usize * texture.height() as usize * 4;
        // The content occupies the top-left corner; the quad samples just that.
        let uv = [
            width as f32 / texture.width() as f32,
            height as f32 / texture.height() as f32,
        ];
        let view = texture.create_view(&Default::default());
        let target = std::mem::replace(&mut self.target, texture);
        let old_view = std::mem::replace(&mut self.view, view);
        let old_size = (self.width, self.height);
        self.width = self.target.width();
        self.height = self.target.height();
        let background = self.background;
        self.background = Color(0, 0, 0, 0);
        let fresh = self.fresh;
        self.fresh = true;
        let local: Vec<_> = items
            .into_iter()
            .map(|mut item| {
                item.bounds.x -= bounds.x;
                item.bounds.y -= bounds.y;
                item.clip = item
                    .clip
                    .map(|r| Rect::new(r.x - bounds.x, r.y - bounds.y, r.width, r.height));
                item.mask = item.mask.map(|(r, bands)| {
                    (
                        Rect::new(r.x - bounds.x, r.y - bounds.y, r.width, r.height),
                        bands,
                    )
                });
                item
            })
            .collect();
        // Nested layer images remain world-positioned in their cache; pass the offset
        // separately so the same texture can be composed into any ancestor layer.
        let old_origin = self.layer_origin;
        self.layer_origin = (bounds.x, bounds.y);
        let in_layer = std::mem::replace(&mut self.in_layer, true);
        // `fresh` repaints the whole texture, slack included, so an earlier
        // larger frame never bleeds into sampling at the content's edge.
        let result = self.render_flat(
            scene,
            local,
            &[Rect::new(0., 0., bounds.width, bounds.height)],
            None,
        );
        self.in_layer = in_layer;
        self.layer_origin = old_origin;
        let texture = std::mem::replace(&mut self.target, target);
        let _view = std::mem::replace(&mut self.view, old_view);
        self.width = old_size.0;
        self.height = old_size.1;
        self.background = background;
        self.fresh = fresh;
        let result = result?;
        stats.draw_calls += result.draw_calls;
        stats.render_passes += result.render_passes;
        stats.blur_passes += result.blur_passes;
        stats.instances += result.instances;
        stats.shaped_nodes += result.shaped_nodes;
        stats.glyph_uploads += result.glyph_uploads;
        stats.image_uploads += result.image_uploads;
        stats.canvas_rasterizations += result.canvas_rasterizations;
        stats.svg_rasterizations += result.svg_rasterizations;
        stats.geometry_rebuilds += result.geometry_rebuilds;
        stats.vertex_buffer_allocations += result.vertex_buffer_allocations;
        stats.layer_repaints += 1;
        stats.damaged_pixels += result.damaged_pixels;
        self.layers.insert(
            id,
            LayerCache {
                texture,
                bind,
                bounds,
                revision,
                root_origin: (root.x, root.y),
                bytes,
                uv,
            },
        );
        Ok(())
    }
    fn select_viewport(&mut self, width: f32, height: f32) {
        let key = (width.to_bits(), height.to_bits(), self.scale.to_bits());
        if !self.viewports.contains_key(&key) {
            // Layer sizes come and go; keep the map small.
            if self.viewports.len() >= 32 {
                self.viewports.clear();
            }
            let buffer = upload::init_buffer(
                &self.device,
                self.mapped.is_some(),
                "viewport",
                // Logical size, and device pixels per logical pixel.
                bytemuck::cast_slice(&[width, height, self.scale, 0.]),
                wgpu::BufferUsages::UNIFORM,
            );
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("viewport"),
                layout: &self.context.inner.viewport_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            self.viewports.insert(key, bind);
        }
        self.viewport_bind = self.viewports.get(&key).cloned();
        #[cfg(target_os = "macos")]
        {
            self.viewport_values = [width, height, self.scale, 0.];
        }
    }
    fn texture_bind(&self, view: &wgpu::TextureView, label: &str) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.context.inner.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.device.create_sampler(
                        &wgpu::SamplerDescriptor {
                            mag_filter: wgpu::FilterMode::Linear,
                            min_filter: wgpu::FilterMode::Linear,
                            ..Default::default()
                        },
                    )),
                },
            ],
        })
    }
    /// `merged`: `damage` merged into regions already (`merge_damage`).
    fn render_flat(
        &mut self,
        _scene: &Scene,
        items: Vec<zgui::scene::PaintItem<'_>>,
        damage: &[Rect],
        merged: Option<Vec<Rect>>,
    ) -> Result<GpuStats, GpuError> {
        let mut stats = GpuStats::default();
        let copies = if self.in_layer || self.fresh {
            Vec::new()
        } else {
            std::mem::take(&mut self.copies)
        };
        if damage.is_empty() && !self.fresh && copies.is_empty() {
            return Ok(stats);
        }
        // Earlier commands read the vertex and uniform buffers rewritten below.
        self.flush_pending();
        let viewport = Rect::new(
            0.,
            0.,
            self.width as f32 / self.scale,
            self.height as f32 / self.scale,
        );
        self.select_viewport(viewport.width, viewport.height);
        // Whether the scene's own damage covers the window (not a repaint
        // forced by a stale target): see `Deferred::direct`.
        let whole = damage.iter().any(|d| {
            d.x <= 0.
                && d.y <= 0.
                && d.x + d.width >= viewport.width
                && d.y + d.height >= viewport.height
        });
        let has_blur = items
            .iter()
            .any(|item| item.effects.opacity > 0. && item.effects.blur_radius > 0.);
        // Reconstruct every sampled backdrop pixel before replaying filters. A
        // dependency can reach another filter, so expand to a fixed point.
        let damage = if self.fresh {
            vec![viewport]
        } else if has_blur {
            let dependencies: Vec<_> = items
                .iter()
                .filter_map(|item| {
                    if item.effects.opacity <= 0. || item.effects.blur_radius <= 0. {
                        return None;
                    }
                    let output = item
                        .bounds
                        .intersection(item.clip.unwrap_or(viewport))?
                        .intersection(viewport)?;
                    Some((output, item.effects.blur_radius))
                })
                .collect();
            blur_damage(damage, &dependencies, viewport, self.scale)
        } else {
            merged.unwrap_or_else(|| merge_damage(damage, viewport, self.scale))
        };
        if damage.is_empty() && copies.is_empty() {
            return Ok(stats);
        }
        self.frame = self.frame.wrapping_add(1);
        if self.cursor.1 + self.cursor.2 > self.atlas_size * 3 / 4 || self.entries.len() >= 8192 {
            // Filling again soon after the last recycle means the visible
            // working set does not fit: grow rather than re-rasterize every
            // glyph and icon each time it fills.
            let thrashing = self.frame.wrapping_sub(self.atlas_recycled_at) < ATLAS_THRASH_FRAMES;
            if thrashing && self.atlas_size < MAX_ATLAS && self.entries.len() < 8192 {
                self.reset_atlas((self.atlas_size * 2).min(MAX_ATLAS));
            } else {
                self.release_atlas_cells();
                self.entries.clear();
                self.image_cells.clear();
                self.atlas_epoch = self.atlas_epoch.wrapping_add(1);
                self.cursor = (0, 0, 0);
            }
            self.atlas_recycled_at = self.frame;
        }
        let mut quads = std::mem::take(&mut self.frame_quads);
        quads.clear();
        quads.push(Quad {
            rect: [0., 0., viewport.width, viewport.height],
            uv: [0.; 4],
            color: rgba(self.background, 1.),
            fade: [0.; 4],
            options: [0.; 4],
            shape: [0.; 4],
            border: [0.; 4],
            mask: NO_MASK,
        });
        let mut draws = std::mem::take(&mut self.frame_draws);
        draws.clear();
        for item in items {
            // Retained hidden subtrees must not allocate textures, shape text,
            // or trigger filters until their effective opacity is visible.
            if item.effects.opacity <= 0. {
                continue;
            }
            let node_clip = if !item.isolated
                && matches!(item.kind, NodeKind::Text { .. } | NodeKind::RichText { .. })
            {
                item.clip
                    .unwrap_or(viewport)
                    .intersection(item.bounds)
                    .unwrap_or_default()
            } else {
                item.clip.unwrap_or(viewport)
            };
            let Some(clip) = node_clip.intersection(viewport) else {
                continue;
            };

            if !damage.iter().any(|d| {
                d.intersects(clip)
                    && d.intersects(if item.isolated {
                        let b = self.layers[&item.id].bounds;
                        Rect::new(
                            b.x - self.layer_origin.0,
                            b.y - self.layer_origin.1,
                            b.width,
                            b.height,
                        )
                    } else if let NodeKind::Quad(style) | NodeKind::Panel { quad: style, .. } =
                        item.kind
                    {
                        style.paint_bounds(item.bounds)
                    } else if let NodeKind::Svg(svg) = item.kind {
                        svg.paint_bounds(item.bounds)
                    } else if let NodeKind::Image(image) = item.kind {
                        image.paint_bounds(item.bounds)
                    } else {
                        item.bounds
                    })
            }) {
                continue;
            }
            let start = quads.len() as u32;
            if item.isolated {
                let layer = self.layers.get(&item.id).expect("prepared layer");
                let bounds = Rect::new(
                    layer.bounds.x - self.layer_origin.0,
                    layer.bounds.y - self.layer_origin.1,
                    layer.bounds.width,
                    layer.bounds.height,
                );
                quads.push(Quad {
                    rect: [bounds.x, bounds.y, bounds.width, bounds.height],
                    uv: [0., 0., layer.uv[0], layer.uv[1]],
                    color: [1., 1., 1., item.effects.opacity],
                    fade: [
                        item.bounds.x,
                        item.bounds.y,
                        item.bounds.width,
                        item.bounds.height,
                    ],
                    options: [item.effects.edge_fade, 2., 0., 0.],
                    shape: [0.; 4],
                    border: [0.; 4],
                    mask: item.mask.map_or(NO_MASK, |(rect, [top, bottom])| {
                        [rect.y, rect.y + rect.height, top, bottom]
                    }),
                });
                draws.push(Draw {
                    shading: Shading::Full,
                    start,
                    end: quads.len() as u32,
                    clip,
                    bounds: quad_bounds(&quads[start as usize..]),
                    image: None,
                    layer: Some(item.id),
                    blur: (item.effects.blur_radius > 0.).then_some((item.bounds, item.effects)),
                });
                continue;
            }
            let bounds = item.bounds;
            let fade = [bounds.x, bounds.y, bounds.width, bounds.height];
            let mut canvas_image = None;
            match item.kind {
                NodeKind::Canvas(canvas) => {
                    if bounds.width <= 0. || bounds.height <= 0. {
                        continue;
                    }
                    let before = self.canvases.rasterizations();
                    let (width, height, scale) = (bounds.width, bounds.height, self.scale);
                    let id = self.canvas_texture(item.id, &mut stats, |cache| {
                        cache.get(item.id, canvas, width, height, scale)
                    })?;
                    stats.canvas_rasterizations +=
                        (self.canvases.rasterizations() - before) as usize;
                    canvas_image = Some(id);
                    quads.push(Quad {
                        rect: fade,
                        uv: [0., 0., 1., 1.],
                        color: [1., 1., 1., item.effects.opacity],
                        fade,
                        options: [item.effects.edge_fade, 2., 0., 0.],
                        shape: [0.; 4],
                        border: [0.; 4],
                        mask: NO_MASK,
                    });
                }
                NodeKind::Rect(color) => quads.push(Quad {
                    rect: fade,
                    uv: [0.; 4],
                    color: rgba(*color, item.effects.opacity),
                    fade,
                    options: [item.effects.edge_fade, 0., 0., 0.],
                    shape: [0.; 4],
                    border: [0.; 4],
                    mask: NO_MASK,
                }),
                NodeKind::Quad(style) | NodeKind::Panel { quad: style, .. } => {
                    if style.shadows().len() > 32 {
                        return Err(GpuError("panel exceeds 32 shadows".into()));
                    }
                    for shadow in style.shadows() {
                        let core = bounds.expand(shadow.spread.max(0.));
                        let core = Rect::new(
                            core.x + shadow.offset.x,
                            core.y + shadow.offset.y,
                            core.width,
                            core.height,
                        );
                        let outer = core.expand(shadow.blur_radius.max(0.) * 3.);
                        quads.push(Quad {
                            rect: [outer.x, outer.y, outer.width, outer.height],
                            uv: [0.; 4],
                            color: rgba(shadow.color, item.effects.opacity),
                            fade: [core.x, core.y, core.width, core.height],
                            options: [0., 0., 0., 1.],
                            shape: [
                                style.radius + shadow.spread.max(0.),
                                0.,
                                shadow.blur_radius.max(0.001),
                                0.,
                            ],
                            border: decoration::shadow_corners(
                                style,
                                core.width,
                                core.height,
                                shadow.spread.max(0.),
                            ),
                            mask: NO_MASK,
                        });
                    }
                    // Per-corner radii over a flat fill need no raster: the quad
                    // shader draws per-corner rounded rects. Borders, gradients
                    // and patterns still rasterize.
                    let flat_fill = style.decoration.as_deref().and_then(|detail| {
                        let fill = match &detail.background {
                            None => style.fill,
                            Some(zgui::decoration::Background::Brush(
                                zgui::canvas::Brush::Solid(color),
                            )) => *color,
                            Some(_) => return None,
                        };
                        let widths = detail
                            .border_widths
                            .unwrap_or(zgui::scene::Insets::all(style.border_width));
                        let border = widths
                            .left
                            .max(widths.right)
                            .max(widths.top)
                            .max(widths.bottom);
                        (border <= 0. || style.border_color.3 == 0).then_some(fill)
                    });
                    if let Some(fill) = flat_fill {
                        if let Some(old) = self.node_images.remove(&item.id) {
                            self.images.remove(&old);
                        }
                        push_panel(
                            &mut quads,
                            Quad {
                                rect: fade,
                                uv: [0.; 4],
                                color: rgba(fill, item.effects.opacity),
                                fade,
                                options: [item.effects.edge_fade, 0., 0., 1.],
                                shape: [0., 0., 0., 1.],
                                border: decoration::shadow_corners(
                                    style,
                                    bounds.width,
                                    bounds.height,
                                    0.,
                                ),
                                mask: NO_MASK,
                            },
                            self.scale,
                            self.split_shading,
                        );
                    } else if style.decoration.is_some() && bounds.width > 0. && bounds.height > 0.
                    {
                        let before = self.canvases.rasterizations();
                        let (width, height, scale) = (bounds.width, bounds.height, self.scale);
                        let id = self.canvas_texture(item.id, &mut stats, |cache| {
                            cache.get_decoration(item.id, style, width, height, scale)
                        })?;
                        stats.canvas_rasterizations +=
                            (self.canvases.rasterizations() - before) as usize;
                        canvas_image = Some(id);
                        quads.push(Quad {
                            rect: fade,
                            uv: [0., 0., 1., 1.],
                            color: [1., 1., 1., item.effects.opacity],
                            fade,
                            options: [item.effects.edge_fade, 2., 0., 0.],
                            shape: [0.; 4],
                            border: [0.; 4],
                            mask: NO_MASK,
                        });
                    } else {
                        push_panel(
                            &mut quads,
                            Quad {
                                rect: fade,
                                uv: [0.; 4],
                                color: rgba(style.fill, item.effects.opacity),
                                fade,
                                options: [item.effects.edge_fade, 0., 0., 0.],
                                shape: [style.radius.max(0.), style.border_width.max(0.), 0., 1.],
                                border: rgba(style.border_color, item.effects.opacity),
                                mask: NO_MASK,
                            },
                            self.scale,
                            self.split_shading,
                        );
                    }
                }
                NodeKind::Svg(svg) => {
                    if svg.paint_bounds(bounds).width <= 0. || svg.paint_bounds(bounds).height <= 0.
                    {
                        continue;
                    }
                    let before = self.svgs.rasterizations();
                    let image =
                        self.svgs
                            .get(item.id, svg, bounds.width, bounds.height, self.scale)?;
                    stats.svg_rasterizations += (self.svgs.rasterizations() - before) as usize;
                    // Nodes share SVG rasters: `prune` releases their textures
                    // once no node shows them (a new source prunes).
                    let transform = image.paint_transform(bounds);
                    let transformed = image.transform() != zgui::affine::Affine::IDENTITY;
                    // Icon-sized rasters live in the glyph atlas and batch with
                    // text; larger ones keep a texture of their own.
                    let uv =
                        if image.width() <= ATLAS_IMAGE_MAX && image.height() <= ATLAS_IMAGE_MAX {
                            let (x, y) = match self.image_cells.get(&image.id()) {
                                Some(cell) => *cell,
                                None => {
                                    let (width, height) = (image.width(), image.height());
                                    let cell = self.atlas_cell(width, height, |rgba| {
                                        premultiply_into_cell(rgba, image.pixels(), width);
                                    })?;
                                    self.image_cells.insert(image.id(), cell);
                                    stats.image_uploads += 1;
                                    cell
                                }
                            };
                            let atlas = self.atlas_size as f32;
                            [
                                x as f32 / atlas,
                                y as f32 / atlas,
                                image.width() as f32 / atlas,
                                image.height() as f32 / atlas,
                            ]
                        } else {
                            if !self.images.contains_key(&image.id()) {
                                self.upload_image(&image)?;
                                stats.image_uploads += 1;
                            }
                            canvas_image = Some(image.id());
                            [0., 0., 1., 1.]
                        };
                    quads.push(Quad {
                        rect: fade,
                        uv,
                        color: [1., 1., 1., item.effects.opacity],
                        fade,
                        options: [
                            item.effects.edge_fade,
                            2.,
                            if transformed { 1. } else { 0. },
                            0.,
                        ],
                        shape: if transformed {
                            [transform.a, transform.c, transform.b, transform.d]
                        } else {
                            [0.; 4]
                        },
                        border: if transformed {
                            [transform.tx, transform.ty, 0., 0.]
                        } else {
                            [0.; 4]
                        },
                        mask: NO_MASK,
                    });
                }
                #[cfg(target_os = "macos")]
                NodeKind::NativeSurface(frame) => {
                    if !self.images.contains_key(&frame.id()) {
                        let bytes = frame.width() as usize
                            * frame.height() as usize
                            * if frame.format() == zgui::native_surface::SurfaceFormat::Bgra {
                                4
                            } else {
                                6
                            };
                        if bytes + self.images.values().map(|e| e.1).sum::<usize>()
                            > 64 * 1024 * 1024
                        {
                            return Err(GpuError(
                                "visible image and native surface textures exceed 64 MiB".into(),
                            ));
                        }
                        if self.native_surfaces.is_none() {
                            self.native_surfaces = Some(native_surface::SurfaceCache::new(
                                &self.device,
                                &self.queue,
                            )?);
                        }
                        let cache = self.native_surfaces.as_mut().unwrap();
                        cache.import(frame.clone())?;
                        stats.native_surface_imports += 1;
                        stats.native_surface_conversions += usize::from(
                            frame.format() == zgui::native_surface::SurfaceFormat::Nv12FullRange,
                        );
                        let view = cache.texture(frame.id()).create_view(&Default::default());
                        let bytes = cache.bytes(frame.id());
                        let bind = self.texture_bind(&view, "native CoreVideo frame");
                        self.images.insert(frame.id(), (bind, bytes));
                    }
                    canvas_image = Some(frame.id());
                    quads.push(Quad {
                        rect: fade,
                        uv: [0., 0., 1., 1.],
                        color: [1., 1., 1., item.effects.opacity],
                        fade,
                        options: [item.effects.edge_fade, 2., 0., 0.],
                        shape: [0.; 4],
                        border: [0.; 4],
                        mask: NO_MASK,
                    });
                }
                NodeKind::Image(image) => {
                    if image.paint_bounds(bounds).width <= 0.
                        || image.paint_bounds(bounds).height <= 0.
                    {
                        continue;
                    }
                    if !self.images.contains_key(&image.id()) {
                        self.upload_image(image)?;
                        stats.image_uploads += 1;
                    }
                    let transform = image.paint_transform(bounds);
                    let transformed = image.transform() != zgui::affine::Affine::IDENTITY;
                    // Images have no procedural border/shape. Reuse those
                    // attribute lanes rather than growing every glyph instance.
                    quads.push(Quad {
                        rect: fade,
                        uv: [0., 0., 1., 1.],
                        color: [1., 1., 1., item.effects.opacity],
                        fade,
                        options: [
                            item.effects.edge_fade,
                            2.,
                            if transformed { 1. } else { 0. },
                            0.,
                        ],
                        shape: if transformed {
                            [transform.a, transform.c, transform.b, transform.d]
                        } else {
                            [0.; 4]
                        },
                        border: if transformed {
                            [transform.tx, transform.ty, 0., 0.]
                        } else {
                            [0.; 4]
                        },
                        mask: NO_MASK,
                    });
                }
                kind @ (NodeKind::Text { .. } | NodeKind::RichText { .. }) => {
                    let rich = match kind {
                        NodeKind::RichText { text } => Some(text.clone()),
                        _ => None,
                    };
                    let rich_content = rich.as_ref().map(|r| r.text_arc());
                    let (text, color, font_size) = match kind {
                        NodeKind::Text {
                            text,
                            color,
                            font_size,
                        } => (text, *color, *font_size),
                        NodeKind::RichText { .. } => (
                            rich_content.as_ref().unwrap(),
                            zgui::scene::Color(255, 255, 255, 255),
                            16.,
                        ),
                        _ => unreachable!(),
                    };
                    if !font_size.is_finite() || font_size <= 0. {
                        return Err(GpuError("font size must be finite and positive".into()));
                    }
                    // Colour-only rich changes (fades, highlights) keep glyph
                    // positions: recolour the cached shape instead of re-shaping.
                    if let (Some(new), Some(shaped)) = (&rich, self.shapes.get_mut(&item.id))
                        && let Some(old) = &shaped.rich
                        && !Arc::ptr_eq(old, new)
                        && old.same_shape(new)
                        && shaped.width == bounds.width
                        && shaped.height == bounds.height
                        && shaped.text_options == item.text_options
                        && shaped.font == *item.font
                    {
                        for glyph in &mut shaped.glyphs {
                            glyph.color = Some(new.runs()[glyph.run].color);
                        }
                        for decoration in &mut shaped.decorations {
                            decoration.color = decoration.color_in(new);
                        }
                        shaped.rich = Some(new.clone());
                        shaped.atlas_epoch = self.atlas_epoch.wrapping_sub(1);
                    }
                    let stale = self.shapes.get(&item.id).is_none_or(|s| {
                        s.rich != rich
                            || s.text_options != item.text_options
                            || s.text != *text
                            || s.font != *item.font
                            || s.size != font_size
                            || s.width != bounds.width
                            || s.height != bounds.height
                    });
                    if stale {
                        if let Some(old) = self.shapes.remove(&item.id) {
                            self.shaped_bytes -= old.bytes();
                        }
                        let mut fonts = self.fonts.borrow_mut();
                        // Default-option rich text: the prepared lines layout
                        // measured with, laid out at this width, no reshaping.
                        let prepared = rich
                            .as_ref()
                            .filter(|rich| prepared::TextCache::handles(rich));
                        let plain = rich.is_none()
                            && item.text_options == Default::default()
                            && prepared::TextCache::handles_plain(text, font_size);
                        let b = if plain {
                            let mut cache = self.text_cache.borrow_mut();
                            let laid = cache.layout_plain(
                                &mut fonts,
                                text,
                                font_size,
                                item.font,
                                Some(bounds.width),
                            );
                            let starts = laid.line_starts();
                            let glyphs = glyphs_of(
                                laid.runs(Some(bounds.height)),
                                &starts,
                                None,
                                self.scale,
                            );
                            Err((glyphs, Vec::new(), laid.key()))
                        } else if let Some(rich) = prepared {
                            let mut cache = self.text_cache.borrow_mut();
                            let laid = cache.layout(&mut fonts, rich, Some(bounds.width));
                            let starts = laid.line_starts();
                            let glyphs = glyphs_of(
                                laid.runs(Some(bounds.height)),
                                &starts,
                                Some(rich),
                                self.scale,
                            );
                            let decorations = text::rich_decorations_in(
                                laid.runs(Some(bounds.height)),
                                &starts,
                                rich,
                            );
                            Err((glyphs, decorations, laid.key()))
                        } else if let Some(rich) = &rich {
                            Ok(text::rich_buffer(
                                &mut fonts,
                                rich,
                                Some(bounds.width),
                                Some(bounds.height),
                            ))
                        } else if item.text_options != Default::default() {
                            Ok(text::display_buffer(
                                &mut fonts,
                                text,
                                item.font,
                                font_size,
                                Some(bounds.width),
                                Some(bounds.height),
                                item.text_options,
                            ))
                        } else {
                            let mut b = Buffer::new_empty(Metrics::new(
                                font_size.max(1.),
                                item.font.line_height.resolve(font_size),
                            ));
                            b.set_size(Some(bounds.width), Some(bounds.height));
                            text::set_buffer_text(
                                &mut fonts,
                                &mut b,
                                text,
                                item.font,
                                font_size.max(1.),
                            );
                            b.shape_until_scroll(&mut fonts, false);
                            Ok(b)
                        };
                        let (glyphs, decorations, prepared) = match b {
                            Err((glyphs, decorations, key)) => (glyphs, decorations, Some(key)),
                            Ok(b) => (
                                glyphs_of(
                                    b.layout_runs(),
                                    &text::line_starts(&b),
                                    rich.as_deref(),
                                    self.scale,
                                ),
                                rich.as_ref()
                                    .map_or_else(Vec::new, |rich| text::rich_decorations(&b, rich)),
                                None,
                            ),
                        };
                        let shaped = Shaped {
                            decorations,
                            rich: rich.clone(),
                            text_options: item.text_options,
                            font: item.font.clone(),
                            text: text.clone(),
                            size: font_size,
                            width: bounds.width,
                            height: bounds.height,
                            glyphs,
                            quads: Vec::new(),
                            atlas_epoch: 0,
                            last_used: self.frame,
                            prepared,
                        };
                        self.shaped_bytes += shaped.bytes();
                        self.shapes.insert(item.id, shaped);
                        stats.shaped_nodes += 1;
                    }
                    let mut shaped = self.shapes.remove(&item.id).unwrap();
                    self.shaped_bytes -= shaped.bytes();
                    if shaped.atlas_epoch != self.atlas_epoch {
                        shaped.quads.clear();
                        shaped.quads.extend(
                            shaped
                                .decorations
                                .iter()
                                .filter(|d| d.background)
                                .map(rich_decoration_quad),
                        );
                        for glyph in &shaped.glyphs {
                            if let Some(a) = self.glyph(glyph.key, &mut stats)? {
                                shaped.quads.push(Quad {
                                    rect: [
                                        (glyph.x + a.left as f32) / self.scale,
                                        (glyph.y - a.top as f32) / self.scale,
                                        a.width as f32 / self.scale,
                                        a.height as f32 / self.scale,
                                    ],
                                    uv: [
                                        a.x as f32 / self.atlas_size as f32,
                                        a.y as f32 / self.atlas_size as f32,
                                        a.width as f32 / self.atlas_size as f32,
                                        a.height as f32 / self.atlas_size as f32,
                                    ],
                                    color: glyph.color.map_or([0.; 4], |c| rgba(c, 1.)),
                                    fade: [0.; 4],
                                    options: [0., 1., 0., 0.],
                                    shape: [0.; 4],
                                    border: [0.; 4],
                                    mask: NO_MASK,
                                });
                            }
                        }
                        shaped.quads.extend(
                            shaped
                                .decorations
                                .iter()
                                .filter(|d| !d.background)
                                .map(rich_decoration_quad),
                        );
                        shaped.atlas_epoch = self.atlas_epoch;
                        stats.geometry_rebuilds += 1;
                    }
                    for cached in &shaped.quads {
                        let mut quad = *cached;
                        quad.rect[0] += bounds.x;
                        quad.rect[1] += bounds.y;
                        quad.color = if rich.is_some() {
                            [
                                cached.color[0],
                                cached.color[1],
                                cached.color[2],
                                cached.color[3] * item.effects.opacity,
                            ]
                        } else {
                            rgba(color, item.effects.opacity)
                        };
                        quad.fade = fade;
                        quad.options[0] = item.effects.edge_fade;
                        quads.push(quad);
                    }
                    shaped.last_used = self.frame;
                    let bytes = shaped.bytes();
                    if bytes <= SHAPE_BUDGET {
                        let mut used = self.shaped_bytes;
                        while self.shapes.len() >= 1024 || used + bytes > SHAPE_BUDGET {
                            let Some(id) = self
                                .shapes
                                .iter()
                                .min_by_key(|(_, s)| s.last_used)
                                .map(|(id, _)| *id)
                            else {
                                break;
                            };
                            used -= self.shapes.remove(&id).unwrap().bytes();
                        }
                        self.shaped_bytes = used + bytes;
                        self.shapes.insert(item.id, shaped);
                    }
                }
                NodeKind::Container(_) => {}
            }
            // Under a `fade_edges` container, fade by its bands in the shader.
            if let Some((rect, [top, bottom])) = item.mask {
                let mask = [rect.y, rect.y + rect.height, top, bottom];
                for quad in &mut quads[start as usize..] {
                    if quad.options[2] < 0.5 {
                        quad.mask = mask;
                    }
                }
            }
            // When the clip only culls whole quads, apply it on the CPU and
            // draw under the viewport, so a change of clip never splits a
            // batch: neighbouring text, icons and rows become one draw. Quads
            // it would cut, transformed quads and filters keep the scissor.
            let mut draw_clip = clip;
            let limit = snap_out(clip, self.scale);
            if item.effects.blur_radius <= 0.
                && quads[start as usize..]
                    .iter()
                    .all(|quad| quad.options[2] < 0.5 && !crosses(quad, limit))
            {
                let mut kept = start as usize;
                for index in start as usize..quads.len() {
                    if inside(&quads[index], limit) {
                        quads[kept] = quads[index];
                        kept += 1;
                    }
                }
                quads.truncate(kept);
                draw_clip = viewport;
            }
            if quads.len() as u32 > start || item.effects.blur_radius > 0. {
                let draw = Draw {
                    shading: Shading::Full,
                    layer: None,
                    blur: (item.effects.blur_radius > 0.).then_some((bounds, item.effects)),
                    start,
                    end: quads.len() as u32,
                    clip: draw_clip,
                    bounds: quad_bounds(&quads[start as usize..]),
                    image: if let Some(image) = canvas_image {
                        Some(image)
                    } else if let NodeKind::Image(image) = item.kind {
                        Some(image.id())
                    } else {
                        None
                    },
                };
                push_draw(&mut draws, &quads, draw, self.split_shading, self.scale);
            }
        }
        // The surface this frame draws into, when presenting through the
        // layer: shown surfaces stay as they are.
        #[cfg(target_os = "macos")]
        let shown = if self.in_layer {
            None
        } else {
            self.begin_layer_frame(self.fresh || whole)?;
            self.layer_present
                .as_ref()
                .and_then(|present| present.shown)
        };
        #[cfg(not(target_os = "macos"))]
        let shown: Option<usize> = None;
        let regions: Vec<(Rect, (u32, u32, u32, u32))> = damage
            .iter()
            .filter_map(|region| {
                scissor(*region, self.scale, self.width, self.height).map(|s| (*region, s))
            })
            .collect();
        for (_, (_, _, w, h)) in &regions {
            stats.damaged_pixels += u64::from(*w) * u64::from(*h);
        }
        // What this frame changes in the retained target, for surfaces that
        // must catch up later (`begin_layer_frame`).
        #[cfg(target_os = "macos")]
        if !self.in_layer
            && let Some(present) = &mut self.layer_present
        {
            present.changed.extend(regions.iter().map(|(_, s)| *s));
            present.changed.extend(copies.iter().map(|(s, _)| *s));
        }
        // What the surface lacks: changes since it was last shown that this
        // frame doesn't repaint, and scrolled content; both drawn from the
        // shown surface at the start of the frame's first pass, as instances
        // after the frame's quads.
        #[cfg(target_os = "macos")]
        let mut catch_up = shown.and_then(|shown| {
            let (bind, draws) = self.catch_up_draws(shown, &copies, &regions)?;
            stats.scroll_copies += copies.len();
            let start = quads.len() as u32;
            quads.extend(draws.into_iter().map(|((x, y, w, h), (dx, dy))| Quad {
                rect: [x as f32, y as f32, w as f32, h as f32],
                uv: [dx as f32, dy as f32, 0., 0.],
                ..bytemuck::Zeroable::zeroed()
            }));
            Some((bind, start..quads.len() as u32))
        });
        let needed = quads.len() * std::mem::size_of::<Quad>();
        if needed > VERTEX_BUDGET {
            return Err(GpuError("frame geometry exceeds 32 MiB budget".into()));
        }
        // Plain frames (no filters, images or layers, nothing else recorded)
        // skip wgpu: see `metal_frame`.
        #[cfg(target_os = "macos")]
        if !self.in_layer
            && !has_blur
            && self.mapped.is_some()
            && self.texture_uploads.is_empty()
            && self.pending.get_mut().is_none()
            && (shown.is_some() || copies.is_empty())
            && (self.layer_present.is_some() || self.surface.is_none())
            && self.single_pass_format.is_none()
            && draws
                .iter()
                .all(|draw| draw.image.is_none() && draw.layer.is_none())
            && self
                .metal_frame
                .get_or_init(|| metal_frame::MetalFrame::new(&self.device, &self.queue).ok())
                .is_some()
        {
            let catch_up = catch_up.map(|(_, instances)| instances);
            return Ok(self.render_direct(quads, draws, &regions, catch_up, shown, stats));
        }
        let mut encoder = self.device.create_command_encoder(&Default::default());
        // Glyphs rasterized while building this frame land before any pass
        // samples them. Counted as glyph uploads, not scene render passes.
        self.texture_uploads.encode(&self.device, &mut encoder);
        // Scrolled with surfaces of its own, the copies are drawn below.
        if shown.is_none() && !copies.is_empty() {
            let spare = self.spare_texture();
            for ((x, y, w, h), (sx, sy)) in &copies {
                let extent = wgpu::Extent3d {
                    width: *w,
                    height: *h,
                    depth_or_array_layers: 1,
                };
                let (from_x, from_y) = ((*x as i32 + sx) as u32, (*y as i32 + sy) as u32);
                encoder.copy_texture_to_texture(
                    texel_at(&self.target, from_x, from_y),
                    texel_at(&spare, *x, *y),
                    extent,
                );
                encoder.copy_texture_to_texture(
                    texel_at(&spare, *x, *y),
                    texel_at(&self.target, *x, *y),
                    extent,
                );
                stats.scroll_copies += 1;
            }
        }
        let bytes: &[u8] = bytemuck::cast_slice(&quads);
        let vertices = if let Some(mapped) = &mut self.mapped {
            let mapped = mapped.get_mut();
            let (vertices, allocated) = mapped.write(bytes);
            stats.vertex_buffer_allocations += usize::from(allocated);
            self.vertex_capacity = mapped.bytes();
            vertices
        } else {
            if needed > self.vertex_capacity {
                self.vertex_capacity = needed.next_power_of_two();
                self.vertices = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("retained frame vertices"),
                    size: self.vertex_capacity as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                stats.vertex_buffer_allocations += 1;
            }
            let vertices = self.vertices.clone();
            self.belt
                .get_mut()
                .write_buffer(
                    &mut encoder,
                    &vertices,
                    0,
                    wgpu::BufferSize::new(bytes.len() as u64).expect("background quad"),
                )
                .copy_from_slice(bytes);
            self.belt.get_mut().finish();
            vertices
        };
        // A clean filter elsewhere in the retained target needs no replay and
        // must not force every unrelated quad into its own render pass.
        let has_damaged_blur = has_blur
            && draws.iter().any(|draw| {
                draw.blur.is_some_and(|(bounds, _)| {
                    damage.iter().any(|region| {
                        region
                            .intersection(draw.clip)
                            .and_then(|rect| rect.intersection(bounds))
                            .and_then(|output| scissor(output, self.scale, self.width, self.height))
                            .is_some()
                    })
                })
            });
        // The part of a filter's output inside `region`, if it reaches it.
        let filtered = |draw: &Draw, region: &Rect| {
            let (bounds, _) = draw.blur?;
            region.intersection(draw.clip)?.intersection(bounds)
        };
        // A filter samples everything painted before it, so each filter a
        // damaged region reaches ends a pass. Between filters, every region
        // shares one pass: a scissor per region, draws batched within it.
        let splits: Vec<usize> = if has_damaged_blur {
            (0..draws.len())
                .filter(|&i| {
                    regions
                        .iter()
                        .any(|(region, _)| filtered(&draws[i], region).is_some())
                })
                .collect()
        } else {
            Vec::new()
        };
        // The window's own frame can be drawn at presentation in the same pass
        // as the drawable (`present_single_pass`). With damaged filters, only
        // the draws after the last one wait: everything before it must land
        // in the target for the filter to sample.
        // Under the topmost opaque quad covering a whole region, nothing shows:
        // skip the clear and every quad below it. Filters sample what lies
        // beneath them, so frames that replay one draw everything.
        let first = if splits.is_empty() {
            occluded_from(&quads, &draws, &regions, self.scale)
        } else {
            vec![0; regions.len()]
        };
        let defer = !self.in_layer
            && (!self.present_unpremultiplies || self.background.3 == 255)
            && self.single_pass_format.is_some()
            && !self.copy_present
            && copies.is_empty();
        if !defer && !self.in_layer {
            // Drawn into the target now: no streak of drawable-only frames.
            self.whole_frames = 0;
        }
        let mut from = 0;
        // Each filter's vertical half opens the pass after its horizontal one.
        let mut vertical = None;
        for (segment, end) in splits.iter().copied().chain([draws.len()]).enumerate() {
            if defer && end == draws.len() {
                stats.render_passes += 1;
                let blur = vertical.take();
                let direct = whole && self.whole_frames > 0 && segment == 0 && blur.is_none();
                self.whole_frames = if whole {
                    self.whole_frames.saturating_add(1)
                } else {
                    0
                };
                *self.deferred.get_mut() = Some(Deferred {
                    vertices: vertices.clone(),
                    draws: draws.split_off(from),
                    regions,
                    viewport: self.viewport_bind.clone(),
                    blur,
                    clear: segment == 0,
                    disjoint: !has_blur,
                    first,
                    direct,
                });
                break;
            }
            if segment == 0 || end > from {
                stats.render_passes += 1;
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("damage"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                #[cfg(target_os = "macos")]
                if let Some((bind, instances)) = catch_up.take() {
                    pass.set_pipeline(self.copy_offset.as_ref().expect("made with the draws"));
                    pass.set_bind_group(0, &bind, &[]);
                    pass.set_vertex_buffer(0, vertices.slice(..));
                    pass.draw(0..4, instances);
                    stats.draw_calls += 1;
                }
                if let Some(blur) = vertical.take() {
                    let pipeline = &self.blur_pipelines.as_ref().expect("filtered").vertical;
                    Self::encode_vertical(&mut pass, pipeline, &blur, &mut stats);
                }
                pass.set_vertex_buffer(0, vertices.slice(..));
                pass.set_bind_group(0, &self.bind, &[]);
                pass.set_bind_group(1, self.viewport_bind.as_ref(), &[]);
                let quads = &self.context.inner.quads;
                for (index, (region, (x, y, w, h))) in regions.iter().enumerate() {
                    pass.set_scissor_rect(*x, *y, *w, *h);
                    let first = first[index];
                    if segment == 0 && first == 0 {
                        pass.set_pipeline(quads.clear(self.split_shading));
                        pass.set_bind_group(0, &self.bind, &[]);
                        pass.draw(0..4, 0..1);
                        stats.draw_calls += 1;
                    }
                    self.encode_draws(
                        &mut pass,
                        &|shading| quads.get(shading),
                        *region,
                        &draws[from..end],
                        first,
                        &mut stats,
                    );
                }
            }
            let Some(draw) = draws.get(end) else {
                break;
            };
            let (bounds, effects) = draw.blur.expect("split at a filter");
            let outputs = regions
                .iter()
                .filter_map(|(region, _)| filtered(draw, region));
            vertical = self.blur(&mut encoder, bounds, draw.clip, outputs, effects);
            if let Some(blur) = &vertical {
                stats.draw_calls += blur.scissors.len();
                stats.render_passes += 1;
                stats.blur_passes += 2;
            }
            // The filtered draw's own quads open the next pass.
            from = end;
        }
        *self.pending.get_mut() = Some(encoder);
        self.fresh = false;
        self.frame_quads = quads;
        draws.clear();
        self.frame_draws = draws;
        Ok(stats)
    }
    /// Record this frame with Metal directly (`metal_frame`): the pass the
    /// wgpu path records for a frame of plain draws, committed at once.
    #[cfg(target_os = "macos")]
    fn render_direct(
        &mut self,
        quads: Vec<Quad>,
        mut draws: Vec<Draw>,
        regions: &[(Rect, Scissor)],
        catch_up: Option<std::ops::Range<u32>>,
        shown: Option<usize>,
        mut stats: GpuStats,
    ) -> GpuStats {
        let vertices = {
            use objc2::Message;
            let mapped = self.mapped.as_mut().expect("checked by the caller").get_mut();
            let (_, allocated) = mapped.write(bytemuck::cast_slice(&quads));
            stats.vertex_buffer_allocations += usize::from(allocated);
            self.vertex_capacity = mapped.bytes();
            mapped.last_metal().retain()
        };
        self.whole_frames = 0;
        let first = occluded_from(&quads, &draws, regions, self.scale);
        let committed = {
            let frame = self
                .metal_frame
                .get()
                .and_then(Option::as_ref)
                .expect("checked by the caller");
            // Metal's textures, each HAL guard dropped before the next is
            // taken (wgpu's lock is not reentrant).
            let raw = |texture: &wgpu::Texture| {
                use objc2::Message;
                // SAFETY: The HAL guard only borrows this live texture.
                unsafe { texture.as_hal::<wgpu::hal::api::Metal>() }
                    .expect("a Metal texture")
                    .raw_handle()
                    .retain()
            };
            // The retained target: through the layer, this frame's surface.
            let (target, atlas) = (raw(&self.target), raw(&self.atlas));
            let mut pass = frame.begin(&target, &vertices, self.viewport_values);
            let present = self.layer_present.as_ref();
            if let (Some(instances), Some(present), Some(shown)) = (catch_up, present, shown) {
                pass.texture(&present.buffers[shown].0.metal);
                pass.copies(frame, instances);
                stats.draw_calls += 1;
            }
            pass.texture(&atlas);
            let clear = if self.split_shading {
                Shading::Basic
            } else {
                Shading::Full
            };
            for (index, (region, region_scissor)) in regions.iter().enumerate() {
                pass.scissor(*region_scissor);
                if first[index] == 0 {
                    pass.quads(frame, clear, true, 0..1);
                    stats.draw_calls += 1;
                }
                self.batches(*region, &draws, first[index], |batch| {
                    let Some(rect) = scissor(batch.rect, self.scale, self.width, self.height)
                    else {
                        return;
                    };
                    pass.scissor(rect);
                    pass.quads(frame, batch.shading, false, batch.start..batch.end);
                    stats.draw_calls += 1;
                    stats.instances += (batch.end - batch.start) as usize;
                });
            }
            pass.commit()
        };
        *self.metal_inflight.get_mut() = Some(committed);
        stats.render_passes += 1;
        self.fresh = false;
        self.frame_quads = quads;
        draws.clear();
        self.frame_draws = draws;
        stats
    }
    /// Wait for a directly encoded frame (`render_direct`) to complete.
    #[cfg(target_os = "macos")]
    fn finish_direct(&self) -> bool {
        use objc2_metal::MTLCommandBuffer;
        let Some(buffer) = self.metal_inflight.borrow_mut().take() else {
            return false;
        };
        buffer.waitUntilCompleted();
        if let Some(mapped) = &self.mapped {
            mapped.borrow_mut().completed();
        }
        true
    }
    /// The spare retained texture (single-pass presentation's other target),
    /// created at the window's size if needed.
    fn spare_texture(&mut self) -> wgpu::Texture {
        if self
            .spare
            .as_ref()
            .is_none_or(|t| t.texture.width() != self.width || t.texture.height() != self.height)
        {
            let texture = texture(&self.device, self.width, self.height, "retained output");
            let view = texture.create_view(&Default::default());
            self.spare = Some(Retained {
                texture,
                view,
                copy_bind: None,
                blit_bind: None,
            });
        }
        self.spare.as_ref().expect("created above").texture.clone()
    }
    /// Split the last flush's scroll moves into pixel copies this frame can
    /// apply and clips it must repaint instead. A copy is exact only when the
    /// retained target holds the previous flush, both the clip and the shift
    /// land on whole pixels, and within the clip nothing but the content
    /// changes position: whatever lies under it is hidden by one opaque
    /// rectangle, and anything over it is repainted anyway.
    fn scroll_copies(&self, scene: &Scene, damage: &[Rect]) -> (Vec<ScrollCopy>, Vec<Rect>) {
        let serial = scene.flush_serial();
        let mut copies = Vec::new();
        let mut repaint = Vec::new();
        // On Apple GPUs a process's first texture blit makes the driver hold
        // over 100 MB of graphics memory (see vendor/wgpu-hal/ZGUI_PATCH.md):
        // repaint there, unless presenting surfaces of its own, which copy
        // by drawing from the shown surface. That holds the previous flush
        // only once it was presented.
        #[cfg(target_os = "macos")]
        let copyable = self
            .layer_present
            .as_ref()
            .is_some_and(|present| present.current.is_none());
        #[cfg(not(target_os = "macos"))]
        let copyable = true;
        for scroll in scene.scroll_moves() {
            if scroll.serial != serial {
                continue;
            }
            let whole = |value: f32| {
                let scaled = value * self.scale;
                ((scaled - scaled.round()).abs() < 1e-3).then_some(scaled.round() as i32)
            };
            let pixels = (|| {
                let clip = scroll.clip.intersection(Rect::new(
                    0.,
                    0.,
                    self.width as f32 / self.scale,
                    self.height as f32 / self.scale,
                ))?;
                let (x, y) = (whole(clip.x)?, whole(clip.y)?);
                let (right, bottom) = (whole(clip.x + clip.width)?, whole(clip.y + clip.height)?);
                let (dx, dy) = (whole(scroll.dx)?, whole(scroll.dy)?);
                // Destination: the clip minus the exposed strip; source: the
                // same pixels before the move.
                let (x0, x1) = (x.max(x + dx), right.min(right + dx));
                let (y0, y1) = (y.max(y + dy), bottom.min(bottom + dy));
                (x0 < x1 && y0 < y1 && x0 >= 0 && y0 >= 0).then(|| {
                    (
                        (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32),
                        (-dx, -dy),
                    )
                })
            })();
            match pixels {
                Some(copy)
                    if copyable
                        && !self.fresh
                        && self.rendered_serial == Some(serial - 1)
                        && copy_is_exact(scene, scroll, damage, self.scale) =>
                {
                    copies.push(copy)
                }
                _ => repaint.push(scroll.clip),
            }
        }
        (copies, repaint)
    }
    /// Submit commands recorded by the last render, if any.
    fn flush_pending(&self) {
        #[cfg(target_os = "macos")]
        self.finish_direct();
        let Some(mut pending) = self.pending.borrow_mut().take() else {
            return;
        };
        // Not presented (skipped, hidden or read back): draw it the two-pass
        // way so the retained target is complete.
        if let Some(deferred) = self.deferred.borrow_mut().take() {
            let mut pass = pending.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("damage"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            let mut stats = GpuStats::default();
            if let Some(blur) = &deferred.blur {
                let pipeline = &self.blur_pipelines.as_ref().expect("filtered").vertical;
                Self::encode_vertical(&mut pass, pipeline, blur, &mut stats);
            }
            let quads = &self.context.inner.quads;
            let picks = Picks {
                full: quads.get(Shading::Full),
                clear: quads.clear(false),
                basic: quads.get(Shading::Basic),
                opaque: quads.get(Shading::Opaque),
            };
            self.encode_damage(&mut pass, &deferred, picks, &mut stats);
        }
        self.queue.submit([pending.finish()]);
        self.belt.borrow_mut().recall();
        self.submitted();
        if let Some(mapped) = &self.mapped {
            mapped.borrow_mut().submitted();
        }
    }
    fn submitted(&self) {
        #[cfg(target_os = "macos")]
        if let Some(cache) = &self.native_surfaces {
            cache.submitted();
        }
    }
    /// Clear and redraw every damaged region of a deferred frame.
    fn encode_damage(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        frame: &Deferred,
        picks: Picks<'_>,
        stats: &mut GpuStats,
    ) {
        pass.set_vertex_buffer(0, frame.vertices.slice(..));
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_bind_group(1, frame.viewport.as_ref(), &[]);
        let (clear, quads) = (picks.clear, picks.full);
        let pick = |shading| picks.get(shading);
        let first = |index: usize| frame.first.get(index).copied().unwrap_or(0);
        if !frame.clear {
            pass.set_pipeline(quads);
            for (index, (region, (x, y, w, h))) in frame.regions.iter().enumerate() {
                pass.set_scissor_rect(*x, *y, *w, *h);
                self.encode_draws(pass, &pick, *region, &frame.draws, first(index), stats);
            }
            return;
        }
        if frame.disjoint {
            // One pipeline switch per phase instead of two per region; many
            // small animations each damage a region of their own.
            pass.set_pipeline(clear);
            for (index, (_, (x, y, w, h))) in frame.regions.iter().enumerate() {
                if first(index) == 0 {
                    pass.set_scissor_rect(*x, *y, *w, *h);
                    pass.draw(0..4, 0..1);
                    stats.draw_calls += 1;
                }
            }
            pass.set_pipeline(quads);
            for (index, (region, _)) in frame.regions.iter().enumerate() {
                self.encode_draws(pass, &pick, *region, &frame.draws, first(index), stats);
            }
            return;
        }
        for (index, (region, (x, y, w, h))) in frame.regions.iter().enumerate() {
            pass.set_scissor_rect(*x, *y, *w, *h);
            if first(index) == 0 {
                pass.set_pipeline(clear);
                pass.set_bind_group(0, &self.bind, &[]);
                pass.draw(0..4, 0..1);
                stats.draw_calls += 1;
            }
            pass.set_pipeline(quads);
            self.encode_draws(pass, &pick, *region, &frame.draws, first(index), stats);
        }
    }
    /// One render pass per frame: copy the retained image into the spare
    /// target and `output`, then redraw the damage into both, and swap. Every
    /// pixel matches the two-pass route because `output`'s format needs no
    /// conversion (see `single_pass_format`).
    fn present_single_pass(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &Deferred,
        output: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        if self.single_pass.as_ref().is_none_or(|p| p.format != format) {
            let inner = &self.context.inner;
            let shader = self
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("single-pass copy"),
                    source: wgpu::ShaderSource::Wgsl(include_str!("copy_both.wgsl").into()),
                });
            let copy = self
                .device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("single-pass copy"),
                    layout: None,
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(FORMAT.into()), Some(format.into())],
                    }),
                    multiview_mask: None,
                    cache: None,
                });
            self.target_copy_bind = None;
            if let Some(spare) = &mut self.spare {
                spare.copy_bind = None;
            }
            let both = |blend, basic| {
                quad_pipeline(
                    &self.device,
                    &inner.quad_shader,
                    &inner.quad_layout,
                    blend,
                    Some(format),
                    basic,
                )
            };
            let direct = |blend, basic| {
                drawable_pipeline(
                    &self.device,
                    &inner.quad_shader,
                    &inner.quad_layout,
                    blend,
                    format,
                    basic,
                )
            };
            self.single_pass = Some(SinglePass {
                format,
                copy,
                both: PickedPipelines {
                    full: both(true, false),
                    clear: both(false, false),
                    basic: both(true, true),
                    opaque: both(false, true),
                },
                direct: PickedPipelines {
                    full: direct(true, false),
                    clear: direct(false, false),
                    basic: direct(true, true),
                    opaque: direct(false, true),
                },
            });
        }
        if !frame.regions.is_empty() {
            self.single_pass_frames += 1;
        }
        let clear = wgpu::Operations {
            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            store: wgpu::StoreOp::Store,
        };
        let attachment = |view| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: clear,
            })
        };
        // A frame that clears and redraws the whole target keeps none of the
        // last one: nothing is copied, so it is drawn into the target itself
        // (or the drawable alone, `direct`) and needs no spare.
        let redrawn = frame.clear
            && frame.regions.iter().any(|(_, (x, y, w, h))| {
                (*x, *y) == (0, 0) && *w >= self.width && *h >= self.height
            });
        if frame.direct || redrawn {
            let pipelines = self.single_pass.as_ref().expect("created above");
            let mut stats = GpuStats::default();
            {
                let both = [attachment(&self.view), attachment(output)];
                let alone = [attachment(output)];
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("frame"),
                    color_attachments: if frame.direct { &alone } else { &both },
                    ..Default::default()
                });
                let picks = if frame.direct {
                    pipelines.direct.picks()
                } else {
                    pipelines.both.picks()
                };
                self.encode_damage(&mut pass, frame, picks, &mut stats);
            }
            self.present_stats.draw_calls += stats.draw_calls;
            self.present_stats.instances += stats.instances;
            if frame.direct {
                // The target still holds an earlier frame: the next render
                // repaints everything.
                self.fresh = true;
                self.direct_frames += 1;
            }
            return;
        }
        if self
            .spare
            .as_ref()
            .is_none_or(|t| t.texture.width() != self.width || t.texture.height() != self.height)
        {
            let texture = texture(&self.device, self.width, self.height, "retained output");
            let view = texture.create_view(&Default::default());
            self.spare = Some(Retained {
                texture,
                view,
                copy_bind: None,
                blit_bind: None,
            });
        }
        if frame.blur.is_some() {
            let device = &self.device;
            let blur = self.blur_pipelines.as_mut().expect("filtered");
            blur.both(device, format);
        }
        let pipelines = self.single_pass.as_ref().expect("created above");
        let previous = self
            .target_copy_bind
            .get_or_insert_with(|| blit_group(&self.device, &pipelines.copy, &self.view))
            .clone();
        let spare = self.spare.take().expect("created above");
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[attachment(&spare.view), attachment(output)],
                ..Default::default()
            });
            pass.set_pipeline(&pipelines.copy);
            pass.set_bind_group(0, &previous, &[]);
            pass.draw(0..3, 0..1);
            let mut stats = GpuStats::default();
            if let Some(blur) = &frame.blur {
                let both = self.blur_pipelines.as_ref().and_then(|b| b.both.as_ref());
                let (_, pipeline) = both.expect("created above");
                Self::encode_vertical(&mut pass, pipeline, blur, &mut stats);
            }
            self.encode_damage(&mut pass, frame, pipelines.both.picks(), &mut stats);
            self.present_stats.draw_calls += stats.draw_calls + 1;
            self.present_stats.instances += stats.instances;
        }
        // The spare now holds this frame; the old target becomes the spare,
        // keeping its view and bind groups for when it is drawn into again.
        let Retained {
            texture,
            view,
            copy_bind,
            blit_bind,
        } = spare;
        let old = Retained {
            texture: std::mem::replace(&mut self.target, texture),
            view: std::mem::replace(&mut self.view, view),
            copy_bind: std::mem::replace(&mut self.target_copy_bind, copy_bind),
            blit_bind: std::mem::replace(&mut self.blit_bind, blit_bind),
        };
        self.spare = Some(old);
        if self.blit_bind.is_none() {
            self.blit_bind = self
                .blit
                .as_ref()
                .map(|p| blit_group(&self.device, p, &self.view));
        }
    }
    /// Encode `draws` clipped to `region`. Consecutive draws sharing a
    /// texture and clip become one instanced call; their quads are contiguous,
    /// so paint order holds. A draw entirely outside the region may be folded
    /// into a batch: the scissor discards its quads.
    fn encode_draws<'p>(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        pipelines: &dyn Fn(Shading) -> &'p wgpu::RenderPipeline,
        region: Rect,
        draws: &[Draw],
        first: u32,
        stats: &mut GpuStats,
    ) {
        // The pipeline set on `pass`: the caller's, until a batch sets one.
        let mut current = None;
        self.batches(region, draws, first, |batch| {
            self.encode_batch(pass, pipelines, &mut current, batch, stats)
        });
    }
    /// The draws within `region` from instance `first` on, as batches:
    /// consecutive draws sharing a clip, texture source and shading join.
    fn batches(&self, region: Rect, draws: &[Draw], first: u32, mut emit: impl FnMut(Batch)) {
        let mut batch: Option<Batch> = None;
        // Skipping blending pays for its extra draws only over many pixels:
        // a small region (a scrolled-in strip) blends plain quads in one draw.
        let pixels = region.width * region.height * self.scale * self.scale;
        let unblended = pixels >= SPLIT_PIXELS;
        for draw in draws {
            let shading = match draw.shading {
                Shading::Opaque if !unblended => Shading::Basic,
                shading => shading,
            };
            if !region.intersects(draw.bounds) || draw.end <= draw.start.max(first) {
                continue;
            }
            let start = draw.start.max(first);
            let Some(rect) = region.intersection(draw.clip) else {
                if let Some(done) = batch.take() {
                    emit(done);
                }
                continue;
            };
            let source = (draw.image, draw.layer);
            match &mut batch {
                Some(open)
                    if open.rect == rect && open.source == source && open.shading == shading =>
                {
                    open.end = draw.end;
                }
                _ => {
                    if let Some(done) = batch.take() {
                        emit(done);
                    }
                    batch = Some(Batch {
                        rect,
                        source,
                        shading,
                        start,
                        end: draw.end,
                    });
                }
            }
        }
        if let Some(done) = batch.take() {
            emit(done);
        }
    }
    fn encode_batch<'p>(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        pipelines: &dyn Fn(Shading) -> &'p wgpu::RenderPipeline,
        current: &mut Option<Shading>,
        batch: Batch,
        stats: &mut GpuStats,
    ) {
        let Some((x, y, w, h)) = scissor(batch.rect, self.scale, self.width, self.height) else {
            return;
        };
        if *current != Some(batch.shading) {
            pass.set_pipeline(pipelines(batch.shading));
            *current = Some(batch.shading);
        }
        pass.set_scissor_rect(x, y, w, h);
        let (image, layer) = batch.source;
        pass.set_bind_group(
            0,
            image.and_then(|id| self.images.get(&id)).map_or_else(
                || {
                    layer
                        .and_then(|id| self.layers.get(&id))
                        .map_or(&self.bind, |l| &l.bind)
                },
                |entry| &entry.0,
            ),
            &[],
        );
        pass.draw(0..4, batch.start..batch.end);
        stats.draw_calls += 1;
        stats.instances += (batch.end - batch.start) as usize;
    }
    fn blur(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        bounds: Rect,
        clip: Rect,
        outputs: impl IntoIterator<Item = Rect>,
        effects: zgui::scene::Effects,
    ) -> Option<VerticalBlur> {
        let sigma = (effects.blur_radius * self.scale).clamp(0.1, 64.);
        let radius = (sigma * 3.).ceil() as u32;
        // Samples stay inside the filter's visible pixels (see blur.wgsl).
        let (ax, ay, aw, ah) = bounds
            .intersection(clip)
            .and_then(|area| scissor(area, self.scale, self.width, self.height))?;
        let scissors: Vec<_> = outputs
            .into_iter()
            .filter_map(|output| scissor(output, self.scale, self.width, self.height))
            .collect();
        if scissors.is_empty() {
            return None;
        }
        if self.blur_pipelines.is_none() {
            self.blur_pipelines = Some(BlurPipelines::new(&self.device));
        }
        if self
            .blur_scratch
            .as_ref()
            .is_none_or(|(source, _)| source.width() < self.width || source.height() < self.height)
        {
            // Round up so a slowly growing layer does not reallocate every frame.
            let (current_w, current_h) = self
                .blur_scratch
                .as_ref()
                .map_or((0, 0), |(source, _)| (source.width(), source.height()));
            let width = self.width.max(current_w).next_multiple_of(256);
            let height = self.height.max(current_h).next_multiple_of(256);
            let limit = self.device.limits().max_texture_dimension_2d;
            let (width, height) = (width.min(limit), height.min(limit));
            self.blur_scratch = Some((
                texture(&self.device, width, height, "backdrop source"),
                texture(&self.device, width, height, "horizontal blur"),
            ));
        }
        let pipelines = self.blur_pipelines.as_ref().expect("created above");
        let (source, intermediate) = self.blur_scratch.as_ref().expect("created above");
        let source_view = source.create_view(&Default::default());
        let intermediate_view = intermediate.create_view(&Default::default());
        let uniform = |vertical: bool| {
            let params: [f32; 16] = [
                if vertical { 0. } else { 1. },
                if vertical { 1. } else { 0. },
                sigma,
                if vertical { 1. } else { 0. },
                bounds.x * self.scale,
                bounds.y * self.scale,
                bounds.width * self.scale,
                bounds.height * self.scale,
                effects.opacity,
                effects.edge_fade * self.scale,
                // The filtered target's size: scratch textures may be larger.
                self.width as f32,
                self.height as f32,
                ax as f32,
                ay as f32,
                (ax + aw) as f32,
                (ay + ah) as f32,
            ];
            upload::init_buffer(
                &self.device,
                self.mapped.is_some(),
                "blur parameters",
                bytemuck::cast_slice(&params),
                wgpu::BufferUsages::UNIFORM,
            )
        };
        let texture = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let horizontal = uniform(false);
        let horizontal = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("horizontal blur"),
            layout: &pipelines.horizontal.get_bind_group_layout(0),
            entries: &[
                texture(0, &self.view),
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: horizontal.as_entire_binding(),
                },
            ],
        });
        let vertical = uniform(true);
        let vertical = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vertical blur"),
            layout: &pipelines.layout,
            entries: &[
                texture(0, &intermediate_view),
                texture(1, &source_view),
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: vertical.as_entire_binding(),
                },
            ],
        });
        // Horizontal: read the target, write the blur and the original pixels
        // for every output at once. The target is unchanged until the vertical
        // draws, so outputs sharing scratch texels compute the same values.
        let load = wgpu::Operations {
            load: wgpu::LoadOp::Load,
            store: wgpu::StoreOp::Store,
        };
        let attachment = |view| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: load,
            })
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("horizontal blur"),
            color_attachments: &[attachment(&intermediate_view), attachment(&source_view)],
            ..Default::default()
        });
        pass.set_pipeline(&pipelines.horizontal);
        pass.set_bind_group(0, &horizontal, &[]);
        for &(x, y, width, height) in &scissors {
            // Cover the rows the vertical kernel reaches inside the area.
            let top = y.saturating_sub(radius).max(ay);
            let bottom = (y + height + radius).min(ay + ah);
            pass.set_scissor_rect(x, top, width, bottom - top);
            pass.draw(0..3, 0..1);
        }
        Some(VerticalBlur {
            bind: vertical,
            scissors,
        })
    }
    /// Draw a filter's vertical half, mixing it over the original pixels, at
    /// the start of the pass that follows its horizontal half.
    fn encode_vertical(
        pass: &mut wgpu::RenderPass<'_>,
        pipeline: &wgpu::RenderPipeline,
        blur: &VerticalBlur,
        stats: &mut GpuStats,
    ) {
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &blur.bind, &[]);
        for &(x, y, width, height) in &blur.scissors {
            pass.set_scissor_rect(x, y, width, height);
            pass.draw(0..3, 0..1);
            stats.draw_calls += 1;
        }
    }
    /// Record `id` as `node`'s raster, dropping the texture it replaces.
    fn retire_node_image(&mut self, node: NodeId, id: u64) {
        if let Some(old) = self.node_images.insert(node, id)
            && old != id
        {
            self.images.remove(&old);
        }
    }
    /// The texture of `node`'s canvas raster, uploaded if it is new; the
    /// cache then keeps no pixels. A lost texture rasterizes again.
    fn canvas_texture(
        &mut self,
        node: NodeId,
        stats: &mut GpuStats,
        mut raster: impl FnMut(&mut canvas::CanvasCache) -> Result<canvas::Raster, GpuError>,
    ) -> Result<u64, GpuError> {
        let mut found = raster(&mut self.canvases)?;
        self.retire_node_image(node, found.id);
        if !self.images.contains_key(&found.id) {
            if found.pixels.is_none() {
                self.canvases.forget(node);
                found = raster(&mut self.canvases)?;
            }
            let pixels = found.pixels.expect("fresh raster");
            self.upload_image(&pixels)?;
            stats.image_uploads += 1;
        }
        self.canvases.uploaded(node);
        Ok(found.id)
    }
    fn upload_image(&mut self, image: &zgui::image::ImageData) -> Result<(), GpuError> {
        let bytes = image.pixels().len();
        if bytes + self.images.values().map(|entry| entry.1).sum::<usize>() > 64 * 1024 * 1024 {
            return Err(GpuError(
                "visible image textures exceed the 64 MiB budget".into(),
            ));
        }
        if image.width() > self.device.limits().max_texture_dimension_2d
            || image.height() > self.device.limits().max_texture_dimension_2d
        {
            return Err(GpuError("image exceeds GPU texture dimensions".into()));
        }
        let texture = sampled_texture(
            &self.device,
            self.mapped.is_some(),
            image.width(),
            image.height(),
            "image",
        );
        let mut pixels = image.pixels().to_vec();
        for pixel in pixels.as_chunks_mut::<4>().0 {
            let alpha = u16::from(pixel[3]);
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
            }
        }
        self.write_texels(&texture, (0, 0, image.width(), image.height()), &mut pixels);
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.context.inner.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &texture.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.device.create_sampler(
                        &wgpu::SamplerDescriptor {
                            mag_filter: wgpu::FilterMode::Linear,
                            min_filter: wgpu::FilterMode::Linear,
                            ..Default::default()
                        },
                    )),
                },
            ],
        });
        self.images.insert(image.id(), (bind, bytes));
        Ok(())
    }
    /// Write premultiplied RGBA texels to `(x, y, width, height)` of a
    /// `sampled_texture`, which no submitted frame samples there.
    fn write_texels(
        &mut self,
        target: &wgpu::Texture,
        (x, y, width, height): (u32, u32, u32, u32),
        rgba: &mut [u8],
    ) {
        #[cfg(target_os = "macos")]
        if self.mapped.is_some() {
            upload::write_texels(target, (x, y, width, height), rgba);
            return;
        }
        if self.mapped.is_some() {
            self.texture_uploads
                .push(target, (x, y, width, height), rgba);
        } else {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: target,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
    /// Before reusing atlas cells: the CPU writes them in place (macOS), so
    /// every frame that sampled the old ones must have finished.
    fn release_atlas_cells(&self) {
        if cfg!(target_os = "macos") && self.mapped.is_some() {
            let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        }
    }
    fn reset_atlas(&mut self, size: u32) {
        self.atlas = sampled_texture(
            &self.device,
            self.mapped.is_some(),
            size,
            size,
            "glyph atlas",
        );
        self.atlas_size = size;
        self.bind = self.texture_bind(&self.atlas.create_view(&Default::default()), "glyph atlas");
        self.entries.clear();
        self.image_cells.clear();
        self.cursor = (0, 0, 0);
        self.atlas_epoch = self.atlas_epoch.wrapping_add(1);
    }
    fn glyph(
        &mut self,
        key: CacheKey,
        stats: &mut GpuStats,
    ) -> Result<Option<AtlasEntry>, GpuError> {
        let size = f32::from_bits(key.font_size_bits);
        if !size.is_finite() || size > MAX_ATLAS as f32 {
            return Err(GpuError(
                "glyph font size exceeds atlas raster limit".into(),
            ));
        }
        if let Some(entry) = self.entries.get(&key) {
            return Ok(*entry);
        }
        if self.entries.len() >= 8192 {
            return Err(GpuError(
                "glyph metadata exceeds 8192 entries in one frame".into(),
            ));
        }
        let Some(image) = self
            .swash
            .get_image_uncached(&mut self.fonts.borrow_mut(), key)
        else {
            self.entries.insert(key, None);
            return Ok(None);
        };
        let p = image.placement;
        if p.width == 0 || p.height == 0 {
            self.entries.insert(key, None);
            return Ok(None);
        }
        let cell_width = p.width + 2;
        let (x, y) = self.atlas_cell(p.width, p.height, |rgba| {
            let row_start = |row: u32| ((row + 1) * cell_width + 1) as usize * 4;
            match image.content {
                SwashContent::Mask => {
                    for (row, alphas) in image.data.chunks_exact(p.width as usize).enumerate() {
                        let start = row_start(row as u32);
                        for (texel, alpha) in rgba[start..].chunks_exact_mut(4).zip(alphas) {
                            texel.copy_from_slice(&[255, 255, 255, *alpha]);
                        }
                    }
                }
                SwashContent::Color => {
                    let bytes = p.width as usize * 4;
                    for (row, texels) in image.data.chunks_exact(bytes).enumerate() {
                        let start = row_start(row as u32);
                        rgba[start..start + bytes].copy_from_slice(texels);
                    }
                }
                SwashContent::SubpixelMask => {
                    let bytes = p.width as usize * 4;
                    for (row, pixels) in image.data.chunks_exact(bytes).enumerate() {
                        let start = row_start(row as u32);
                        for (texel, pixel) in rgba[start..]
                            .chunks_exact_mut(4)
                            .zip(pixels.as_chunks::<4>().0)
                        {
                            texel.copy_from_slice(&[
                                255,
                                255,
                                255,
                                pixel[0].max(pixel[1]).max(pixel[2]),
                            ]);
                        }
                    }
                }
            }
        })?;
        let entry = AtlasEntry {
            x,
            y,
            width: p.width,
            height: p.height,
            left: p.left,
            top: p.top,
        };
        self.entries.insert(key, Some(entry));
        stats.glyph_uploads += 1;
        Ok(Some(entry))
    }
    /// Reserve a `width`×`height` atlas cell with a transparent 1-texel border
    /// and upload it; `fill` writes the bordered RGBA cell. Returns the
    /// interior's origin. Linear sampling at fractional positions reads one
    /// texel past the content, and a recycled atlas is not cleared, so shared
    /// gutters could otherwise hold stale texels.
    fn atlas_cell(
        &mut self,
        width: u32,
        height: u32,
        fill: impl FnOnce(&mut [u8]),
    ) -> Result<(u32, u32), GpuError> {
        if self.atlas_size == 1 {
            self.reset_atlas(INITIAL_ATLAS);
        }
        let (cell_width, cell_height) = (width + 2, height + 2);
        if self.cursor.0 + cell_width > self.atlas_size {
            self.cursor.0 = 0;
            self.cursor.1 += self.cursor.2;
            self.cursor.2 = 0;
        }
        if self.cursor.1 + cell_height > self.atlas_size || cell_width > self.atlas_size {
            return Err(GpuError(
                "glyph atlas capacity exceeded in one frame".into(),
            ));
        }
        let mut rgba = vec![0; (cell_width * cell_height * 4) as usize];
        fill(&mut rgba);
        let atlas = self.atlas.clone();
        self.write_texels(
            &atlas,
            (self.cursor.0, self.cursor.1, cell_width, cell_height),
            &mut rgba,
        );
        let origin = (self.cursor.0 + 1, self.cursor.1 + 1);
        self.cursor.0 += cell_width;
        self.cursor.2 = self.cursor.2.max(cell_height);
        Ok(origin)
    }
    /// Present the retained target. Use `present_with_status` when scheduling retries.
    pub fn present(&mut self) -> Result<(), GpuError> {
        self.present_with_status().map(|_| ())
    }
    /// Present without discarding whether native acquisition skipped this frame.
    pub fn present_with_status(&mut self) -> Result<PresentationStatus, GpuError> {
        self.present_with_notify(|| {})
    }
    /// The copies bringing the current surface up to date from the one shown
    /// (`buffers[shown]`): the changes since it was last shown, less what `regions`
    /// repaint and `copies` cover this frame, then the scroll `copies`.
    #[cfg(target_os = "macos")]
    fn catch_up_draws(
        &mut self,
        shown: usize,
        copies: &[ScrollCopy],
        regions: &[(Rect, Scissor)],
    ) -> Option<(wgpu::BindGroup, Vec<ScrollCopy>)> {
        let mut missing = self
            .layer_present
            .as_mut()
            .map(|present| std::mem::take(&mut present.catch_up))
            .unwrap_or_default();
        let mut rest = Vec::new();
        // Whatever this frame repaints or copies over needs nothing first.
        let cuts = regions
            .iter()
            .map(|(_, s)| *s)
            .chain(copies.iter().map(|(s, _)| *s));
        for cut in cuts {
            for rect in missing.drain(..) {
                scissor_minus(rect, cut, &mut rest);
            }
            std::mem::swap(&mut missing, &mut rest);
        }
        // Frames often change the same places: draw each pixel once where
        // one rectangle holds another.
        missing
            .sort_unstable_by_key(|&(_, _, w, h)| std::cmp::Reverse(u64::from(w) * u64::from(h)));
        let mut kept: Vec<Scissor> = Vec::with_capacity(missing.len());
        for rect in missing {
            if !kept.iter().any(|&outer| scissor_holds(outer, rect)) {
                kept.push(rect);
            }
        }
        if kept.is_empty() && copies.is_empty() {
            return None;
        }
        let pipeline = self.copy_offset.get_or_insert_with(|| {
            let shader = self
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("shifted copy"),
                    source: wgpu::ShaderSource::Wgsl(include_str!("copy_offset.wgsl").into()),
                });
            self.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("shifted copy"),
                    layout: None,
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        // A quad's rect (device pixels) and uv (the shift).
                        buffers: &[wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<Quad>() as u64,
                            step_mode: wgpu::VertexStepMode::Instance,
                            attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4],
                        }],
                    },
                    primitive: QUAD_STRIP,
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(FORMAT.into())],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
        });
        // Made once per surface: it is the source every other frame.
        let source = &self.layer_present.as_ref()?.buffers[shown].0;
        let bind = source
            .copy_source
            .get_or_init(|| {
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("shifted copy source"),
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source.view),
                    }],
                })
            })
            .clone();
        let draws = kept
            .into_iter()
            .map(|rect| (rect, (0, 0)))
            .chain(copies.iter().copied())
            .collect();
        Some((bind, draws))
    }
    /// Make a surface the retained target for the frame about to draw,
    /// unless one is: one the window server is done with, with the regions it
    /// lacks of the last frame queued to copy from the shown surface (none
    /// when `whole`: the frame repaints everything).
    #[cfg(target_os = "macos")]
    fn begin_layer_frame(&mut self, whole: bool) -> Result<(), GpuError> {
        let (width, height) = (self.width, self.height);
        let Some(present) = self.layer_present.as_mut() else {
            return Ok(());
        };
        if present.current.is_some() {
            return Ok(());
        }
        if present
            .buffers
            .iter()
            .any(|(b, _)| (b.texture.width(), b.texture.height()) != (width, height))
        {
            // Resized: the layer keeps showing the old surface until the next.
            present.buffers.clear();
            present.shown = None;
            present.history.clear();
        }
        // Certainly free: not the latest, and no use beyond the layer's own
        // record of it (one on screen or queued has more). The most recent of
        // them holds the frame nearest this one.
        let idle = |present: &LayerPresent| -> Vec<usize> {
            let mut idle: Vec<usize> = (0..present.buffers.len())
                .filter(|&i| Some(i) != present.shown && present.buffers[i].0.surface.use_count() <= 1)
                .collect();
            idle.sort_by_key(|&i| std::cmp::Reverse(present.buffers[i].1));
            idle
        };
        let budget = if present.full_rate { LAYER_BUFFERS } else { 2 };
        let mut free = idle(present);
        // Slower again: the third surface goes once free.
        if present.buffers.len() > budget && let Some(&spare) = free.last() {
            present.buffers.remove(spare);
            if let Some(shown) = &mut present.shown
                && *shown > spare
            {
                *shown -= 1;
            }
            free = idle(present);
        }
        // None: ask the layer which it no longer shows nor queues (it holds a
        // surface a while after showing the next; that one is free).
        let pick = |present: &LayerPresent| {
            let busy = present.busy();
            (0..present.buffers.len())
                .filter(|&i| Some(i) != present.shown && !busy[i])
                .max_by_key(|&i| present.buffers[i].1)
        };
        let mut index = free.first().copied().or_else(|| pick(present));
        if index.is_none() && present.buffers.len() < budget {
            present.buffers.push((
                layer_present::Buffer::new(&self.device, width, height)?,
                None,
            ));
            index = Some(present.buffers.len() - 1);
        }
        // Still none: the layer moves on by its next refresh.
        let started = std::time::Instant::now();
        let mut asked = started;
        while index.is_none() && started.elapsed() < std::time::Duration::from_millis(20) {
            std::thread::sleep(std::time::Duration::from_micros(100));
            index = idle(present).first().copied();
            if index.is_none() && asked.elapsed() >= std::time::Duration::from_millis(2) {
                asked = std::time::Instant::now();
                index = pick(present);
            }
        }
        let index = index.unwrap_or_else(|| {
            (0..present.buffers.len())
                .find(|&i| Some(i) != present.shown)
                .expect("more than one surface")
        });
        let held = present.buffers[index].1;
        let latest = present.shown.and_then(|shown| present.buffers[shown].1);
        present.catch_up = match (whole, latest) {
            (true, _) | (false, None) => Vec::new(),
            (false, Some(latest)) if held == Some(latest) => Vec::new(),
            (false, Some(_)) => match held {
                // The changes of every frame since the one it holds.
                Some(held) if present.history.front().is_some_and(|(f, _)| *f <= held + 1) => {
                    present
                        .history
                        .iter()
                        .filter(|(frame, _)| *frame > held)
                        .flat_map(|(_, changed)| changed.iter().copied())
                        .collect()
                }
                _ => vec![(0, 0, width, height)],
            },
        };
        let buffer = &present.buffers[index].0;
        self.target = buffer.texture.clone();
        self.view = buffer.view.clone();
        present.current = Some(index);
        Ok(())
    }
    /// Show the retained frame (the current surface) through the layer.
    #[cfg(target_os = "macos")]
    fn present_layer(
        &mut self,
        before_present: impl FnOnce(),
    ) -> Result<PresentationStatus, GpuError> {
        let mut present = self.layer_present.take().expect("layer presenting");
        if present.layer.is_none() {
            let surface = self.surface.as_ref().expect("a window surface");
            // SAFETY: The HAL guard only borrows this live surface for the call.
            let hal = unsafe { surface.as_hal::<wgpu::hal::api::Metal>() }
                .ok_or_else(|| GpuError("not a Metal surface".into()))?;
            present.layer = Some(layer_present::Layer::attach(&hal.render_layer().lock()));
        }
        let Some(index) = present.current.take() else {
            // Nothing drawn since: the shown frame stands.
            self.layer_present = Some(present);
            return Ok(PresentationStatus::Presented);
        };
        // The window server reads the surface from its next composite: the
        // frame must be complete by then.
        if self.finish_direct() {
            // wgpu still frees what was dropped once its own work is done.
            if present.frame.is_multiple_of(64) {
                let _ = self.device.poll(wgpu::PollType::Poll);
            }
        } else {
            let encoder = self
                .pending
                .get_mut()
                .take()
                .unwrap_or_else(|| self.device.create_command_encoder(&Default::default()));
            let submission = self.queue.submit([encoder.finish()]);
            self.belt.get_mut().recall();
            self.submitted();
            if let Some(mapped) = self.mapped.as_mut() {
                mapped.get_mut().submitted();
            }
            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            });
        }
        before_present();
        let now = std::time::Instant::now();
        let steady = present.full_rate
            || present
                .shown_at
                .is_some_and(|at| now - at < std::time::Duration::from_millis(50));
        present.shown_at = Some(now);
        let layer = present.layer.as_mut().expect("attached above");
        layer.show(&present.buffers[index].0, self.background.3 == 255, steady);
        present.frame += 1;
        let frame = present.frame;
        present.buffers[index].1 = Some(frame);
        present.shown = Some(index);
        let changed = std::mem::take(&mut present.changed);
        present.history.push_back((frame, changed));
        if present.history.len() > LAYER_HISTORY {
            present.history.pop_front();
        }
        self.layer_present = Some(present);
        Ok(PresentationStatus::Presented)
    }
    /// Whether frames come at the display's full rate (animating or
    /// scrolling). On macOS a display layer still holds the surface shown
    /// before the shown one when the next frame starts, so full-rate frames
    /// keep a third surface; slower ones reuse that surface once the layer
    /// shows the next. Elsewhere ignored.
    pub fn set_full_rate(&mut self, #[allow(unused)] full: bool) {
        #[cfg(target_os = "macos")]
        if let Some(present) = &mut self.layer_present {
            present.full_rate = full;
        }
    }
    /// Encode plain frames with Metal directly (the default on macOS) or
    /// always through wgpu: for comparing the two. Elsewhere ignored.
    #[doc(hidden)]
    pub fn set_direct_metal(&mut self, #[allow(unused)] enabled: bool) {
        #[cfg(target_os = "macos")]
        {
            self.flush_pending();
            self.metal_frame = if enabled {
                Default::default()
            } else {
                std::cell::OnceCell::from(None)
            };
        }
    }
    /// Notify the native host immediately before committing an acquired frame.
    /// Skipped acquisition never invokes the callback, so Wayland hosts do not
    /// wait for a frame callback on a surface commit that never happened.
    pub fn present_with_notify(
        &mut self,
        before_present: impl FnOnce(),
    ) -> Result<PresentationStatus, GpuError> {
        if self.surface.is_none() {
            return Ok(PresentationStatus::Offscreen);
        }
        #[cfg(target_os = "macos")]
        if self.layer_present.is_some() {
            return self.present_layer(before_present);
        }
        let frame = match surface::acquire(self)? {
            surface::Acquired::Frame(frame) => frame,
            surface::Acquired::Skipped(status) => {
                // The retained target must still be complete for the retry.
                self.flush_pending();
                return Ok(status);
            }
        };
        let mut encoder = self
            .pending
            .get_mut()
            .take()
            .unwrap_or_else(|| self.device.create_command_encoder(&Default::default()));
        if let Some(deferred) = self.deferred.get_mut().take() {
            let format = self
                .single_pass_format
                .expect("deferred only with a view format");
            let raw = frame.texture.create_view(&wgpu::TextureViewDescriptor {
                format: Some(format),
                ..Default::default()
            });
            self.present_single_pass(&mut encoder, &deferred, &raw, format);
        } else if self.copy_present {
            encoder.copy_texture_to_texture(
                self.target.as_image_copy(),
                frame.texture.as_image_copy(),
                self.target.size(),
            );
        } else {
            let view = frame.texture.create_view(&Default::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(self.blit.as_ref().unwrap());
            pass.set_bind_group(0, self.blit_bind.as_ref().unwrap(), &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.belt.get_mut().recall();
        self.submitted();
        if let Some(mapped) = self.mapped.as_mut() {
            mapped.get_mut().submitted();
        }
        before_present();
        frame.present();
        // Transactional presents block on scheduling; keep them to resizes.
        #[cfg(target_os = "macos")]
        self.set_transaction_present(false);
        #[cfg(target_os = "macos")]
        if self
            .wide_until
            .is_some_and(|until| std::time::Instant::now() >= until)
        {
            self.narrow_drawables();
            // Free the third drawable now rather than when the pool turns over.
            self.release_drawables();
        }
        Ok(PresentationStatus::Presented)
    }
    /// Release memory that only animation needs: filter scratch textures,
    /// idle vertex buffers and, on macOS, Core Animation's pool of window-sized
    /// drawables (two of three, ~13 MB each at 5K). The window keeps showing
    /// its last frame; the next frame recreates whatever it uses. Call when a
    /// window has stopped presenting for a while.
    pub fn trim(&mut self) {
        self.flush_pending();
        self.blur_scratch = None;
        self.spare = None;
        // Idle: only the shown surface is needed; the next frame makes another.
        #[cfg(target_os = "macos")]
        if let Some(present) = &mut self.layer_present
            && present.current.is_none()
            && let Some(shown) = present.shown
        {
            let kept = present.buffers.swap_remove(shown);
            present.buffers = vec![kept];
            present.shown = Some(0);
            if let Some(layer) = &mut present.layer {
                layer.rest(&present.buffers[0].0);
            }
        }
        if let Some(mapped) = &mut self.mapped {
            mapped.get_mut().trim();
        }
        // Word shapes kept for the next frame, and the table they peaked in
        // (streaming fills it): the next frame shapes what it needs again.
        self.fonts.borrow_mut().shape_run_cache = Default::default();
        #[cfg(target_os = "macos")]
        {
            self.narrow_drawables();
            self.release_drawables();
        }
    }
    #[cfg(target_os = "macos")]
    fn release_drawables(&mut self) {
        let Some(surface) = &self.surface else {
            return;
        };
        // SAFETY: The HAL guard only borrows this live surface for the call.
        let Some(hal) = (unsafe { surface.as_hal::<wgpu::hal::api::Metal>() }) else {
            return;
        };
        let layer = hal.render_layer().lock();
        // Changing drawableSize discards pooled drawables; restoring it at once
        // leaves the configuration and the displayed contents untouched.
        // SAFETY: `drawableSize` is a CGSize property of CAMetalLayer.
        unsafe {
            let size: objc2_core_foundation::CGSize = objc2::msg_send![&**layer, drawableSize];
            let nudged = objc2_core_foundation::CGSize::new(size.width, size.height + 1.);
            let _: () = objc2::msg_send![&**layer, setDrawableSize: nudged];
            let _: () = objc2::msg_send![&**layer, setDrawableSize: size];
        }
    }
    /// A resized layer holds its transactional drawable until the window
    /// server shows the new frame size, up to a second later. With two
    /// drawables the next frame waits that long for one (every step of a
    /// drag resize would stall): keep a third until resizing settles.
    #[cfg(target_os = "macos")]
    fn widen_drawables(&mut self) {
        // Each resize configures the surface, which restores two.
        self.set_drawable_count(3);
        self.wide_until = Some(std::time::Instant::now() + WIDE_DRAWABLES);
    }
    #[cfg(target_os = "macos")]
    fn narrow_drawables(&mut self) {
        if self.wide_until.take().is_some() {
            self.set_drawable_count(2);
        }
    }
    #[cfg(target_os = "macos")]
    fn set_drawable_count(&self, count: usize) {
        let Some(surface) = &self.surface else {
            return;
        };
        // SAFETY: The HAL guard only borrows this live surface for the call.
        let Some(hal) = (unsafe { surface.as_hal::<wgpu::hal::api::Metal>() }) else {
            return;
        };
        let layer = hal.render_layer().lock();
        // SAFETY: `maximumDrawableCount` is an NSUInteger property of CAMetalLayer.
        let _: () = unsafe { objc2::msg_send![&**layer, setMaximumDrawableCount: count] };
    }
    #[cfg(target_os = "macos")]
    fn set_transaction_present(&mut self, enabled: bool) {
        if self.transaction_present == enabled {
            return;
        }
        let Some(surface) = &self.surface else {
            return;
        };
        // SAFETY: The HAL guard only borrows this live surface for the call.
        let Some(hal) = (unsafe { surface.as_hal::<wgpu::hal::api::Metal>() }) else {
            return;
        };
        let layer = hal.render_layer().lock();
        // SAFETY: `presentsWithTransaction` is a BOOL property of CAMetalLayer.
        let _: () = unsafe { objc2::msg_send![&**layer, setPresentsWithTransaction: enabled] };
        self.transaction_present = enabled;
    }
    /// Frames whose damage was drawn in the same pass as presentation.
    /// Frames presented to the drawable alone, the retained target untouched.
    pub fn debug_direct_frames(&self) -> u64 {
        self.direct_frames
    }
    pub fn debug_single_pass_frames(&self) -> u64 {
        self.single_pass_frames
    }
    /// Test hook: draw plain quads with the basic shader, or every quad with
    /// the full shader.
    pub fn debug_split_shading(&mut self, split: bool) {
        self.split_shading = split;
    }
    /// Test hook: treat an offscreen `Bgra8Unorm` texture as the drawable, so
    /// single-pass presentation runs without a window.
    pub fn debug_enable_single_pass(&mut self) {
        self.single_pass_format = Some(wgpu::TextureFormat::Bgra8Unorm);
        self.present_unpremultiplies = !cfg!(target_os = "macos");
    }
    /// Test hook: present into a fresh offscreen drawable configured like a
    /// window (`Bgra8UnormSrgb`, non-opaque) and return its
    /// bytes in RGBA order. Single-pass renderers draw the pending frame
    /// through a non-sRGB view; others use the present blit, as on screen.
    pub fn debug_present_offscreen(&mut self) -> Result<Vec<u8>, GpuError> {
        let srgb = wgpu::TextureFormat::Bgra8UnormSrgb;
        let drawable = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen drawable"),
            size: wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: srgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[wgpu::TextureFormat::Bgra8Unorm],
        });
        let mut encoder = self
            .pending
            .get_mut()
            .take()
            .unwrap_or_else(|| self.device.create_command_encoder(&Default::default()));
        if let Some(frame) = self.deferred.get_mut().take() {
            let raw = drawable.create_view(&wgpu::TextureViewDescriptor {
                format: Some(wgpu::TextureFormat::Bgra8Unorm),
                ..Default::default()
            });
            self.present_single_pass(&mut encoder, &frame, &raw, wgpu::TextureFormat::Bgra8Unorm);
        } else {
            let pipeline = blit_pipeline(&self.device, srgb, !cfg!(target_os = "macos"));
            let bind = blit_group(&self.device, &pipeline, &self.view);
            let view = drawable.create_view(&Default::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.submitted();
        if let Some(mapped) = self.mapped.as_mut() {
            mapped.get_mut().submitted();
        }
        let mut pixels = self.read_texture(&drawable)?;
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        Ok(pixels)
    }
    pub fn readback(&self) -> Result<Vec<u8>, GpuError> {
        self.flush_pending();
        self.read_texture(&self.target)
    }
    fn read_texture(&self, texture: &wgpu::Texture) -> Result<Vec<u8>, GpuError> {
        let stride = (self.width * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(stride) * u64::from(self.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| GpuError(e.to_string()))?;
        rx.recv()
            .map_err(|e| GpuError(e.to_string()))?
            .map_err(|e| GpuError(e.to_string()))?;
        let mapped = buffer.get_mapped_range(..);
        let mut pixels = Vec::with_capacity((self.width * self.height * 4) as usize);
        for row in mapped.chunks(stride as usize) {
            pixels.extend_from_slice(&row[..self.width as usize * 4]);
        }
        // Retained textures are BGRA (see `FORMAT`); callers read RGBA.
        if texture.format() == wgpu::TextureFormat::Bgra8Unorm {
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        drop(mapped);
        buffer.unmap();
        Ok(pixels)
    }
}
/// `rect` less `cut`, as up to four rectangles pushed to `out`.
fn scissor_minus(rect: Scissor, cut: Scissor, out: &mut Vec<Scissor>) {
    let (x, y, right, bottom) = (rect.0, rect.1, rect.0 + rect.2, rect.1 + rect.3);
    let (cx, cy, cr, cb) = (cut.0, cut.1, cut.0 + cut.2, cut.1 + cut.3);
    if cx >= right || cr <= x || cy >= bottom || cb <= y {
        out.push(rect);
        return;
    }
    // Full-width bands above and below the cut, then its sides between.
    if cy > y {
        out.push((x, y, rect.2, cy - y));
    }
    if cb < bottom {
        out.push((x, cb, rect.2, bottom - cb));
    }
    let (top, low) = (cy.max(y), cb.min(bottom));
    if cx > x {
        out.push((x, top, cx - x, low - top));
    }
    if cr < right {
        out.push((cr, top, right - cr, low - top));
    }
}
/// Whether `outer` contains `inner`.
fn scissor_holds(outer: Scissor, inner: Scissor) -> bool {
    outer.0 <= inner.0
        && outer.1 <= inner.1
        && outer.0 + outer.2 >= inner.0 + inner.2
        && outer.1 + outer.3 >= inner.1 + inner.3
}
/// A texture draws sample and the CPU writes (glyph cells, images): with
/// `cpu_writes` on macOS, in shared memory written in place
/// (`upload::shared_texture`).
fn sampled_texture(
    device: &wgpu::Device,
    #[allow(unused)] cpu_writes: bool,
    width: u32,
    height: u32,
    label: &str,
) -> wgpu::Texture {
    #[cfg(target_os = "macos")]
    if cpu_writes {
        return upload::shared_texture(device, width, height, label);
    }
    texture(device, width, height, label)
}
/// Clear a new target through wgpu, so wgpu knows it is written: frames drawn
/// with Metal directly (`render_direct`) write it behind wgpu's back, and wgpu
/// zeroes a texture it has never written on that texture's first use.
#[cfg(target_os = "macos")]
fn written(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("new target"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    queue.submit([encoder.finish()]);
}
fn texture(device: &wgpu::Device, width: u32, height: u32, label: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}
fn rich_decoration_quad(d: &text::RichDecoration) -> Quad {
    Quad {
        rect: [d.bounds.x, d.bounds.y, d.bounds.width, d.bounds.height],
        color: rgba(d.color, 1.),
        uv: [0.; 4],
        fade: [0.; 4],
        options: [0.; 4],
        shape: [0.; 4],
        border: [0.; 4],
        mask: NO_MASK,
    }
}
fn rgba(c: Color, opacity: f32) -> [f32; 4] {
    [
        c.0 as f32 / 255.,
        c.1 as f32 / 255.,
        c.2 as f32 / 255.,
        c.3 as f32 / 255. * opacity,
    ]
}
/// Quads into the retained target, or with `drawable` also into a second,
/// presentation target in the same pass (`fs_both`).
/// The full quad shader drawing into a drawable of `format` alone
/// (`Deferred::direct`).
fn drawable_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    blend: bool,
    format: wgpu::TextureFormat,
    basic: bool,
) -> wgpu::RenderPipeline {
    let all = wgpu::vertex_attr_array![0=>Float32x4,1=>Float32x4,2=>Float32x4,3=>Float32x4,4=>Float32x4,5=>Float32x4,6=>Float32x4,7=>Float32x4];
    let basic_attributes = [all[0], all[1], all[2], all[4]];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("quads to the drawable"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(if basic { "vs_basic" } else { "vs" }),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: 128,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: if basic { &basic_attributes } else { &all },
            }],
        },
        primitive: QUAD_STRIP,
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(if basic { "fs_basic" } else { "fs" }),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: blend.then_some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
fn quad_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    blend: bool,
    drawable: Option<wgpu::TextureFormat>,
    basic: bool,
) -> wgpu::RenderPipeline {
    let blend = blend.then_some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
    let all = wgpu::vertex_attr_array![0=>Float32x4,1=>Float32x4,2=>Float32x4,3=>Float32x4,4=>Float32x4,5=>Float32x4,6=>Float32x4,7=>Float32x4];
    // `basic` reads rect, uv, colour and options only.
    let basic_attributes = [all[0], all[1], all[2], all[4]];
    let target = |format| {
        Some(wgpu::ColorTargetState {
            format,
            blend,
            write_mask: wgpu::ColorWrites::ALL,
        })
    };
    let targets = [target(FORMAT), drawable.and_then(target)];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("quads"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(if basic { "vs_basic" } else { "vs" }),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: 128,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: if basic { &basic_attributes } else { &all },
            }],
        },
        primitive: QUAD_STRIP,
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            // Single-pass presentation draws with the full shader alone.
            entry_point: Some(match (basic, drawable.is_some()) {
                (true, true) => "fs_basic_both",
                (true, false) => "fs_basic",
                (false, true) => "fs_both",
                (false, false) => "fs",
            }),
            compilation_options: Default::default(),
            targets: if drawable.is_some() {
                &targets
            } else {
                &targets[..1]
            },
        }),
        multiview_mask: None,
        cache: None,
    })
}
/// Whether presentation must convert premultiplied pixels to straight alpha.
/// wgpu reports Metal layers as "post-multiplied" merely because they are not
/// opaque, but Core Animation composites `CAMetalLayer` contents as
/// premultiplied (measured: straight 50% red over black shows twice as bright).
fn unpremultiplies(config: &wgpu::SurfaceConfiguration) -> bool {
    config.alpha_mode == wgpu::CompositeAlphaMode::PostMultiplied && !cfg!(target_os = "macos")
}
fn blit_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    post_alpha: bool,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("present"),
        source: wgpu::ShaderSource::Wgsl(
            (format!(
                "const OUTPUT_SRGB:bool={};\nconst POST_ALPHA:bool={};\n{}",
                format.is_srgb(),
                post_alpha,
                include_str!("blit.wgsl")
            ))
            .into(),
        ),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("present"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
fn blit_group(
    device: &wgpu::Device,
    pipeline: &wgpu::RenderPipeline,
    view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("present"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(view),
        }],
    })
}
/// `r` grown to whole physical pixels, as `scissor` rounds it.
fn snap_out(r: Rect, scale: f32) -> Rect {
    let x = (r.x * scale).floor() / scale;
    let y = (r.y * scale).floor() / scale;
    let right = ((r.x + r.width) * scale).ceil() / scale;
    let bottom = ((r.y + r.height) * scale).ceil() / scale;
    Rect::new(x, y, right - x, bottom - y)
}
/// Whether `quad` lies entirely within `clip`.
fn inside(quad: &Quad, clip: Rect) -> bool {
    let Rect {
        x,
        y,
        width: w,
        height: h,
    } = quad_rect(quad);
    x >= clip.x && y >= clip.y && x + w <= clip.x + clip.width && y + h <= clip.y + clip.height
}
/// Whether `clip` would cut `quad` rather than keep or cull it whole.
fn crosses(quad: &Quad, clip: Rect) -> bool {
    let Rect {
        x,
        y,
        width: w,
        height: h,
    } = quad_rect(quad);
    let outside =
        x + w <= clip.x || y + h <= clip.y || x >= clip.x + clip.width || y >= clip.y + clip.height;
    !outside && !inside(quad, clip)
}
fn scissor(r: Rect, scale: f32, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    let x = (r.x * scale).floor().max(0.) as u32;
    let y = (r.y * scale).floor().max(0.) as u32;
    let right = ((r.x + r.width) * scale).ceil().min(width as f32) as u32;
    let bottom = ((r.y + r.height) * scale).ceil().min(height as f32) as u32;
    (right > x && bottom > y).then(|| (x, y, right - x, bottom - y))
}
// Each activated filter contributes its complete clipped output and sampling
// halo. Bounding merges may touch previously unrelated filters; rescan until
// all such dependencies are included. Each filter activates at most once.
fn blur_damage(damage: &[Rect], filters: &[(Rect, f32)], viewport: Rect, scale: f32) -> Vec<Rect> {
    let mut regions = merge_damage(damage, viewport, scale);
    let mut dependencies: Vec<_> = filters
        .iter()
        .filter_map(|(output, _radius)| {
            let width = (viewport.width * scale).round() as u32;
            let height = (viewport.height * scale).round() as u32;
            let output = output.intersection(viewport)?;
            // A filter samples only its own visible pixels, so damage
            // anywhere in them re-blurs all of them, and nothing beyond.
            let (x, y, w, h) = scissor(output, scale, width, height)?;
            let (left, top, right, bottom) = (x, y, x + w, y + h);
            let dependency = Rect::new(
                left as f32 / scale,
                top as f32 / scale,
                (right - left) as f32 / scale,
                (bottom - top) as f32 / scale,
            );
            Some((dependency, false))
        })
        .collect();
    loop {
        let mut changed = false;
        for (dependency, processed) in &mut dependencies {
            if !*processed && regions.iter().any(|region| region.intersects(*dependency)) {
                regions.push(*dependency);
                regions = merge_damage(&regions, viewport, scale);
                *processed = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    regions
}
fn rounded_damage(rect: Rect, viewport: Rect, scale: f32) -> Option<Rect> {
    let rect = rect.intersection(viewport)?;
    let x = (rect.x * scale).floor() / scale;
    let y = (rect.y * scale).floor() / scale;
    Some(Rect::new(
        x,
        y,
        ((rect.x + rect.width) * scale).ceil() / scale - x,
        ((rect.y + rect.height) * scale).ceil() / scale - y,
    ))
}
/// Push a panel quad. An opaque one with rounded corners or a border paints
/// exactly its fill colour away from its edges (in `shade`: past the corner
/// radius, the border and the antialiasing band of at most two device
/// pixels): that interior goes as a flat opaque quad, drawn unblended so the
/// GPU skips shading whatever it covers, and only the four bands around it
/// need the distance field and blending. Every band keeps the
/// panel's own outer edges (the right and bottom ones measured back from the
/// far edge), so the same pixels are covered and antialiased; the seams
/// between pieces lie on whole device pixels, where rounding cannot move a
/// pixel centre across them.
fn push_panel(quads: &mut Vec<Quad>, quad: Quad, scale: f32, split: bool) {
    let radius = if quad.options[3] > 0.5 {
        quad.border.iter().copied().fold(0., f32::max)
    } else {
        quad.shape[0]
    };
    let band = radius.max(quad.shape[1] + 1.).max(0.) + 1.;
    let [x, y, width, height] = quad.rect;
    let (right, bottom) = (x + width, y + height);
    // The interior, on device pixels inside the bands.
    let (left, top) = (
        ((x + band) * scale).ceil() / scale,
        ((y + band) * scale).ceil() / scale,
    );
    let (inner_right, inner_bottom) = (
        ((right - band) * scale).floor() / scale,
        ((bottom - band) * scale).floor() / scale,
    );
    let (inner_width, inner_height) = (inner_right - left, inner_bottom - top);
    if !split
        || quad.color[3] < 1.
        || quad.options[..3] != [0.; 3]
        || quad.shape[2] > 0.
        || quad.shape[3] <= 0.5
        || quad.rect != quad.fade
        || inner_width <= 0.
        || inner_height <= 0.
        || inner_width * inner_height < PANEL_INTERIOR
    {
        quads.push(quad);
        return;
    }
    for rect in [
        [x, y, width, top - y],
        [x, bottom, width, inner_bottom - bottom],
        [x, top, left - x, inner_height],
        [right, top, inner_right - right, inner_height],
    ] {
        quads.push(Quad { rect, ..quad });
    }
    // Last, and opaque: drawn without blending, it hides what lies beneath
    // (the GPU skips shading fragments an opaque one covers), as the whole
    // panel's blended shader could not.
    let interior = [left, top, inner_width, inner_height];
    quads.push(Quad {
        rect: interior,
        fade: interior,
        options: [0.; 4],
        shape: [0.; 4],
        border: [0.; 4],
        ..quad
    });
}
/// Panels split only with an interior of at least this many logical pixels:
/// below, five quads cost more than shading one.
const PANEL_INTERIOR: f32 = 1024.;
/// Quads draw as four-vertex triangle strips, one per instance.
pub(crate) const QUAD_STRIP: wgpu::PrimitiveState = wgpu::PrimitiveState {
    topology: wgpu::PrimitiveTopology::TriangleStrip,
    strip_index_format: None,
    front_face: wgpu::FrontFace::Ccw,
    cull_mode: None,
    unclipped_depth: false,
    polygon_mode: wgpu::PolygonMode::Fill,
    conservative: false,
};
/// Regions a frame draws at most: each draws every batch it touches again.
const MAX_REGIONS: usize = 16;
/// Damage in at most `MAX_REGIONS` groups, each the bounds of its members:
/// rectangles sharing a cell of a grid whose cells double until few enough.
fn cluster_damage(damage: &[Rect], viewport: Rect) -> Vec<Rect> {
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut cell = 16_f32;
    loop {
        let columns = (viewport.width / cell).ceil().max(1.) as usize;
        let rows = (viewport.height / cell).ceil().max(1.) as usize;
        let mut owner = vec![usize::MAX; columns * rows];
        let mut parent: Vec<usize> = (0..damage.len()).collect();
        for (i, r) in damage.iter().enumerate() {
            let first =
                |value: f32, count: usize| ((value / cell).floor().max(0.) as usize).min(count);
            let end =
                |value: f32, count: usize| ((value / cell).ceil().max(0.) as usize).min(count);
            for row in first(r.y, rows)..end(r.y + r.height, rows) {
                for column in first(r.x, columns)..end(r.x + r.width, columns) {
                    let slot = &mut owner[row * columns + column];
                    if *slot == usize::MAX {
                        *slot = i;
                    } else {
                        let (a, b) = (root(&mut parent, i), root(&mut parent, *slot));
                        parent[a] = b;
                    }
                }
            }
        }
        let mut bounds: Vec<Option<Rect>> = vec![None; damage.len()];
        for (i, r) in damage.iter().enumerate() {
            let group = &mut bounds[root(&mut parent, i)];
            *group = Some(group.map_or(*r, |g| g.union(*r)));
        }
        let groups: Vec<Rect> = bounds.into_iter().flatten().collect();
        if groups.len() <= MAX_REGIONS || cell >= viewport.width.max(viewport.height) {
            return groups;
        }
        cell *= 2.;
    }
}
fn merge_damage(damage: &[Rect], viewport: Rect, scale: f32) -> Vec<Rect> {
    // Each region redraws every batch it touches: nearby small ones (the
    // dots of a spinner) are fewer draws as one, for a few more pixels. The
    // pixels gained are bounded by the smaller region, so a large region
    // doesn't swallow every small one in reach, growing as it goes.
    let area = |r: Rect| r.width * r.height * scale * scale;
    let close = |a: Rect, b: Rect| {
        let (a, b) = (area(a), area(b));
        a + b + a.min(b) * 0.5 + NEAR_PIXELS
    };
    let close = |a: Rect, b: Rect| a.intersects(b) || area(a.union(b)) <= close(a, b);
    let mut regions: Vec<Rect> = Vec::new();
    let rounded: Vec<Rect>;
    let damage = if damage.len() > MAX_REGIONS {
        rounded = cluster_damage(damage, viewport);
        &rounded[..]
    } else {
        damage
    };
    for r in damage {
        let Some(mut r) = rounded_damage(*r, viewport, scale) else {
            continue;
        };
        let mut i = 0;
        while i < regions.len() {
            if close(regions[i], r) {
                r = r.union(regions.swap_remove(i));
                i = 0;
            } else {
                i += 1;
            }
        }
        regions.push(r);
    }
    regions
}

pub mod assets;

pub mod prepared;
pub mod text;

fn create_quad_pipelines(
    device: &wgpu::Device,
) -> (
    wgpu::BindGroupLayout,
    wgpu::BindGroupLayout,
    wgpu::ShaderModule,
    wgpu::PipelineLayout,
) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("quads"),
        source: wgpu::ShaderSource::Wgsl(include_str!("draw.wgsl").into()),
    });
    let group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("quads"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let viewport_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("viewport"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("quads"),
        bind_group_layouts: &[Some(&group_layout), Some(&viewport_layout)],
        immediate_size: 0,
    });
    (group_layout, viewport_layout, shader, layout)
}

impl surface::SurfaceSource for GpuRenderer {
    type Frame = wgpu::SurfaceTexture;
    fn acquire(&mut self) -> Result<surface::Acquisition<Self::Frame>, GpuError> {
        use surface::Acquisition;
        Ok(match self.surface.as_ref().unwrap().get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Acquisition::Frame(frame),
            wgpu::CurrentSurfaceTexture::Timeout => Acquisition::Timeout,
            wgpu::CurrentSurfaceTexture::Occluded => Acquisition::Occluded,
            wgpu::CurrentSurfaceTexture::Lost => Acquisition::Lost,
            wgpu::CurrentSurfaceTexture::Outdated => Acquisition::Outdated,
            other => return Err(GpuError(format!("surface acquisition: {other:?}"))),
        })
    }
    fn recreate(&mut self) -> Result<(), GpuError> {
        self.surface = Some(
            self.instance
                .create_surface(self.window.as_ref().unwrap().clone())
                .map_err(|e| GpuError(e.to_string()))?,
        );
        #[cfg(target_os = "macos")]
        {
            self.transaction_present = false;
        }
        Ok(())
    }
    fn configure(&mut self) {
        self.surface
            .as_ref()
            .unwrap()
            .configure(&self.device, self.config.as_ref().unwrap());
        // Configuring restores the default count.
        #[cfg(target_os = "macos")]
        if self.wide_until.is_some() {
            self.set_drawable_count(3);
        }
    }
}

#[cfg(test)]
mod blur_damage_tests {
    use super::*;

    /// A filter samples only its own pixels: damage beside it stays local,
    /// damage inside it re-blurs the whole filter and nothing more.
    #[test]
    fn damage_outside_a_filter_is_local_and_inside_rebuilds_the_filter() {
        let viewport = Rect::new(0., 0., 500., 300.);
        let filter = (Rect::new(100., 100., 40., 30.), 2.);
        let distant = Rect::new(10., 10., 5., 5.);
        assert_eq!(
            blur_damage(&[distant], &[filter], viewport, 1.),
            vec![distant]
        );
        let beside = Rect::new(95., 110., 1., 1.);
        assert_eq!(
            blur_damage(&[beside], &[filter], viewport, 1.),
            vec![beside]
        );
        assert_eq!(
            blur_damage(&[Rect::new(101., 110., 1., 1.)], &[filter], viewport, 1.),
            vec![Rect::new(100., 100., 40., 30.)]
        );
        assert!(blur_damage(&[], &[filter], viewport, 1.).is_empty());
    }

    #[test]
    fn chained_filters_expand_to_fixed_point_independent_of_order() {
        let viewport = Rect::new(0., 0., 500., 300.);
        // Overlapping filters: rebuilding one damages the other.
        let filters = [
            (Rect::new(135., 100., 20., 20.), 2.),
            (Rect::new(120., 100., 20., 20.), 2.),
        ];
        let damage = [Rect::new(121., 105., 1., 1.)];
        let expected = vec![Rect::new(120., 100., 35., 20.)];
        assert_eq!(blur_damage(&damage, &filters, viewport, 1.), expected);
        assert_eq!(
            blur_damage(&damage, &[filters[1], filters[0]], viewport, 1.),
            expected
        );
    }

    #[test]
    fn reconstruction_covers_the_filter_pixels_across_fractional_scales() {
        for scale in [0.7, 1., 1.25, 1.3, 1.5, 1.7, 2.3] {
            let viewport = Rect::new(0., 0., 701. / scale, 503. / scale);
            for index in 0..127 {
                let output = Rect::new(
                    (index * 37 % 680) as f32 / scale + 0.017,
                    (index * 19 % 480) as f32 / scale + 0.031,
                    7.71,
                    8.13,
                );
                for radius in [0.0001, 0.33334, 1.17, 11.31, 1000.] {
                    let (x, y, w, h) = scissor(output, scale, 701, 503).unwrap();
                    let regions = blur_damage(&[output], &[(output, radius)], viewport, scale);
                    assert_eq!(regions.len(), 1);
                    let (dx, dy, dw, dh) = scissor(regions[0], scale, 701, 503).unwrap();
                    assert!(dx <= x && dy <= y);
                    assert!(dx + dw >= x + w && dy + dh >= y + h);
                }
            }
        }
    }

    #[test]
    fn fractional_output_rounds_to_whole_pixels_whatever_the_radius() {
        let viewport = Rect::new(0., 0., 400., 300.);
        // At 1.25x the output spans physical [12, 26).
        let regions = blur_damage(
            &[Rect::new(10.2, 10.2, 1., 1.)],
            &[(Rect::new(10.1, 10.1, 10.1, 10.1), 0.25)],
            viewport,
            1.25,
        );
        let (x, y, width, height) = scissor(regions[0], 1.25, 500, 375).unwrap();
        assert!(x <= 12 && y <= 12 && x + width >= 26 && y + height >= 26);
        assert!(width < 20 && height < 20);
        let huge = blur_damage(
            &[Rect::new(0., 0., 1., 1.)],
            &[(Rect::new(0., 0., 1., 1.), 10000.)],
            viewport,
            1.,
        );
        assert_eq!(huge, vec![Rect::new(0., 0., 1., 1.)]);
    }
}

pub mod canvas;

pub mod decoration;

pub mod svg;

#[cfg(test)]
mod native_surface_shader_tests {
    #[test]
    fn nv12_compute_shader_validates_on_the_available_gpu_backend() {
        let gpu = super::GpuRenderer::new(4, 4).unwrap();
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("NV12 portable validation"),
                source: wgpu::ShaderSource::Wgsl(include_str!("native_surface.wgsl").into()),
            });
        let _pipeline = gpu
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("NV12 validation"),
                layout: None,
                module: &shader,
                entry_point: Some("convert"),
                compilation_options: Default::default(),
                cache: None,
            });
    }
}
