//! macOS presentation through IOSurfaces of the renderer's own, shown by a
//! plain layer or, at the display's full rate, queued by an
//! `AVSampleBufferDisplayLayer`, instead of `CAMetalLayer` drawables.
//!
//! A drawable's contents are not kept between frames, so presenting one means
//! filling all of it: copying the whole retained frame every frame. A surface
//! of the renderer's own keeps what it held, so the retained frame can be the
//! surface itself: each frame repaints what changed since that surface was
//! last shown, and showing it copies nothing.
//!
//! Shown as a plain layer's contents, a surface replaces the one before; the
//! window server, compositing on a schedule of its own, then now and then
//! takes two in one refresh and shows nothing new in the next when frames
//! come at the display's full rate (measured: one frame in fifty, the
//! 99th-percentile frame interval about 21 ms where 13 is possible). A
//! sample buffer display layer queues what it is given, one image a refresh,
//! as `CAMetalLayer` does with its drawables, but without a drawable's
//! per-frame bookkeeping: each surface is wrapped once in a sample buffer
//! that is enqueued again whenever the surface holds the next frame. Slower
//! frames (an app's own 60 Hz updates, typing) are the plain layer's
//! contents: handed over at a set point before a refresh (the host's
//! `refresh_grid`), they show on it reliably, which the video layer's own thread
//! does not quite manage, and cost less (see `Layer`).
use crate::{FORMAT, GpuError};
use objc2::{Encoding, Message, RefEncode, msg_send, rc::Retained, runtime::AnyObject};
use objc2_core_foundation::{
    CFDictionary, CFNull, CFNumber, CFRetained, CFString, CFType, kCFNull,
};
use objc2_core_graphics::{CGColorSpace, kCGColorSpaceSRGB};
use objc2_io_surface::{
    IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceColorSpace, kIOSurfaceHeight,
    kIOSurfacePixelFormat, kIOSurfaceWidth,
};
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTextureDescriptor, MTLTextureType,
    MTLTextureUsage,
};
use objc2_quartz_core::{CAAutoresizingMask, CALayer, CAMetalLayer, CATransaction};
use std::{cell::Cell, ffi::c_void, ptr::null, rc::Rc};

/// A surface the renderer draws into and a layer shows.
pub(crate) struct Buffer {
    pub surface: CFRetained<IOSurfaceRef>,
    /// The surface as a sample buffer to enqueue (`Layer::show`).
    sample: Sample,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    /// The texture as Metal's, for frames encoded directly (`metal_frame`).
    pub metal: crate::metal_frame::Texture,
    /// Reading it to bring another surface up to date (copy_offset.wgsl).
    pub copy_source: std::cell::OnceCell<wgpu::BindGroup>,
}
impl Buffer {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Result<Self, GpuError> {
        // 'BGRA': four bytes a pixel, as `FORMAT` (Bgra8Unorm) stores them.
        let number = |value: i32| CFNumber::new_i32(value);
        let (w, h, bytes, format) = (
            number(width as i32),
            number(height as i32),
            number(4),
            number(i32::from_be_bytes(*b"BGRA")),
        );
        // SAFETY: The keys are IOSurface's own constant CFStrings.
        let keys: [&CFString; 4] = unsafe {
            [
                kIOSurfaceWidth,
                kIOSurfaceHeight,
                kIOSurfaceBytesPerElement,
                kIOSurfacePixelFormat,
            ]
        };
        let values: [&CFType; 4] = [&w, &h, &bytes, &format];
        let properties = CFDictionary::from_slices(&keys, &values);
        // SAFETY: The dictionary holds IOSurface creation properties.
        let surface = unsafe { IOSurfaceRef::new(properties.as_opaque()) }
            .ok_or_else(|| GpuError("IOSurface creation failed".into()))?;
        // sRGB, as a `CAMetalLayer` without a colour space shows its drawables:
        // untagged layer contents would be taken as the display's space.
        if let Some(list) = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
            .and_then(|space| CGColorSpace::property_list(&space))
        {
            // SAFETY: IOSurface's colour-space key and a colour space's
            // property list form, as it documents.
            unsafe { surface.set_value(kIOSurfaceColorSpace, &list) };
        }
        let sample = Sample::new(&surface)?;
        // SAFETY: The HAL guard only borrows this live device for the call.
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
            .ok_or_else(|| GpuError("not a Metal device".into()))?;
        // SAFETY: A plain 2D texture descriptor for a surface of this size.
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                width as usize,
                height as usize,
                false,
            )
        };
        descriptor.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
        descriptor.setStorageMode(MTLStorageMode::Shared);
        let metal = hal
            .raw_device()
            .newTextureWithDescriptor_iosurface_plane(&descriptor, &surface, 0)
            .ok_or_else(|| GpuError("Metal texture over an IOSurface failed".into()))?;
        drop(hal);
        // SAFETY: Metal made this texture on this device with this format and
        // size; the surface backing it lives as long as the buffer.
        let raw = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                metal.clone(),
                FORMAT,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width,
                    height,
                    depth: 1,
                },
            )
        };
        let texture = unsafe {
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                raw,
                &wgpu::TextureDescriptor {
                    label: Some("presented surface"),
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
                },
            )
        };
        let view = texture.create_view(&Default::default());
        Ok(Self {
            surface,
            sample,
            texture,
            view,
            metal,
            copy_source: Default::default(),
        })
    }
}

/// A surface as a sample buffer a display layer can enqueue, made once: a
/// pixel buffer over the surface, tagged sRGB, marked to show at once.
struct Sample {
    pixels: *const c_void,
    sample: *const c_void,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}
#[repr(C)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation: CMTime,
    decode: CMTime,
}
#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    static kCMTimeInvalid: CMTime;
    static kCMSampleAttachmentKey_DisplayImmediately: *const c_void;
    fn CMVideoFormatDescriptionCreateForImageBuffer(
        allocator: *const c_void,
        image: *const c_void,
        format: *mut *const c_void,
    ) -> i32;
    fn CMSampleBufferCreateReadyWithImageBuffer(
        allocator: *const c_void,
        image: *const c_void,
        format: *const c_void,
        timing: *const CMSampleTimingInfo,
        sample: *mut *const c_void,
    ) -> i32;
    fn CMSampleBufferGetSampleAttachmentsArray(sample: *const c_void, create: u8) -> *const c_void;
}
#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVImageBufferCGColorSpaceKey: *const c_void;
    static kCVImageBufferColorPrimariesKey: *const c_void;
    static kCVImageBufferColorPrimaries_ITU_R_709_2: *const c_void;
    static kCVImageBufferTransferFunctionKey: *const c_void;
    static kCVImageBufferTransferFunction_sRGB: *const c_void;
    fn CVPixelBufferCreateWithIOSurface(
        allocator: *const c_void,
        surface: *const c_void,
        attributes: *const c_void,
        pixels: *mut *const c_void,
    ) -> i32;
    fn CVBufferSetAttachment(buffer: *const c_void, key: *const c_void, value: *const c_void, mode: u32);
    fn CVPixelBufferGetIOSurface(pixels: *const c_void) -> *const c_void;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: *const c_void;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFDictionarySetValue(dictionary: *const c_void, key: *const c_void, value: *const c_void);
    fn CFRelease(object: *const c_void);
}
#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVLayerVideoGravityResize: *const c_void;
}
impl Sample {
    fn new(surface: &IOSurfaceRef) -> Result<Self, GpuError> {
        let failed = |what: &str| GpuError(format!("{what} failed for a presented surface"));
        // SAFETY: CoreVideo and CoreMedia calls on objects made here, each
        // checked; the created ones are released with the sample (or below).
        unsafe {
            let mut pixels = null();
            if CVPixelBufferCreateWithIOSurface(
                null(),
                (surface as *const IOSurfaceRef).cast(),
                null(),
                &mut pixels,
            ) != 0
                || pixels.is_null()
            {
                return Err(failed("CVPixelBufferCreateWithIOSurface"));
            }
            let mut this = Self { pixels, sample: null() };
            // sRGB, as the surface itself is tagged (kCVAttachmentMode_ShouldPropagate).
            if let Some(space) = CGColorSpace::with_name(Some(kCGColorSpaceSRGB)) {
                let space = CFRetained::as_ptr(&space).as_ptr().cast::<c_void>();
                CVBufferSetAttachment(pixels, kCVImageBufferCGColorSpaceKey, space, 1);
            }
            CVBufferSetAttachment(
                pixels,
                kCVImageBufferColorPrimariesKey,
                kCVImageBufferColorPrimaries_ITU_R_709_2,
                1,
            );
            CVBufferSetAttachment(
                pixels,
                kCVImageBufferTransferFunctionKey,
                kCVImageBufferTransferFunction_sRGB,
                1,
            );
            let mut format = null();
            if CMVideoFormatDescriptionCreateForImageBuffer(null(), pixels, &mut format) != 0
                || format.is_null()
            {
                return Err(failed("CMVideoFormatDescriptionCreateForImageBuffer"));
            }
            // Shown on arrival, so its time does not matter.
            let zero = CMTime {
                value: 0,
                timescale: 1,
                flags: 1,
                epoch: 0,
            };
            let timing = CMSampleTimingInfo {
                duration: kCMTimeInvalid,
                presentation: zero,
                decode: kCMTimeInvalid,
            };
            let status = CMSampleBufferCreateReadyWithImageBuffer(
                null(),
                pixels,
                format,
                &timing,
                &mut this.sample,
            );
            CFRelease(format);
            if status != 0 || this.sample.is_null() {
                return Err(failed("CMSampleBufferCreateReadyWithImageBuffer"));
            }
            let attachments = CMSampleBufferGetSampleAttachmentsArray(this.sample, 1);
            if attachments.is_null() {
                return Err(failed("CMSampleBufferGetSampleAttachmentsArray"));
            }
            CFDictionarySetValue(
                CFArrayGetValueAtIndex(attachments, 0),
                kCMSampleAttachmentKey_DisplayImmediately,
                kCFBooleanTrue,
            );
            Ok(this)
        }
    }
}
impl Drop for Sample {
    fn drop(&mut self) {
        // SAFETY: Each was created (+1) in `new` and is released once.
        unsafe {
            if !self.sample.is_null() {
                CFRelease(self.sample);
            }
            CFRelease(self.pixels);
        }
    }
}

/// Layers showing the presented surface, filling the window's `CAMetalLayer`
/// (which follows the view's size and, given no drawables, shows nothing).
///
/// While frames come at the display's full rate, a sample buffer display
/// layer queues them (see the module's documentation). Once it exists its
/// CoreMedia thread wakes about 55 times a second for good, even with
/// nothing to show, so slower frames, and a window at rest, are a plain
/// layer's contents instead, and the video layer is dropped at rest.
pub(crate) struct Layer {
    host: Retained<CAMetalLayer>,
    plain: Retained<CALayer>,
    /// Whether `plain` has contents (shows a surface, or holds one under
    /// `video` until the video layer shows a frame of its own).
    plain_shows: Rc<Cell<bool>>,
    /// Counts changes of which layer shows frames, so a pending `Uncover`
    /// of an earlier one does nothing.
    generation: Rc<Cell<u64>>,
    /// An `Uncover` is pending.
    uncovering: Rc<Cell<bool>>,
    video: Option<Video>,
    /// What the layers were last given, so a frame sets only what changed.
    opaque: Cell<bool>,
}
/// Clears the plain layer's contents once the video layer above it shows a
/// frame: until then they show the frame before; after, under a window's
/// translucent pixels, they would show through. Checked on the main queue
/// each refresh or so, as no further frame may come to check it.
struct Uncover {
    plain: Retained<CALayer>,
    queue: Retained<AnyObject>,
    reports_displayed: bool,
    shows: Rc<Cell<bool>>,
    generation: Rc<Cell<u64>>,
    expected: u64,
    pending: Rc<Cell<bool>>,
    tries: u32,
}
impl Uncover {
    fn schedule(self: Box<Self>) {
        // SAFETY: The main queue runs `run` once with the box, which it
        // takes back.
        unsafe {
            dispatch_after_f(
                dispatch_time(0, 8_000_000),
                &raw const _dispatch_main_q,
                Box::into_raw(self).cast(),
                Self::run,
            )
        };
    }
    extern "C" fn run(context: *mut c_void) {
        // SAFETY: `schedule` passed a boxed `Uncover`, for this call alone.
        let mut this = unsafe { Box::from_raw(context.cast::<Uncover>()) };
        if this.generation.get() != this.expected || !this.shows.get() {
            this.pending.set(false);
            return;
        }
        this.tries += 1;
        // Without word from the video layer, about six refreshes.
        let shown = if this.reports_displayed {
            displayed(&this.queue).is_some()
        } else {
            this.tries > 6
        };
        if shown || this.tries > 60 {
            // SAFETY: No contents is valid.
            transaction(|| unsafe { this.plain.setContents(None) });
            this.shows.set(false);
            this.pending.set(false);
            return;
        }
        this.schedule();
    }
}
#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_time(when: u64, delta: i64) -> u64;
    fn dispatch_after_f(
        when: u64,
        queue: *const c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}
/// `CMSampleBufferRef` and `CVPixelBufferRef` pointees, encoded as the
/// Objective-C runtime declares them (objc2 checks messages in debug builds).
#[repr(C)]
struct CMSampleBuffer([u8; 0]);
// SAFETY: `opaqueCMSampleBuffer` is CoreMedia's struct tag for the type.
unsafe impl RefEncode for CMSampleBuffer {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("opaqueCMSampleBuffer", &[]));
}
#[repr(C)]
struct CVBuffer([u8; 0]);
// SAFETY: `__CVBuffer` is CoreVideo's struct tag for the type.
unsafe impl RefEncode for CVBuffer {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("__CVBuffer", &[]));
}
/// The surface a video renderer shows (its IOSurface ID), if any.
fn displayed(queue: &AnyObject) -> Option<u32> {
    // SAFETY: Returns a retained CVPixelBufferRef or null, released here; the
    // surface it names is borrowed only for its ID.
    unsafe {
        let pixels: *mut CVBuffer = msg_send![queue, copyDisplayedPixelBuffer];
        let pixels = pixels.cast::<c_void>();
        if pixels.is_null() {
            return None;
        }
        let surface = CVPixelBufferGetIOSurface(pixels);
        let id = (!surface.is_null()).then(|| (*surface.cast::<IOSurfaceRef>()).id());
        CFRelease(pixels);
        id
    }
}
struct Video {
    layer: Retained<CALayer>,
    /// What surfaces are enqueued to: the layer's video renderer (macOS 14),
    /// or the layer itself.
    queue: Retained<AnyObject>,
    /// Whether `queue` can say what it shows (`displayed`).
    reports_displayed: bool,
}
/// Fill `host` and follow its size, without implicit animation.
fn fill(layer: &CALayer, host: &CAMetalLayer, opaque: bool) {
    layer.setFrame(host.bounds());
    layer.setContentsScale(host.contentsScale());
    layer.setOpaque(opaque);
    layer.setAutoresizingMask(
        CAAutoresizingMask::LayerWidthSizable | CAAutoresizingMask::LayerHeightSizable,
    );
    let keys = ["contents", "contentsScale", "opaque", "bounds", "position", "hidden"]
        .map(CFString::from_static_str);
    // SAFETY: CoreFoundation's null singleton.
    let null_value: &CFNull = unsafe { kCFNull }.expect("kCFNull");
    let actions = CFDictionary::<CFString, CFNull>::from_slices(
        &keys.each_ref().map(|key| &**key),
        &[null_value; 6],
    );
    // SAFETY: A CFDictionary is toll-free bridged to NSDictionary, and
    // CFNull to NSNull: an action that does nothing.
    unsafe {
        let actions = &*(&*actions as *const CFDictionary<CFString, CFNull>).cast::<AnyObject>();
        let _: () = msg_send![layer, setActions: actions];
    }
}
/// Run `change` as one Core Animation transaction, without animation.
fn transaction(change: impl FnOnce()) {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    change();
    CATransaction::commit();
}
impl Video {
    fn new(host: &CAMetalLayer, opaque: bool) -> Self {
        // SAFETY: AVFoundation (linked above) defines the class; `new` makes
        // a layer, a CALayer subclass.
        let layer: Retained<CALayer> = unsafe {
            let layer: Retained<AnyObject> = msg_send![objc2::class!(AVSampleBufferDisplayLayer), new];
            Retained::cast_unchecked(layer)
        };
        // SAFETY: The layer's own properties, with an AVLayerVideoGravity
        // constant: stretch each surface (the window's size in pixels) to the
        // layer's bounds, so one pixel is one pixel.
        unsafe {
            let gravity = &*AVLayerVideoGravityResize.cast::<AnyObject>();
            let _: () = msg_send![&*layer, setVideoGravity: gravity];
            // A window animating is not a video playing: the display sleeps
            // as it otherwise would.
            let _: () = msg_send![&*layer, setPreventsDisplaySleepDuringVideoPlayback: false];
        }
        fill(&layer, host, opaque);
        // SAFETY: `sampleBufferRenderer` (macOS 14) returns the layer's
        // renderer; earlier systems enqueue to the layer itself.
        let queue = unsafe {
            let has_renderer: bool =
                msg_send![&*layer, respondsToSelector: objc2::sel!(sampleBufferRenderer)];
            let renderer: Option<Retained<AnyObject>> = if has_renderer {
                msg_send![&*layer, sampleBufferRenderer]
            } else {
                None
            };
            renderer.unwrap_or_else(|| Retained::cast_unchecked(layer.clone()))
        };
        // SAFETY: A selector check on a live object.
        let reports_displayed: bool = unsafe {
            msg_send![&*queue, respondsToSelector: objc2::sel!(copyDisplayedPixelBuffer)]
        };
        // Above the plain layer, which shows the frame before until this
        // layer shows one.
        transaction(|| host.addSublayer(&layer));
        Self {
            layer,
            queue,
            reports_displayed,
        }
    }
    fn displayed(&self) -> Option<u32> {
        self.reports_displayed
            .then(|| displayed(&self.queue))
            .flatten()
    }
    fn enqueue(&self, buffer: &Buffer) {
        // SAFETY: The renderer's (or layer's) own methods; the sample buffer
        // is a live CMSampleBufferRef the buffer owns.
        unsafe {
            // A renderer that failed, or was interrupted, shows nothing new
            // until flushed (AVQueuedSampleBufferRenderingStatusFailed is 2).
            let status: isize = msg_send![&*self.queue, status];
            let interrupted: bool = msg_send![&*self.queue, requiresFlushToResumeDecoding];
            if status == 2 || interrupted {
                let _: () = msg_send![&*self.queue, flush];
            }
            let sample = buffer.sample.sample.cast_mut().cast::<CMSampleBuffer>();
            let _: () = msg_send![&*self.queue, enqueueSampleBuffer: sample];
        }
    }
}
impl Layer {
    pub fn attach(host: &CAMetalLayer) -> Self {
        let plain = CALayer::new();
        fill(&plain, host, false);
        transaction(|| host.addSublayer(&plain));
        Self {
            host: host.retain(),
            plain,
            plain_shows: Default::default(),
            generation: Default::default(),
            uncovering: Default::default(),
            video: None,
            opaque: Cell::new(false),
        }
    }
    /// The surface on screen (its IOSurface ID), if the video layer shows it
    /// and says so (macOS 14.4). The window server keeps every surface a
    /// video layer was given marked in use, so `IOSurfaceIsInUse` cannot
    /// tell; a plain layer's contents release theirs once replaced.
    pub fn displayed(&self) -> Option<u32> {
        self.video.as_ref().and_then(Video::displayed)
    }
    /// Show `buffer`'s surface: queued by the video layer when `steady`
    /// (frames come at the display's full rate), else as the plain layer's
    /// contents. `opaque`:
    /// every pixel is, so the window server need not blend the layers over
    /// what lies behind them.
    pub fn show(&mut self, buffer: &Buffer, opaque: bool, steady: bool) {
        if self.opaque.replace(opaque) != opaque {
            transaction(|| {
                self.plain.setOpaque(opaque);
                if let Some(video) = &self.video {
                    video.layer.setOpaque(opaque);
                }
            });
        }
        if steady && self.video.is_none() {
            self.video = Some(Video::new(&self.host, opaque));
        }
        let Some(video) = &self.video else {
            // SAFETY: An IOSurface is a layer's valid contents.
            let surface = &*buffer.surface as *const IOSurfaceRef as *const AnyObject;
            transaction(|| unsafe { self.plain.setContents(Some(&*surface)) });
            self.plain_shows.set(true);
            return;
        };
        video.enqueue(buffer);
        if !self.plain_shows.get() || self.uncovering.replace(true) {
            return;
        }
        Box::new(Uncover {
            plain: self.plain.clone(),
            queue: video.queue.clone(),
            reports_displayed: video.reports_displayed,
            shows: self.plain_shows.clone(),
            generation: self.generation.clone(),
            expected: self.generation.get(),
            pending: self.uncovering.clone(),
            tries: 0,
        })
        .schedule();
    }
    /// The window is at rest showing `shown`: show it on the plain layer
    /// and drop the video layer.
    pub fn rest(&mut self, shown: &Buffer) {
        let Some(video) = self.video.take() else {
            return;
        };
        self.generation.set(self.generation.get() + 1);
        // SAFETY: An IOSurface is a layer's valid contents.
        let surface = &*shown.surface as *const IOSurfaceRef as *const AnyObject;
        transaction(|| {
            unsafe { self.plain.setContents(Some(&*surface)) };
            video.layer.removeFromSuperlayer();
        });
        self.plain_shows.set(true);
    }
}
impl Drop for Layer {
    fn drop(&mut self) {
        self.generation.set(self.generation.get() + 1);
        transaction(|| {
            if let Some(video) = &self.video {
                video.layer.removeFromSuperlayer();
            }
            self.plain.removeFromSuperlayer();
        });
    }
}
