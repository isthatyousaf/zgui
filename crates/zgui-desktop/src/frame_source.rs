//! Native display refresh sources for [`zgui::frame::FrameClock`].
//!
//! macOS uses a `CVDisplayLink` per display (see `display_link`), which
//! windows follow as they move between displays. Wayland paces through compositor frame
//! callbacks (driven from the host's redraw requests). Everything else, and
//! any platform whose native source is unavailable, uses [`FrameTimer`]:
//! event-loop deadlines on a fixed grid at the display's reported refresh rate.
use std::time::{Duration, Instant};
use zgui::frame::Frame;

pub(crate) const DEFAULT_INTERVAL: Duration = Duration::from_nanos(16_666_667);
/// A native source that stops ticking while frames are wanted is backed up by
/// the timer after this long, so animations degrade instead of freezing.
#[cfg(target_os = "macos")]
pub(crate) const WATCHDOG: Duration = Duration::from_millis(250);

pub(crate) fn interval_for(refresh_hz: Option<f64>) -> Duration {
    refresh_hz
        .filter(|hz| hz.is_finite() && *hz >= 1.)
        .map_or(DEFAULT_INTERVAL, |hz| Duration::from_secs_f64(1. / hz))
}

/// Refresh-aligned deadlines without a native vsync signal. Ticks land on a
/// fixed grid from `epoch`, so a late wake-up does not drift later ticks.
pub(crate) struct FrameTimer {
    epoch: Instant,
    /// The index of the refresh starting at `epoch`.
    first: u64,
}
impl FrameTimer {
    pub(crate) fn new(epoch: Instant) -> Self {
        Self { epoch, first: 0 }
    }
    /// Ticks continuing a native source's: refresh `first` starts at `epoch`.
    #[cfg(target_os = "macos")]
    pub(crate) fn aligned(epoch: Instant, first: u64) -> Self {
        Self { epoch, first }
    }
    fn index(&self, at: Instant, interval: Duration) -> u64 {
        (at.saturating_duration_since(self.epoch).as_secs_f64() / interval.as_secs_f64()) as u64
    }
    /// The first refresh boundary strictly after `now`.
    pub(crate) fn next_deadline(&self, now: Instant, interval: Duration) -> Instant {
        if now < self.epoch {
            return self.epoch;
        }
        let next = self.index(now, interval) + 1;
        self.epoch + interval.mul_f64(next as f64)
    }
    /// The frame for the boundary at or before `now`. It is expected on screen
    /// one refresh later, once rendered.
    pub(crate) fn frame(&self, now: Instant, interval: Duration) -> Frame {
        let index = self.index(now, interval);
        Frame {
            index: self.first + index,
            time: self.epoch + interval.mul_f64((index + 1) as f64),
            interval,
        }
    }
}

/// How long a refresh grid learned from the display link paces frames
/// before the link runs again to resync it (see `Host::sync_frames`).
#[cfg(target_os = "macos")]
pub(crate) const VSYNC_RESYNC: Duration = Duration::from_secs(1);

#[cfg(target_os = "macos")]
pub(crate) use display_link::DisplayLink;

#[cfg(target_os = "macos")]
mod display_link {
    //! `CVDisplayLink` pacing, shared per display.
    //!
    //! A view's `CADisplayLink` calls back as late in the refresh as it thinks
    //! the app can afford: about 2 ms before the deadline for a fast renderer.
    //! Any hiccup then misses the compositor's latch, the frame shows a refresh
    //! late, a second present queues behind it, and from then on every
    //! drawable acquisition waits on the compositor (a frame lost every few
    //! refreshes). `CVDisplayLink` fires on its own thread at the start of each
    //! refresh, leaving the whole interval to render and present.
    //!
    //! `CVDisplayLinkStop` returns before the link's thread has necessarily
    //! finished its last callback, and releasing a link or anything its
    //! callback reads races that callback. So, as in GPUI: one link per
    //! display, created on first use and never released; its callback context
    //! is the display id, not a pointer, and it reads only the static registry.
    //! Windows subscribe with a thread-safe delivery function. Links start and
    //! stop only on the main thread and never while the registry lock is held,
    //! since the callback takes that lock under CoreVideo's own locks.
    use super::*;
    use objc2::{msg_send, rc::Retained, runtime::AnyObject};
    use std::{
        collections::HashMap,
        ffi::c_void,
        sync::{
            Arc, Mutex, MutexGuard, PoisonError,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
    };
    use winit::{
        raw_window_handle::{HasWindowHandle, RawWindowHandle},
        window::Window,
    };

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SmpteTime {
        subframes: i16,
        subframe_divisor: i16,
        counter: u32,
        kind: u32,
        flags: u32,
        hours: i16,
        minutes: i16,
        seconds: i16,
        frames: i16,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CvTimeStamp {
        version: u32,
        video_time_scale: i32,
        video_time: i64,
        host_time: u64,
        rate_scalar: f64,
        video_refresh_period: i64,
        smpte_time: SmpteTime,
        flags: u64,
        reserved: u64,
    }
    type CvDisplayLink = c_void;
    type OutputCallback = extern "C" fn(
        *mut CvDisplayLink,
        *const CvTimeStamp,
        *const CvTimeStamp,
        u64,
        *mut u64,
        *mut c_void,
    ) -> i32;
    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        fn CVDisplayLinkCreateWithCGDisplay(display: u32, link: *mut *mut CvDisplayLink) -> i32;
        fn CVDisplayLinkSetOutputCallback(
            link: *mut CvDisplayLink,
            callback: OutputCallback,
            context: *mut c_void,
        ) -> i32;
        fn CVDisplayLinkStart(link: *mut CvDisplayLink) -> i32;
        fn CVDisplayLinkStop(link: *mut CvDisplayLink) -> i32;
    }
    #[repr(C)]
    #[derive(Default)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    fn nanos(ticks: i128) -> i128 {
        static TIMEBASE: std::sync::OnceLock<(i128, i128)> = std::sync::OnceLock::new();
        let (numer, denom) = *TIMEBASE.get_or_init(|| {
            let mut info = Timebase::default();
            // SAFETY: writes two integers into a local.
            unsafe { mach_timebase_info(&mut info) };
            (info.numer as i128, info.denom.max(1) as i128)
        });
        ticks * numer / denom
    }

    /// One window's subscription, reachable from the link thread.
    struct Subscriber {
        id: u64,
        /// Delivering: the window wants refreshes.
        active: AtomicBool,
        /// A delivered refresh has not been consumed yet; later ones coalesce.
        pending: AtomicBool,
        /// Deliver at most this often (millihertz; 0 for every refresh).
        max_rate: AtomicU64,
        deliver: Box<dyn Fn(Frame) + Send + Sync>,
    }
    struct Entry {
        /// Immortal: see the module comment.
        link: usize,
        running: bool,
        subscribers: Vec<Arc<Subscriber>>,
        /// The refresh the link last reported starting (host time), and the
        /// display's refresh period (ns): its refresh grid.
        refreshed: AtomicU64,
        period: AtomicU64,
    }
    #[derive(Default)]
    struct Registry {
        displays: HashMap<u32, Entry>,
        next_id: u64,
    }
    static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);
    fn registry() -> MutexGuard<'static, Option<Registry>> {
        // The map stays consistent across a panic, and the extern callback
        // must not unwind.
        REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
    }

    extern "C" fn output(
        _link: *mut CvDisplayLink,
        _now: *const CvTimeStamp,
        output: *const CvTimeStamp,
        _flags_in: u64,
        _flags_out: *mut u64,
        context: *mut c_void,
    ) -> i32 {
        // SAFETY: CoreVideo passes a valid timestamp for this callback.
        let output = unsafe { *output };
        let display = context as usize as u32;
        let period = output.video_refresh_period.max(1) as f64;
        let scale = output.video_time_scale.max(1) as f64;
        let interval = Duration::from_secs_f64(period / scale);
        let index = (output.video_time as f64 / period).round().max(0.) as u64;
        // SAFETY: reads the monotonic host clock.
        let host_now = unsafe { mach_absolute_time() };
        let ahead = nanos(output.host_time as i128 - host_now as i128).clamp(0, 1_000_000_000);
        let frame = Frame {
            index,
            time: Instant::now() + Duration::from_nanos(ahead as u64),
            interval,
        };
        let refresh = scale / period;
        let guard = registry();
        let Some(entry) = guard.as_ref().and_then(|r| r.displays.get(&display)) else {
            return 0;
        };
        // The output time: exactly on the display's refresh grid (the
        // callback's `now`, when it runs, wanders by up to 2 ms).
        entry.refreshed.store(output.host_time, Ordering::Release);
        entry.period.store(interval.as_nanos() as u64, Ordering::Release);
        for subscriber in &entry.subscribers {
            if !subscriber.active.load(Ordering::Acquire) {
                continue;
            }
            let max_rate = subscriber.max_rate.load(Ordering::Relaxed);
            if max_rate > 0 {
                let divisor = ((refresh * 1000. / max_rate as f64) - 1e-6).ceil().max(1.) as u64;
                if !index.is_multiple_of(divisor) {
                    continue;
                }
            }
            if !subscriber.pending.swap(true, Ordering::AcqRel) {
                (subscriber.deliver)(frame);
            }
        }
        0
    }

    /// Start or stop `display`'s link to match its active subscribers. Main
    /// thread only; CoreVideo is called outside the lock.
    fn reconcile(display: u32) {
        let change = {
            let mut guard = registry();
            let Some(entry) = guard.as_mut().and_then(|r| r.displays.get_mut(&display)) else {
                return;
            };
            let wanted = entry
                .subscribers
                .iter()
                .any(|s| s.active.load(Ordering::Acquire));
            (wanted != entry.running).then(|| {
                entry.running = wanted;
                (entry.link, wanted)
            })
        };
        if let Some((link, start)) = change {
            let link = link as *mut CvDisplayLink;
            // SAFETY: the link is immortal and valid.
            unsafe {
                if start {
                    CVDisplayLinkStart(link);
                } else {
                    CVDisplayLinkStop(link);
                }
            }
        }
    }

    /// Ensure `display` has a link, creating it (outside the lock) if needed.
    fn ensure_link(display: u32) -> bool {
        if registry()
            .as_ref()
            .is_some_and(|r| r.displays.contains_key(&display))
        {
            return true;
        }
        let mut link: *mut CvDisplayLink = std::ptr::null_mut();
        // SAFETY: CoreVideo writes a new link on success.
        if unsafe { CVDisplayLinkCreateWithCGDisplay(display, &mut link) } != 0 || link.is_null() {
            return false;
        }
        // SAFETY: the context is the display id, never dereferenced.
        unsafe { CVDisplayLinkSetOutputCallback(link, output, display as usize as *mut c_void) };
        let mut guard = registry();
        let registry = guard.get_or_insert_with(Registry::default);
        // Another subscriber may have raced us; the spare link is leaked
        // deliberately (never started, so it never calls back).
        registry.displays.entry(display).or_insert(Entry {
            link: link as usize,
            running: false,
            subscribers: Vec::new(),
            refreshed: AtomicU64::new(0),
            period: AtomicU64::new(0),
        });
        true
    }

    /// A window's subscription to its display's refreshes.
    pub(crate) struct DisplayLink {
        view: Retained<AnyObject>,
        display: u32,
        subscriber: Arc<Subscriber>,
        /// When the window's display was last checked (an AppKit round trip).
        checked: Instant,
    }
    impl DisplayLink {
        /// `deliver` runs on the link's thread, at most once per consumed
        /// refresh; hosts forward it to the UI thread.
        pub(crate) fn new(
            window: &Window,
            deliver: impl Fn(Frame) + Send + Sync + 'static,
        ) -> Option<Self> {
            let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
                return None;
            };
            // SAFETY: winit keeps this NSView alive while the window exists;
            // retaining it keeps it valid for the link's lifetime.
            let view: Retained<AnyObject> =
                unsafe { Retained::retain(handle.ns_view.as_ptr().cast())? };
            let display = display_of(&view)?;
            if !ensure_link(display) {
                return None;
            }
            let subscriber = {
                let mut guard = registry();
                let registry = guard.as_mut()?;
                registry.next_id += 1;
                let subscriber = Arc::new(Subscriber {
                    id: registry.next_id,
                    active: AtomicBool::new(false),
                    pending: AtomicBool::new(false),
                    max_rate: AtomicU64::new(0),
                    deliver: Box::new(deliver),
                });
                registry
                    .displays
                    .get_mut(&display)?
                    .subscribers
                    .push(subscriber.clone());
                subscriber
            };
            Some(Self {
                view,
                display,
                subscriber,
                checked: Instant::now(),
            })
        }
        pub(crate) fn running(&self) -> bool {
            self.subscriber.active.load(Ordering::Acquire)
        }
        pub(crate) fn set_running(&mut self, running: bool) {
            // Recheck the display when starting, and about once a second while
            // running, in case the window moved to another screen.
            if running && (!self.running() || self.checked.elapsed() > Duration::from_secs(1)) {
                self.checked = Instant::now();
                self.follow_display();
            }
            if self.subscriber.active.swap(running, Ordering::AcqRel) != running {
                self.subscriber.pending.store(false, Ordering::Release);
                reconcile(self.display);
            }
        }
        /// How far into a refresh of the window's display `at` falls, and the
        /// refresh period, on the display's refresh grid as the link last
        /// reported it (within `fresh`): refreshes start when the previous
        /// one shows.
        pub(crate) fn refresh_phase(&self, at: Instant, fresh: Duration) -> Option<(Duration, Duration)> {
            let (refreshed, period) = {
                let guard = registry();
                let entry = guard.as_ref()?.displays.get(&self.display)?;
                (entry.refreshed.load(Ordering::Acquire), entry.period.load(Ordering::Acquire))
            };
            if refreshed == 0 || period == 0 {
                return None;
            }
            // SAFETY: reads the monotonic host clock.
            let host_now = unsafe { mach_absolute_time() };
            // The output time is a refresh or two ahead; the grid continues
            // back from it.
            let since = nanos(host_now as i128 - refreshed as i128)
                + at.saturating_duration_since(Instant::now()).as_nanos() as i128;
            if since > fresh.as_nanos() as i128 {
                return None;
            }
            let phase = since.rem_euclid(period as i128) as u64;
            Some((Duration::from_nanos(phase), Duration::from_nanos(period)))
        }
        /// The UI thread took the last delivered refresh; deliver the next.
        pub(crate) fn consumed(&self) {
            self.subscriber.pending.store(false, Ordering::Release);
        }
        /// Deliver at most `hz` refreshes (whole-refresh spacing); `None`
        /// for every refresh.
        pub(crate) fn set_preferred_rate(&mut self, hz: Option<f64>) {
            let rate = hz
                .filter(|hz| hz.is_finite() && *hz > 0.)
                .map_or(0, |hz| (hz * 1000.) as u64);
            self.subscriber.max_rate.store(rate, Ordering::Relaxed);
        }
        /// Move the subscription when the window moves to another display.
        fn follow_display(&mut self) {
            let Some(display) = display_of(&self.view) else {
                return;
            };
            if display == self.display || !ensure_link(display) {
                return;
            }
            let old = self.display;
            {
                let mut guard = registry();
                let Some(registry) = guard.as_mut() else {
                    return;
                };
                if let Some(entry) = registry.displays.get_mut(&old) {
                    entry.subscribers.retain(|s| s.id != self.subscriber.id);
                }
                if let Some(entry) = registry.displays.get_mut(&display) {
                    entry.subscribers.push(self.subscriber.clone());
                }
            }
            self.display = display;
            reconcile(old);
            reconcile(display);
        }
    }
    impl Drop for DisplayLink {
        fn drop(&mut self) {
            self.subscriber.active.store(false, Ordering::Release);
            if let Some(registry) = registry().as_mut()
                && let Some(entry) = registry.displays.get_mut(&self.display)
            {
                entry.subscribers.retain(|s| s.id != self.subscriber.id);
            }
            reconcile(self.display);
        }
    }

    /// The `CGDirectDisplayID` of the screen showing `view`'s window.
    fn display_of(view: &AnyObject) -> Option<u32> {
        // SAFETY: NSView -window, NSWindow -screen, NSScreen
        // -deviceDescription and NSNumber -unsignedIntValue on the main thread.
        unsafe {
            let window: Option<Retained<AnyObject>> = msg_send![view, window];
            let screen: Option<Retained<AnyObject>> = msg_send![&*window?, screen];
            let description: Option<Retained<AnyObject>> = msg_send![&*screen?, deviceDescription];
            let key = objc2_foundation::NSString::from_str("NSScreenNumber");
            let number: Option<Retained<AnyObject>> =
                msg_send![&*description?, objectForKey: &*key];
            let id: u32 = msg_send![&*number?, unsignedIntValue];
            Some(id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_ticks_on_a_fixed_grid_after_late_wakeups() {
        let epoch = Instant::now();
        let timer = FrameTimer::new(epoch);
        let interval = Duration::from_millis(10);
        assert_eq!(timer.next_deadline(epoch, interval), epoch + interval);
        // Waking 3 ms late still schedules the following boundary on the grid.
        let late = epoch + Duration::from_millis(13);
        assert_eq!(timer.next_deadline(late, interval), epoch + interval * 2);
        let frame = timer.frame(late, interval);
        assert_eq!(frame.index, 1);
        assert_eq!(frame.time, epoch + interval * 2);
        assert_eq!(frame.interval, interval);
    }

    #[test]
    fn missing_or_bogus_refresh_rates_fall_back_to_60_hz() {
        assert_eq!(interval_for(None), DEFAULT_INTERVAL);
        assert_eq!(interval_for(Some(0.)), DEFAULT_INTERVAL);
        assert_eq!(interval_for(Some(f64::NAN)), DEFAULT_INTERVAL);
        assert_eq!(interval_for(Some(120.)), Duration::from_secs_f64(1. / 120.));
    }
}
