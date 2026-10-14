//! Glass resource monitor: launches the kitchen sink, or another example with
//! `--app <example>` (or attaches with `--pid <pid>`), and charts CPU, GPU and
//! memory over the last minute for any process on the machine. GPU time per
//! process comes from the GPU driver's per-client accounting (macOS: each
//! `AGXDeviceUserClient`'s `AppUsage`), the way Activity Monitor reads it.
//!
//! The sidebar lists every readable process. ↑/↓ browse it in the active
//! panel, Enter (or a row's +) splits a new panel beside it so the previous one
//! stays put for comparison, ←/→ switch panels and ⌘W closes one. Drag a
//! process or a panel header onto a panel: its left or right edge inserts
//! beside it, the centre replaces (or swaps) it.
//! Run: cargo run --release -p zgui-desktop --example zgui_monitor
//! (build the target first: cargo build --release -p zgui-desktop --example kitchen_sink)
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    process::{Child, Command},
    rc::Rc,
    time::{Duration, Instant},
};
use zgui::{
    compose::prelude::*,
    cursor::Cursor,
    input::{DragPhase, EventPhase, InputEvent, Key, PointerButton},
    reactive::{Runtime, Signal},
    scene::{Color, Insets, Rect, Scene},
    text_layout::FontFeatures,
    timer::sleep,
};
use zgui_desktop::{Application, WindowHandle, WindowOptions};

#[cfg(target_os = "macos")]
#[path = "support/glass.rs"]
mod glass;

const TITLE: &str = "zgui monitor";
const WIDTH: f64 = 1280.;
const HEIGHT: f64 = 780.;
const INTERVAL: Duration = Duration::from_millis(500);
/// One minute of samples.
const CAPACITY: usize = 120;
const CHART_HEIGHT: f32 = 130.;
/// Room for the marker at the plot's edges; the y labels' half line height,
/// so a justified label column lines up with the grid.
const INSET: f32 = 6.;
/// Beyond this, dropping on a panel's edge replaces it instead of splitting.
const MAX_PANELS: usize = 4;

// Sidebar geometry; the chrome heights let key navigation know the list's
// viewport without measuring it.
const SIDEBAR_WIDTH: f32 = 272.;
const TITLEBAR: f32 = 38.;
const SEARCH: f32 = 48.;
const FOOTER: f32 = 40.;
const ROW: f32 = 44.;

// Dark-surface steps of the reference categorical palette: one hue per chart.
const CPU_HUE: u32 = 0x3987e5;
const MEMORY_HUE: u32 = 0x199e70;
const GPU_HUE: u32 = 0xe5793a;
const ACCENT: u32 = 0x8b7cf6;
const MUTED: u32 = 0xffffff8c;
const FAINT: u32 = 0xffffff14;
const HAIRLINE: u32 = 0xffffff1f;

#[derive(Clone, Copy, PartialEq)]
struct Sample {
    cpu: f32,
    /// Percent of one GPU's time, like `cpu` is of one core's.
    gpu: f32,
    memory_mb: f32,
}

/// One sidebar row: the latest reading of a live process.
#[derive(Clone, PartialEq)]
struct Proc {
    pid: i32,
    name: Rc<str>,
    cpu: f32,
    gpu: f32,
    memory_mb: f32,
    /// Smoothed CPU for a sort order that doesn't jitter every sample.
    smooth: f32,
}

/// Sampler state per process, kept past exit while a panel shows it.
struct Track {
    start: u64,
    name: Rc<str>,
    cpu_ns: u64,
    /// Accumulated GPU time, if the process has ever used the GPU.
    gpu_ns: Option<u64>,
    at: Instant,
    smooth: f32,
    samples: VecDeque<Sample>,
}

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Cpu,
    Gpu,
    Memory,
    Name,
}

/// A panel; `pid` is None until a process is picked for it.
#[derive(Clone, PartialEq)]
struct Slot {
    id: u64,
    pid: Option<i32>,
}

/// What a drag carries: a sidebar process or a panel header.
#[derive(Clone, PartialEq)]
enum Dragged {
    Process(i32, Rc<str>),
    Panel(u64, Rc<str>),
}

#[derive(Clone, Copy, PartialEq)]
enum Zone {
    Before,
    Replace,
    After,
}

#[derive(Clone)]
struct App {
    procs: Signal<Vec<Proc>>,
    /// Filtered and sorted pids, in sidebar order.
    shown: Signal<Vec<i32>>,
    tracks: Rc<RefCell<HashMap<i32, Track>>>,
    tick: Signal<u64>,
    slots: Signal<Vec<Slot>>,
    active: Signal<u64>,
    hint: Signal<Option<(u64, Zone)>>,
    query: Signal<String>,
    sort: Signal<Sort>,
    /// The whole GPU's utilisation, where the driver reports it.
    device_gpu: Signal<Option<f32>>,
    list_offset: Signal<f32>,
    /// The pointer is over the list: hold its order still under the cursor.
    hovering: Rc<Cell<bool>>,
    /// Keyboard browsing holds the order briefly so ↑/↓ walk a stable list.
    held_until: Rc<Cell<Instant>>,
    input: zgui::input::InputDispatcher,
    next_id: Rc<Cell<u64>>,
    launched: Option<i32>,
    runtime: Runtime,
    scene: Rc<RefCell<Scene>>,
}

impl App {
    fn name_of(&self, pid: i32) -> Option<Rc<str>> {
        self.tracks.borrow().get(&pid).map(|t| t.name.clone())
    }
    fn alive(&self, pid: i32) -> bool {
        self.procs.with(|p| p.iter().any(|p| p.pid == pid))
    }
    fn active_pid(&self) -> Option<i32> {
        let active = self.active.with_untracked(|v| *v);
        self.slots
            .with_untracked(|s| s.iter().find(|s| s.id == active).and_then(|s| s.pid))
    }
    fn new_slot(&self, pid: Option<i32>) -> Slot {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        Slot { id, pid }
    }
    /// Show `pid` in the active panel, opening one if there are none.
    fn show(&self, pid: i32) {
        let active = self.active.with_untracked(|v| *v);
        let mut opened = None;
        self.slots
            .update(|slots| match slots.iter_mut().find(|s| s.id == active) {
                Some(slot) => slot.pid = Some(pid),
                None => {
                    let slot = self.new_slot(Some(pid));
                    opened = Some(slot.id);
                    slots.push(slot);
                }
            });
        if let Some(id) = opened {
            self.active.set(id);
        }
    }
    /// Open `pid` in a new panel right of the active one, which stays as is.
    fn split(&self, pid: Option<i32>) {
        if self.slots.with_untracked(Vec::len) >= MAX_PANELS {
            if let Some(pid) = pid {
                self.show(pid);
            }
            return;
        }
        let active = self.active.with_untracked(|v| *v);
        let slot = self.new_slot(pid);
        let id = slot.id;
        self.slots.update(|slots| {
            let at = slots
                .iter()
                .position(|s| s.id == active)
                .map_or(slots.len(), |i| i + 1);
            slots.insert(at, slot);
        });
        self.active.set(id);
    }
    fn close(&self, id: u64) {
        let mut next = None;
        self.slots.update(|slots| {
            if let Some(i) = slots.iter().position(|s| s.id == id) {
                slots.remove(i);
                next = slots
                    .get(i.min(slots.len().saturating_sub(1)))
                    .map(|s| s.id);
            }
        });
        if self.active.with_untracked(|v| *v) == id {
            self.active.set(next.unwrap_or(0));
        }
    }
    /// Apply a drop of `what` onto the panel `target`.
    fn drop_on(&self, what: &Dragged, target: u64, zone: Zone) {
        let full = self.slots.with_untracked(Vec::len) >= MAX_PANELS;
        let mut active = target;
        self.slots.update(|slots| {
            let Some(at) = slots.iter().position(|s| s.id == target) else {
                return;
            };
            match (what, zone) {
                (Dragged::Process(pid, _), Zone::Replace) => slots[at].pid = Some(*pid),
                (Dragged::Process(pid, _), _) if full => slots[at].pid = Some(*pid),
                (Dragged::Process(pid, _), side) => {
                    let slot = self.new_slot(Some(*pid));
                    active = slot.id;
                    slots.insert(at + usize::from(side == Zone::After), slot);
                }
                (Dragged::Panel(id, _), _) if *id == target => {}
                (Dragged::Panel(id, _), zone) => {
                    let Some(from) = slots.iter().position(|s| s.id == *id) else {
                        return;
                    };
                    active = *id;
                    if zone == Zone::Replace {
                        slots.swap(from, at);
                    } else {
                        let slot = slots.remove(from);
                        let at = slots.iter().position(|s| s.id == target).unwrap_or(0);
                        slots.insert(at + usize::from(zone == Zone::After), slot);
                    }
                }
            }
        });
        self.active.set(active);
    }
    /// Move the active panel's process by `step` rows through the sidebar.
    fn step(&self, step: isize, viewport: f32) {
        let current = self.active_pid();
        let Some(pid) = self.shown.with_untracked(|shown| {
            let index = current
                .and_then(|pid| shown.iter().position(|p| *p == pid))
                .map_or(0, |i| {
                    (i as isize + step).clamp(0, shown.len() as isize - 1) as usize
                });
            shown.get(index).copied()
        }) else {
            return;
        };
        self.held_until.set(Instant::now() + Duration::from_secs(3));
        self.show(pid);
        self.reveal(pid, viewport);
    }
    /// Scroll the sidebar just enough to show `pid`'s row.
    fn reveal(&self, pid: i32, viewport: f32) {
        let Some(index) = self
            .shown
            .with_untracked(|s| s.iter().position(|p| *p == pid))
        else {
            return;
        };
        let (top, bottom) = (index as f32 * ROW, (index + 1) as f32 * ROW);
        let offset = self.list_offset.with_untracked(|v| *v);
        if top < offset {
            self.list_offset.set(top);
        } else if bottom > offset + viewport {
            self.list_offset.set(bottom - viewport);
        }
    }
    fn cycle_panel(&self, step: isize) {
        let active = self.active.with_untracked(|v| *v);
        let next = self.slots.with_untracked(|slots| {
            let i = slots.iter().position(|s| s.id == active)? as isize;
            let n = slots.len() as isize;
            Some(slots[(i + step).rem_euclid(n) as usize].id)
        });
        if let Some(id) = next {
            self.active.set(id);
            if let Some(pid) = self.active_pid() {
                self.reveal(pid, f32::MAX);
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    Application::new()
        .window(WindowOptions {
            title: TITLE.into(),
            width: WIDTH,
            height: HEIGHT,
            transparent: true,
            decorations: cfg!(target_os = "macos"),
            min_size: Some((820., 640.)),
            ..Default::default()
        })
        .run(move |cx| {
            #[cfg(target_os = "macos")]
            {
                let window = cx.window.clone();
                cx.tasks.spawn(async move {
                    sleep(Duration::from_millis(1)).await;
                    if glass::inset_titlebar(TITLE) {
                        window.set_inner_size(WIDTH, HEIGHT);
                    }
                });
            }
            let error = cx.ui.signal(None::<String>);

            // Launch the target beside this binary unless attaching.
            let child: Rc<RefCell<Option<Child>>> = Rc::default();
            let launched = match attach_pid() {
                Some(pid) => Some(pid),
                None => match launch_target() {
                    Ok(process) => {
                        let pid = process.id() as i32;
                        *child.borrow_mut() = Some(process);
                        Some(pid)
                    }
                    Err(message) => {
                        error.set(Some(message));
                        None
                    }
                },
            };
            // The monitor owns the process it launched.
            cx.on_closed(move || {
                if let Some(mut process) = child.borrow_mut().take() {
                    let _ = process.kill();
                    let _ = process.wait();
                }
            });

            let app = App {
                procs: cx.ui.signal(Vec::new()),
                shown: cx.ui.signal(Vec::new()),
                tracks: Rc::default(),
                tick: cx.ui.signal(0),
                slots: cx.ui.signal(vec![Slot {
                    id: 1,
                    pid: launched,
                }]),
                active: cx.ui.signal(1),
                hint: cx.ui.signal(None),
                query: cx.ui.signal(String::new()),
                sort: cx.ui.signal(Sort::Cpu),
                device_gpu: cx.ui.signal(None),
                list_offset: cx.ui.signal(0.),
                hovering: Rc::default(),
                held_until: Rc::new(Cell::new(Instant::now())),
                input: cx.ui.input.clone(),
                next_id: Rc::new(Cell::new(2)),
                launched,
                runtime: cx.ui.runtime.clone(),
                scene: cx.ui.scene.clone(),
            };
            spawn_sampler(&cx.tasks, app.clone());
            // The sidebar order: filter and sort once per change, not per row.
            let order = {
                let app = app.clone();
                let mut last = None;
                cx.ui.runtime.effect(move || {
                    let needle = app.query.with(|q| q.trim().to_lowercase());
                    let sort = app.sort.get();
                    // A sample tick while the pointer is on the list (or
                    // dragging from it) keeps rows in place: exited ones go,
                    // new ones join the end. Filter and sort changes re-sort.
                    let steady =
                        last.replace((needle.clone(), sort)) == Some((needle.clone(), sort));
                    let mut rows: Vec<(i32, Rc<str>, f32, f32, f32)> = app.procs.with(|procs| {
                        procs
                            .iter()
                            .filter(|p| {
                                needle.is_empty()
                                    || p.name.to_lowercase().contains(&needle)
                                    || p.pid.to_string().starts_with(&needle)
                            })
                            .map(|p| (p.pid, p.name.clone(), p.smooth, p.memory_mb, p.gpu))
                            .collect()
                    });
                    let held = app.hovering.get()
                        || app.input.is_dragging()
                        || app.held_until.get() > Instant::now();
                    if steady && held {
                        let present: HashSet<i32> = rows.iter().map(|r| r.0).collect();
                        let mut order: Vec<i32> = app.shown.with_untracked(|shown| {
                            shown
                                .iter()
                                .copied()
                                .filter(|p| present.contains(p))
                                .collect()
                        });
                        let known: HashSet<i32> = order.iter().copied().collect();
                        order.extend(rows.iter().map(|r| r.0).filter(|p| !known.contains(p)));
                        app.shown.set(order);
                        return;
                    }
                    rows.sort_by(|a, b| match sort {
                        Sort::Cpu => b.2.total_cmp(&a.2).then_with(|| b.3.total_cmp(&a.3)),
                        Sort::Gpu => b.4.total_cmp(&a.4).then_with(|| b.2.total_cmp(&a.2)),
                        Sort::Memory => b.3.total_cmp(&a.3),
                        Sort::Name => a.1.to_lowercase().cmp(&b.1.to_lowercase()),
                    });
                    // The launched target leads the list.
                    if let Some(i) = rows.iter().position(|r| Some(r.0) == app.launched) {
                        let row = rows.remove(i);
                        rows.insert(0, row);
                    }
                    app.shown.set(rows.into_iter().map(|r| r.0).collect());
                })
            };
            // Keep the ordering effect for the window's lifetime.
            cx.on_closed(move || drop(order));

            let keys = app.clone();
            cx.render(
                row()
                    .w_full()
                    .h_full()
                    .bg(rgba(0x0a0d1466))
                    .text_color(rgb(0xffffff))
                    .focusable(true)
                    .on_event(move |event| {
                        if event.phase != EventPhase::Capture {
                            return;
                        }
                        let InputEvent::KeyDown { key, modifiers, .. } = &event.event else {
                            return;
                        };
                        let typing = !keys.query.with_untracked(String::is_empty);
                        match key {
                            Key::ArrowDown | Key::ArrowUp => {
                                let Ok(height) = keys
                                    .scene
                                    .try_borrow()
                                    .map(|scene| scene.bounds(event.current_target).height)
                                else {
                                    return;
                                };
                                let viewport = height - TITLEBAR - SEARCH - FOOTER;
                                keys.step(if *key == Key::ArrowDown { 1 } else { -1 }, viewport);
                            }
                            Key::Enter => keys.split(keys.active_pid()),
                            Key::ArrowLeft | Key::ArrowRight if !typing => {
                                keys.cycle_panel(if *key == Key::ArrowRight { 1 } else { -1 })
                            }
                            Key::Character(c) if c == "w" && modifiers.primary_shortcut() => {
                                keys.close(keys.active.with_untracked(|v| *v))
                            }
                            Key::Escape if typing => {
                                keys.query.set(String::new());
                            }
                            _ => return,
                        }
                        event.prevent_default();
                        event.stop_propagation();
                    })
                    .child(sidebar(&app, cx.window.clone()))
                    .child(workspace(&app, &error, cx.window.clone())),
            );
        })
}

fn attach_pid() -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|arg| arg == "--pid")
        .and_then(|i| args.get(i + 1))
        .and_then(|pid| pid.parse().ok())
}

/// The example to launch: `--app <name>`, the kitchen sink by default.
fn target() -> String {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|arg| arg == "--app")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "kitchen_sink".into())
}

fn launch_target() -> Result<Child, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let name = target();
    let path = exe.with_file_name(&name);
    Command::new(&path).spawn().map_err(|e| {
        format!(
            "Could not start {} ({e}). Build it with: cargo build --release -p zgui-desktop --example {name}",
            path.display()
        )
    })
}

/// Samples every readable process, so a newly opened panel already has its
/// last minute of history.
fn spawn_sampler(tasks: &zgui_desktop::TaskSpawner, app: App) {
    tasks.spawn(async move {
        loop {
            let open: HashSet<i32> = app
                .slots
                .with_untracked(|s| s.iter().filter_map(|s| s.pid).collect());
            let procs = sample(&mut app.tracks.borrow_mut(), &open);
            let device = usage::device_gpu();
            app.runtime.batch(|| {
                app.procs.set(procs);
                app.device_gpu.set(device);
                app.tick.update(|t| *t += 1);
            });
            sleep(INTERVAL).await;
        }
    });
}

fn sample(tracks: &mut HashMap<i32, Track>, open: &HashSet<i32>) -> Vec<Proc> {
    let now = Instant::now();
    let mut seen = HashSet::new();
    let mut procs = Vec::new();
    let gpu_times = usage::gpu_times();
    for pid in usage::pids() {
        let Some(usage) = usage::read(pid) else {
            continue;
        };
        seen.insert(pid);
        let memory_mb = usage.footprint as f32 / (1024. * 1024.);
        let track = match tracks.get_mut(&pid) {
            // A reused pid is a different process.
            Some(track) if track.start == usage.start => track,
            _ => {
                tracks.insert(
                    pid,
                    Track {
                        start: usage.start,
                        name: usage::name(pid).into(),
                        cpu_ns: usage.cpu_ns,
                        gpu_ns: gpu_times.get(&pid).copied(),
                        at: now,
                        smooth: 0.,
                        samples: VecDeque::new(),
                    },
                );
                let track = &tracks[&pid];
                procs.push(Proc {
                    pid,
                    name: track.name.clone(),
                    cpu: 0.,
                    gpu: 0.,
                    memory_mb,
                    smooth: 0.,
                });
                continue;
            }
        };
        let wall = now.duration_since(track.at).as_nanos().max(1) as f64;
        let cpu = (usage.cpu_ns.saturating_sub(track.cpu_ns) as f64 / wall * 100.) as f32;
        track.cpu_ns = usage.cpu_ns;
        let gpu_ns = gpu_times.get(&pid).copied();
        // A process that closes its last GPU client drops back to zero.
        let gpu = match (gpu_ns, track.gpu_ns) {
            (Some(now_ns), Some(then)) => (now_ns.saturating_sub(then) as f64 / wall * 100.) as f32,
            _ => 0.,
        };
        track.gpu_ns = gpu_ns;
        track.at = now;
        track.smooth = track.smooth * 0.8 + cpu * 0.2;
        if track.samples.len() == CAPACITY {
            track.samples.pop_front();
        }
        track.samples.push_back(Sample {
            cpu,
            gpu,
            memory_mb,
        });
        procs.push(Proc {
            pid,
            name: track.name.clone(),
            cpu,
            gpu,
            memory_mb,
            smooth: track.smooth,
        });
    }
    // Exited processes keep their history while a panel shows them.
    tracks.retain(|pid, _| seen.contains(pid) || open.contains(pid));
    procs
}

// ---------------------------------------------------------------- sidebar

fn white(alpha: u8) -> Color {
    Color(255, 255, 255, alpha)
}

fn tnum() -> FontFeatures {
    FontFeatures::new([(*b"tnum", 1)])
}

fn drag_region(view: View, window: WindowHandle) -> View {
    view.on_event(move |event| {
        if event.phase != EventPhase::Capture
            && matches!(
                event.event,
                InputEvent::PointerDown {
                    button: PointerButton::Primary,
                    ..
                }
            )
        {
            window.drag_window();
        }
    })
}

fn sidebar(app: &App, window: WindowHandle) -> View {
    let search = {
        let query = app.query.clone();
        let placeholder = text("Filter processes")
            .text_size(12.5)
            .text_color(white(90))
            .reactive_style(move || {
                if query.with(String::is_empty) {
                    Styles::new().flex()
                } else {
                    Styles::new().hidden()
                }
            });
        row()
            .w_full()
            .h(32.)
            .px(10.)
            .gap(8.)
            .items_center()
            .rounded(9.)
            .bg(white(10))
            .border(1.)
            .border_color(white(14))
            .child(
                overlay().grow().min_w(0.).child(placeholder).child(
                    text_input("Filter processes", app.query.clone())
                        .w_full()
                        .h(20.)
                        .text_size(12.5)
                        .bg(white(0))
                        .border(0.)
                        .p(0.)
                        .focus(|s| s),
                ),
            )
    };
    let sort = {
        let option = |label: &'static str, value: Sort| {
            let (read, write) = (app.sort.clone(), app.sort.clone());
            text(label)
                .text_size(10.5)
                .font_weight(600)
                .letter_spacing(0.6)
                .px(7.)
                .py(3.)
                .rounded(6.)
                .cursor(Cursor::Pointer)
                .reactive_style(move || {
                    if read.get() == value {
                        Styles::new().bg(white(22)).text_color(white(235))
                    } else {
                        Styles::new().bg(white(0)).text_color(white(110))
                    }
                })
                .on_click(move || {
                    write.set(value);
                })
        };
        row()
            .gap(2.)
            .child(option("CPU", Sort::Cpu))
            .child(option("GPU", Sort::Gpu))
            .child(option("MEM", Sort::Memory))
            .child(option("A–Z", Sort::Name))
    };
    let list = {
        let (count, key, build) = (app.shown.clone(), app.shown.clone(), app.clone());
        let hovering = app.hovering.clone();
        virtual_list(
            app.list_offset.clone(),
            ROW,
            4,
            move || count.with(Vec::len),
            move |index| key.with(|s| s.get(index).copied().unwrap_or(-1)),
            move |_, pid, _| process_row(&build, pid),
        )
        .scrollbar(true)
        .w_full()
        .grow()
        .min_h(0.)
        .px(8.)
        .on_event(move |event| match event.event {
            InputEvent::PointerMove { .. } => hovering.set(true),
            InputEvent::PointerLeave if event.target == event.current_target => hovering.set(false),
            _ => {}
        })
    };
    let footer = {
        let (procs, device_gpu) = (app.procs.clone(), app.device_gpu.clone());
        row()
            .w_full()
            .h(FOOTER)
            .shrink_0()
            .px(16.)
            .items_center()
            .border_edges(Insets {
                top: 1.,
                right: 0.,
                bottom: 0.,
                left: 0.,
            })
            .border_color(white(22))
            .child(
                text_signal(move || {
                    let device = device_gpu.get();
                    procs.with(|p| {
                        let cpu: f32 = p.iter().map(|p| p.cpu).sum();
                        match device {
                            Some(gpu) => {
                                format!("{} processes · {cpu:.0}% CPU · {gpu:.0}% GPU", p.len())
                            }
                            None => format!("{} processes · {cpu:.0}% CPU", p.len()),
                        }
                    })
                })
                .text_size(11.)
                .text_color(rgba(MUTED))
                .font_features(tnum()),
            )
            .child(div().grow())
            .child(
                text("↑↓ browse · ⏎ split")
                    .text_size(11.)
                    .text_color(white(80)),
            )
    };
    column()
        .w(SIDEBAR_WIDTH)
        .h_full()
        .shrink_0()
        .bg(white(10))
        .border_edges(Insets {
            top: 0.,
            right: 1.,
            bottom: 0.,
            left: 0.,
        })
        .border_color(white(24))
        .child(drag_region(
            row()
                .w_full()
                .h(TITLEBAR)
                .shrink_0()
                .pr(10.)
                .items_center()
                .justify_end()
                .child(sort),
            window,
        ))
        .child(
            column()
                .w_full()
                .h(SEARCH)
                .shrink_0()
                .px(10.)
                .pt(4.)
                .child(search),
        )
        .child(list)
        .child(footer)
}

fn process_row(app: &App, pid: i32) -> View {
    let proc = {
        let procs = app.procs.clone();
        move || procs.with(|p| p.iter().find(|p| p.pid == pid).cloned())
    };
    let name = {
        let proc = proc.clone();
        text_signal(move || proc().map(|p| p.name.to_string()).unwrap_or_default())
            .text_size(12.5)
            .font_weight(500)
            .truncate()
            .min_w(0.)
    };
    let detail = {
        let proc = proc.clone();
        let launched = app.launched == Some(pid);
        text_signal(move || {
            let (memory, gpu) = proc().map_or((0., 0.), |p| (p.memory_mb, p.gpu));
            let gpu = if gpu >= 0.05 {
                format!(" · GPU {gpu:.1}%")
            } else {
                String::new()
            };
            let tag = if launched { " · launched" } else { "" };
            format!("{pid} · {memory:.0} MB{gpu}{tag}")
        })
        .text_size(10.5)
        .text_color(rgba(MUTED))
        .font_features(tnum())
        .truncate()
        .min_w(0.)
    };
    let cpu = {
        let proc = proc.clone();
        text_signal(move || format!("{:.1}%", proc().map_or(0., |p| p.cpu)))
            .text_size(12.)
            .font_features(tnum())
    };
    // A 48px CPU meter; a busy core fills it.
    let meter = {
        let proc = proc.clone();
        div().w(48.).h(3.).rounded(1.5).bg(white(18)).child(
            div()
                .h(3.)
                .rounded(1.5)
                .bg(rgb(CPU_HUE))
                .reactive_style(move || {
                    let cpu = proc().map_or(0., |p| p.cpu);
                    Styles::new().w((cpu / 100. * 48.).clamp(0., 48.))
                }),
        )
    };
    let split = {
        let app = app.clone();
        glyph_button("+", 15.).on_click(move || app.split(Some(pid)))
    };
    let selected = {
        let (slots, active) = (app.slots.clone(), app.active.clone());
        move || {
            let active = active.get();
            slots.with(|s| {
                let open = s.iter().any(|s| s.pid == Some(pid));
                let current = s.iter().any(|s| s.id == active && s.pid == Some(pid));
                (current, open)
            })
        }
    };
    let dot = {
        let selected = selected.clone();
        div()
            .size(5., 5.)
            .rounded(2.5)
            .bg(rgb(ACCENT))
            .reactive_style(move || Styles::new().opacity(if selected().1 { 1. } else { 0. }))
    };
    let payload = {
        let app = app.clone();
        move || Dragged::Process(pid, app.name_of(pid).unwrap_or_else(|| "".into()))
    };
    let click = app.clone();
    let row_view = row()
        .w_full()
        .h(ROW)
        .pl(6.)
        .pr(6.)
        .gap(8.)
        .items_center()
        .rounded(9.)
        .cursor(Cursor::Pointer)
        .reactive_style(move || {
            if selected().0 {
                Styles::new().bg(white(26))
            } else {
                Styles::new().bg(white(0))
            }
        })
        .hover(|s| s.bg(white(14)))
        .on_click(move || click.show(pid))
        .on_drag(payload)
        .drag_preview_at_cursor((12., 10.), drag_badge)
        .child(dot)
        .child(column().w(0.).grow().gap(2.).child(name).child(detail))
        .child(column().items_end().gap(4.).child(cpu).child(meter))
        .child(split);
    div().w_full().h(ROW).pr(10.).child(row_view)
}

/// A 22px square button holding one centred glyph.
fn glyph_button(glyph: &str, size: f32) -> View {
    row()
        .size(22., 22.)
        .shrink_0()
        .items_center()
        .justify_center()
        .rounded(6.)
        .text_color(white(120))
        .cursor(Cursor::Pointer)
        .hover(|s| s.bg(white(24)).text_color(white(240)))
        .child(text(glyph).text_size(size))
}

fn drag_badge(dragged: &Dragged) -> View {
    let label = match dragged {
        Dragged::Process(pid, name) => format!("{name} · {pid}"),
        Dragged::Panel(_, name) => name.to_string(),
    };
    row()
        .px(12.)
        .py(7.)
        .gap(8.)
        .items_center()
        .rounded(10.)
        .bg(Color(38, 36, 58, 235))
        .border(1.)
        .border_color(rgb(ACCENT))
        .child(div().size(6., 6.).rounded(3.).bg(rgb(ACCENT)))
        .child(text(label).text_size(12.).text_color(white(240)))
}

// ---------------------------------------------------------------- panels

fn workspace(app: &App, error: &Signal<Option<String>>, window: WindowHandle) -> View {
    let panels = {
        let (keys, build) = (app.slots.clone(), app.clone());
        keyed(
            move || keys.with(|s| s.iter().map(|s| s.id).collect::<Vec<_>>()),
            move |id, _| panel(&build, id),
        )
        .w_full()
        .grow()
        .min_h(0.)
        .flex_row()
        .gap(12.)
    };
    let empty = {
        let (slots, app) = (app.slots.clone(), app.clone());
        column()
            .w_full()
            .grow()
            .items_center()
            .justify_center()
            .gap(6.)
            .rounded(14.)
            .border(1.)
            .border_color(white(24))
            .reactive_style(move || {
                if slots.with(Vec::is_empty) {
                    Styles::new().flex()
                } else {
                    Styles::new().hidden()
                }
            })
            .child(text("No panels open").text_size(15.).font_weight(500))
            .child(
                text("Pick a process in the sidebar, or drag one here")
                    .text_size(12.)
                    .text_color(rgba(MUTED)),
            )
            .on_drop(move |dragged: &Dragged, _| {
                if let Dragged::Process(pid, _) = dragged {
                    app.split(Some(*pid));
                }
            })
    };
    let status = {
        let (error, slots) = (error.clone(), app.slots.clone());
        text_signal(move || {
            error.get().unwrap_or_else(|| {
                let n = slots.with(Vec::len);
                format!(
                    "{n} panel{} · sampled every 0.5 s · last 60 s",
                    if n == 1 { "" } else { "s" }
                )
            })
        })
        .grow()
        .min_w(0.)
        .truncate()
        .text_size(12.)
        .text_color(rgba(MUTED))
    };
    column()
        .grow()
        .min_w(0.)
        .h_full()
        .px(16.)
        .pb(16.)
        .child(drag_region(
            row()
                .w_full()
                .h(TITLEBAR + 8.)
                .shrink_0()
                .gap(10.)
                .items_center()
                .child(text(TITLE).text_size(14.).font_weight(600))
                .child(status),
            window,
        ))
        .child(panels)
        .child(empty)
}

fn panel(app: &App, id: u64) -> View {
    let pid_of = {
        let slots = app.slots.clone();
        move || slots.with(|s| s.iter().find(|s| s.id == id).and_then(|s| s.pid))
    };
    let body = {
        let app = app.clone();
        switch(pid_of.clone(), move |pid, cx| match pid {
            Some(pid) => panel_body(cx, &app, pid),
            None => column()
                .w_full()
                .h_full()
                .items_center()
                .justify_center()
                .gap(6.)
                .child(text("Empty panel").text_size(14.).font_weight(500))
                .child(
                    text("↑↓ in the sidebar, click or drop a process")
                        .text_size(12.)
                        .text_color(rgba(MUTED)),
                ),
        })
        .w_full()
        .grow()
        .min_h(0.)
    };
    let header = {
        let (name_app, alive_app, close) = (app.clone(), app.clone(), app.clone());
        let (name_pid, alive_pid) = (pid_of.clone(), pid_of.clone());
        row()
            .w_full()
            .h(40.)
            .px(14.)
            .gap(9.)
            .items_center()
            .cursor(Cursor::Grab)
            .border_edges(Insets {
                top: 0.,
                right: 0.,
                bottom: 1.,
                left: 0.,
            })
            .border_color(white(18))
            .on_drag({
                let (app, pid_of) = (app.clone(), pid_of.clone());
                move || {
                    let name = pid_of()
                        .and_then(|pid| app.name_of(pid))
                        .unwrap_or_else(|| "Empty panel".into());
                    Dragged::Panel(id, name)
                }
            })
            .drag_preview_at_cursor((12., 10.), drag_badge)
            .child(div().size(8., 8.).rounded(4.).reactive_style(move || {
                // Ticks re-evaluate liveness as processes come and go.
                let color = match alive_pid() {
                    Some(pid) if alive_app.alive(pid) => 0x3fd08a,
                    Some(_) => 0xe66767,
                    None => 0x5a5d66,
                };
                Styles::new().bg(rgb(color))
            }))
            .child(
                text_signal(move || {
                    name_pid()
                        .map(|pid| {
                            let name = name_app.name_of(pid).unwrap_or_else(|| "?".into());
                            let exited = if name_app.alive(pid) {
                                ""
                            } else {
                                " · exited"
                            };
                            format!("{name}  ·  {pid}{exited}")
                        })
                        .unwrap_or_else(|| "No process".into())
                })
                .w(0.)
                .grow()
                .truncate()
                .text_size(13.)
                .font_weight(600)
                .font_features(tnum()),
            )
            .child(glyph_button("×", 16.).on_click(move || close.close(id)))
    };
    // Drop feedback: an accent bar on the inserting edge, a wash to replace.
    let hint = {
        let hint = app.hint.clone();
        div()
            .absolute()
            .left(0.)
            .top(0.)
            .w_full()
            .h_full()
            .rounded(14.)
            .reactive_style(move || match hint.get() {
                Some((target, Zone::Replace)) if target == id => Styles::new()
                    .flex()
                    .bg(Color(139, 124, 246, 36))
                    .border(2.)
                    .border_color(rgb(ACCENT)),
                Some((target, zone)) if target == id => Styles::new()
                    .flex()
                    .bg(white(0))
                    .border(0.)
                    .border_edges(if zone == Zone::Before {
                        Insets {
                            top: 0.,
                            right: 0.,
                            bottom: 0.,
                            left: 3.,
                        }
                    } else {
                        Insets {
                            top: 0.,
                            right: 3.,
                            bottom: 0.,
                            left: 0.,
                        }
                    })
                    .border_color(rgb(ACCENT)),
                _ => Styles::new().hidden(),
            })
    };
    let frame = {
        let active = app.active.clone();
        move || {
            if active.get() == id {
                Styles::new().border_color(white(70))
            } else {
                Styles::new().border_color(rgba(HAIRLINE))
            }
        }
    };
    let (focus, over, drop, end) = (app.clone(), app.clone(), app.clone(), app.hint.clone());
    column()
        .grow()
        .flex_basis(0.)
        .min_w(0.)
        .h_full()
        .rounded(14.)
        .bg(rgba(FAINT))
        .border(1.)
        .reactive_style(frame)
        .on_event(move |event| {
            if event.phase == EventPhase::Capture
                && matches!(event.event, InputEvent::PointerDown { .. })
            {
                focus.active.set(id);
            }
        })
        .on_event(move |event| {
            if event.phase == EventPhase::Capture {
                return;
            }
            let InputEvent::Drag(drag) = &event.event else {
                return;
            };
            if drag.payload.downcast_ref::<Dragged>().is_none() {
                return;
            }
            match drag.phase {
                DragPhase::Over => {
                    let Ok(bounds) = over
                        .scene
                        .try_borrow()
                        .map(|scene| scene.bounds(event.current_target))
                    else {
                        return;
                    };
                    let t = (drag.x - bounds.x) / bounds.width.max(1.);
                    let zone = if t < 0.3 {
                        Zone::Before
                    } else if t > 0.7 {
                        Zone::After
                    } else {
                        Zone::Replace
                    };
                    over.hint.set(Some((id, zone)));
                }
                DragPhase::Leave => {
                    over.hint.update(|hint| {
                        if hint.is_some_and(|(target, _)| target == id) {
                            *hint = None;
                        }
                    });
                }
                _ => {}
            }
        })
        .on_drop(move |dragged: &Dragged, _| {
            let zone = drop
                .hint
                .with_untracked(|v| *v)
                .filter(|(target, _)| *target == id)
                .map_or(Zone::Replace, |(_, zone)| zone);
            drop.hint.set(None);
            drop.drop_on(dragged, id, zone);
        })
        .on_drag_end(move |_: &Dragged, _| {
            end.set(None);
        })
        .child(header)
        .child(body)
        .child(hint)
}

fn panel_body(cx: &mut Context, app: &App, pid: i32) -> View {
    let samples = cx.state(VecDeque::<Sample>::new());
    let hover = cx.state(None::<usize>);
    {
        let (tick, tracks, samples) = (app.tick.clone(), app.tracks.clone(), samples.clone());
        let effect = cx.runtime().effect(move || {
            tick.get();
            if let Some(history) = tracks.borrow().get(&pid).map(|t| t.samples.clone()) {
                samples.set(history);
            }
        });
        cx.retain(effect);
    }
    let offset = cx.state(0_f32);
    scroll(offset).w_full().h_full().child(
        column()
            .w_full()
            .p(14.)
            .gap(12.)
            .child(stat_grid(&samples))
            .child(chart(
                ChartSpec {
                    title: "CPU",
                    unit: "%",
                    hue: CPU_HUE,
                    value: |s| s.cpu,
                    floor: 10.,
                },
                &samples,
                &hover,
                app.scene.clone(),
            ))
            .child(chart(
                ChartSpec {
                    title: "GPU",
                    unit: "%",
                    hue: GPU_HUE,
                    value: |s| s.gpu,
                    floor: 10.,
                },
                &samples,
                &hover,
                app.scene.clone(),
            ))
            .child(chart(
                ChartSpec {
                    title: "MEMORY",
                    unit: " MB",
                    hue: MEMORY_HUE,
                    value: |s| s.memory_mb,
                    floor: 50.,
                },
                &samples,
                &hover,
                app.scene.clone(),
            )),
    )
}

fn stat_grid(samples: &Signal<VecDeque<Sample>>) -> View {
    let stat = |label: &'static str, read: fn(&VecDeque<Sample>) -> Option<String>| {
        let samples = samples.clone();
        glass_card()
            .grow()
            .flex_basis(0.)
            .min_w(0.)
            .px(14.)
            .py(12.)
            .gap(4.)
            .child(eyebrow(label))
            .child(
                text_signal(move || samples.with(read).unwrap_or_else(|| "—".into()))
                    .text_size(22.)
                    .font_weight(300)
                    .truncate()
                    .font_features(tnum()),
            )
    };
    column()
        .w_full()
        .gap(10.)
        .child(
            row()
                .w_full()
                .gap(10.)
                .child(stat("CPU NOW", |s| {
                    s.back().map(|s| format!("{:.1}%", s.cpu))
                }))
                .child(stat("CPU AVG · 60 S", |s| {
                    (!s.is_empty()).then(|| {
                        format!(
                            "{:.1}%",
                            s.iter().map(|s| s.cpu).sum::<f32>() / s.len() as f32
                        )
                    })
                })),
        )
        .child(
            row()
                .w_full()
                .gap(10.)
                .child(stat("GPU NOW", |s| {
                    s.back().map(|s| format!("{:.1}%", s.gpu))
                }))
                .child(stat("GPU AVG · 60 S", |s| {
                    (!s.is_empty()).then(|| {
                        format!(
                            "{:.1}%",
                            s.iter().map(|s| s.gpu).sum::<f32>() / s.len() as f32
                        )
                    })
                })),
        )
        .child(
            row()
                .w_full()
                .gap(10.)
                .child(stat("MEMORY NOW", |s| {
                    s.back().map(|s| format!("{:.0} MB", s.memory_mb))
                }))
                .child(stat("MEMORY PEAK", |s| {
                    s.iter()
                        .map(|s| s.memory_mb)
                        .reduce(f32::max)
                        .map(|mb| format!("{mb:.0} MB"))
                })),
        )
}

struct ChartSpec {
    title: &'static str,
    unit: &'static str,
    hue: u32,
    value: fn(&Sample) -> f32,
    /// Smallest axis maximum, so near-zero series are not blown up.
    floor: f32,
}

/// A 1-2-2.5-5 "nice" ceiling for the axis.
fn nice_max(peak: f32, floor: f32) -> f32 {
    let target = (peak * 1.15).max(floor);
    let magnitude = 10_f32.powf(target.log10().floor());
    [1., 2., 2.5, 5., 10.]
        .into_iter()
        .map(|step| step * magnitude)
        .find(|candidate| *candidate >= target)
        .unwrap_or(10. * magnitude)
}

fn chart(
    spec: ChartSpec,
    samples: &Signal<VecDeque<Sample>>,
    hover: &Signal<Option<usize>>,
    scene: Rc<RefCell<Scene>>,
) -> View {
    let ChartSpec {
        title,
        unit,
        hue,
        value,
        floor,
    } = spec;
    let axis = {
        let samples = samples.clone();
        move || {
            nice_max(
                samples.with(|s| s.iter().map(value).fold(0., f32::max)),
                floor,
            )
        }
    };
    let readout = {
        let (samples, hover) = (samples.clone(), hover.clone());
        move || {
            samples
                .with(|s| {
                    let index = hover
                        .get()
                        .filter(|i| *i < s.len())
                        .or(s.len().checked_sub(1))?;
                    let ago = (s.len() - 1 - index) as f32 * INTERVAL.as_secs_f32();
                    let when = if ago == 0. {
                        "now".to_owned()
                    } else {
                        format!("{ago:.1} s ago")
                    };
                    Some(format!("{:.1}{unit} · {when}", value(&s[index])))
                })
                .unwrap_or_default()
        }
    };
    let y_labels = {
        let axis = axis.clone();
        move |fraction: f32| {
            let axis = axis.clone();
            text_signal(move || format!("{:.0}", axis() * fraction))
                .text_size(10.5)
                .text_color(rgba(MUTED))
                .font_features(tnum())
        }
    };
    let (events_hover, events_samples) = (hover.clone(), samples.clone());
    let plot = {
        let (samples, hover) = (samples.clone(), hover.clone());
        let axis = axis.clone();
        canvas(move |(w, h)| {
            let mut drawing = Canvas::new();
            // Recessive grid at 0, ½ and the top of the axis.
            let plot_h = h - 2. * INSET;
            for fraction in [0., 0.5, 1.] {
                let y = INSET + plot_h * (1. - fraction);
                drawing.fill(
                    Path::rectangle(Rect::new(0., y - 0.5, w, 1.)),
                    rgba(HAIRLINE),
                );
            }
            let top = axis();
            let hovered = hover.get();
            samples.with(|s| {
                if s.len() < 2 {
                    return;
                }
                let offset = CAPACITY - s.len();
                let x = |i: usize| {
                    INSET + (w - 2. * INSET) * (i + offset) as f32 / (CAPACITY - 1) as f32
                };
                let y = |v: f32| INSET + plot_h * (1. - (v / top).clamp(0., 1.));
                let mut line = Path::builder();
                let mut area = Path::builder();
                area.move_to(x(0), INSET + plot_h);
                for (i, sample) in s.iter().enumerate() {
                    let (px, py) = (x(i), y(value(sample)));
                    if i == 0 {
                        line.move_to(px, py);
                    } else {
                        line.line_to(px, py);
                    }
                    area.line_to(px, py);
                }
                area.line_to(x(s.len() - 1), INSET + plot_h).close();
                let (r, g, b) = ((hue >> 16) as u8, (hue >> 8) as u8, hue as u8);
                if let (Ok(area), Ok(fill)) = (
                    area.build(),
                    Brush::linear(
                        Point::new(0., 0.),
                        Point::new(0., h),
                        vec![
                            GradientStop {
                                offset: 0.,
                                color: Color(r, g, b, 70),
                            },
                            GradientStop {
                                offset: 1.,
                                color: Color(r, g, b, 0),
                            },
                        ],
                    ),
                ) {
                    drawing.fill(area, fill);
                }
                if let Ok(line) = line.build() {
                    drawing.stroke(
                        line,
                        rgb(hue),
                        Stroke {
                            width: 2.,
                            cap: LineCap::Round,
                            join: LineJoin::Round,
                            ..Default::default()
                        },
                    );
                }
                // Crosshair and a ringed marker at the hovered (or latest) sample.
                let index = hovered.filter(|i| *i < s.len());
                if let Some(i) = index {
                    drawing.fill(
                        Path::rectangle(Rect::new(x(i) - 0.5, 0., 1., h)),
                        rgba(0xffffff40),
                    );
                }
                let i = index.unwrap_or(s.len() - 1);
                let (px, py) = (x(i), y(value(&s[i])));
                let mut ring = Path::builder();
                ring.arc(Point::new(px, py), 6., 0., std::f32::consts::TAU);
                let mut dot = Path::builder();
                dot.arc(Point::new(px, py), 4., 0., std::f32::consts::TAU);
                if let (Ok(ring), Ok(dot)) = (ring.build(), dot.build()) {
                    drawing.fill(ring, rgb(0x1a1d24));
                    drawing.fill(dot, rgb(hue));
                }
            });
            drawing
        })
        .w_full()
        .h(CHART_HEIGHT)
        .on_event({
            let (hover, samples) = (events_hover, events_samples);
            move |event| {
                match event.event {
                    InputEvent::PointerMove { x, .. } => {
                        // Release the scene before setting signals: their
                        // effects write to it synchronously.
                        let Ok(bounds) = scene
                            .try_borrow()
                            .map(|scene| scene.bounds(event.current_target))
                        else {
                            return;
                        };
                        let fraction = ((x - bounds.x - INSET)
                            / (bounds.width - 2. * INSET).max(1.))
                        .clamp(0., 1.);
                        let slot = (fraction * (CAPACITY - 1) as f32).round() as usize;
                        let len = samples.with(VecDeque::len);
                        // Snap to the nearest sample; empty history to the left snaps to the oldest.
                        let index = (slot + len)
                            .saturating_sub(CAPACITY)
                            .min(len.saturating_sub(1));
                        hover.set(Some(index));
                    }
                    InputEvent::PointerLeave => {
                        hover.set(None);
                    }
                    _ => {}
                }
            }
        })
    };
    glass_card()
        .w_full()
        .p(16.)
        .gap(10.)
        .child(
            row()
                .w_full()
                .items_center()
                .justify_between()
                .child(eyebrow(title))
                .child(text_signal(readout).text_size(12.).font_features(tnum())),
        )
        .child(
            row()
                .w_full()
                .gap(10.)
                .child(
                    column()
                        .w(36.)
                        .h(CHART_HEIGHT)
                        .items_end()
                        .justify_between()
                        .child(y_labels(1.))
                        .child(y_labels(0.5))
                        .child(y_labels(0.)),
                )
                .child(column().grow().min_w(0.).child(plot)),
        )
        .child(
            row().w_full().pl(46.).justify_between().children(
                ["60 s ago", "30 s", "now"]
                    .map(|label| text(label).text_size(10.5).text_color(rgba(MUTED))),
            ),
        )
}

fn eyebrow(label: &str) -> View {
    text(label)
        .text_size(10.5)
        .font_weight(600)
        .letter_spacing(1.4)
        .text_color(rgba(0xffffff70))
}

fn glass_card() -> View {
    column()
        .rounded(12.)
        .bg(rgba(FAINT))
        .border(1.)
        .border_color(rgba(HAIRLINE))
}

/// Per-process CPU time and physical footprint (what Activity Monitor shows).
#[cfg(target_os = "macos")]
mod usage {
    use std::sync::OnceLock;
    #[repr(C)]
    #[derive(Default)]
    struct RusageInfoV2 {
        uuid: [u8; 16],
        user_time: u64,
        system_time: u64,
        pkg_idle_wkups: u64,
        interrupt_wkups: u64,
        pageins: u64,
        wired_size: u64,
        resident_size: u64,
        phys_footprint: u64,
        proc_start_abstime: u64,
        proc_exit_abstime: u64,
        child_user_time: u64,
        child_system_time: u64,
        child_pkg_idle_wkups: u64,
        child_interrupt_wkups: u64,
        child_pageins: u64,
        child_elapsed_abstime: u64,
        diskio_bytesread: u64,
        diskio_byteswritten: u64,
    }
    #[repr(C)]
    #[derive(Default)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut RusageInfoV2) -> i32;
        fn proc_listallpids(buffer: *mut i32, size: i32) -> i32;
        fn proc_pidpath(pid: i32, buffer: *mut u8, size: u32) -> i32;
        fn proc_name(pid: i32, buffer: *mut u8, size: u32) -> i32;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    const RUSAGE_INFO_V2: i32 = 2;

    pub struct Usage {
        pub cpu_ns: u64,
        pub footprint: u64,
        /// Distinguishes a reused pid.
        pub start: u64,
    }
    pub fn pids() -> Vec<i32> {
        // SAFETY: a null buffer asks for the current count.
        let count = unsafe { proc_listallpids(std::ptr::null_mut(), 0) }.max(0) as usize;
        // Room for processes started between the two calls.
        let mut pids = vec![0_i32; count + 64];
        let bytes = (pids.len() * size_of::<i32>()) as i32;
        // SAFETY: the buffer holds `bytes` writable bytes.
        let count = unsafe { proc_listallpids(pids.as_mut_ptr(), bytes) }.max(0) as usize;
        pids.truncate(count.min(pids.len()));
        pids.retain(|pid| *pid > 0);
        pids
    }
    /// The executable's file name (proc_name truncates at 32 bytes).
    pub fn name(pid: i32) -> String {
        let mut buffer = [0_u8; 4096];
        // SAFETY: the buffer is writable for its full length.
        let len = unsafe { proc_pidpath(pid, buffer.as_mut_ptr(), buffer.len() as u32) };
        if len > 0 {
            let path = String::from_utf8_lossy(&buffer[..len as usize]);
            if let Some(name) = path.rsplit('/').next().filter(|n| !n.is_empty()) {
                return name.to_owned();
            }
        }
        // SAFETY: as above.
        let len = unsafe { proc_name(pid, buffer.as_mut_ptr(), buffer.len() as u32) };
        if len > 0 {
            return String::from_utf8_lossy(&buffer[..len as usize]).into_owned();
        }
        format!("pid {pid}")
    }
    pub fn read(pid: i32) -> Option<Usage> {
        static TIMEBASE: OnceLock<(u32, u32)> = OnceLock::new();
        let mut info = RusageInfoV2::default();
        // SAFETY: the buffer is a correctly sized, writable rusage_info_v2.
        if unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V2, &mut info) } != 0 {
            return None;
        }
        // CPU times are in Mach absolute-time units (not ns on Apple silicon).
        let &(numer, denom) = TIMEBASE.get_or_init(|| {
            let mut timebase = Timebase::default();
            // SAFETY: writes two integers into a local.
            unsafe { mach_timebase_info(&mut timebase) };
            (timebase.numer, timebase.denom.max(1))
        });
        let ticks = info.user_time + info.system_time;
        let cpu_ns = (ticks as u128 * numer as u128 / denom as u128) as u64;
        Some(Usage {
            cpu_ns,
            footprint: info.phys_footprint,
            start: info.proc_start_abstime,
        })
    }

    // GPU time per process -----------------------------------------------------
    //
    // The GPU driver keeps per-client accounting in the IORegistry: every
    // `AGXDeviceUserClient` names its creator ("pid 395, WindowServer") and
    // lists `AppUsage` entries with `accumulatedGPUTime` in nanoseconds.
    // `IOAccelerator`'s `PerformanceStatistics` hold the device-wide
    // "Device Utilization %". Both are readable without privileges.
    use std::{
        collections::HashMap,
        ffi::{CStr, c_char, c_void},
    };
    type CfType = *const c_void;
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingServices(main: u32, matching: *mut c_void, it: *mut u32) -> i32;
        fn IOIteratorNext(it: u32) -> u32;
        fn IOObjectRelease(object: u32) -> i32;
        fn IORegistryEntryGetChildIterator(entry: u32, plane: *const c_char, it: *mut u32) -> i32;
        fn IORegistryEntryCreateCFProperty(
            entry: u32,
            key: CfType,
            allocator: CfType,
            options: u32,
        ) -> CfType;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: CfType,
            text: *const c_char,
            encoding: u32,
        ) -> CfType;
        fn CFStringGetCString(
            text: CfType,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> bool;
        fn CFGetTypeID(object: CfType) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFArrayGetTypeID() -> usize;
        fn CFDictionaryGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFArrayGetCount(array: CfType) -> isize;
        fn CFArrayGetValueAtIndex(array: CfType, index: isize) -> CfType;
        fn CFDictionaryGetValue(dictionary: CfType, key: CfType) -> CfType;
        fn CFNumberGetValue(number: CfType, kind: isize, value: *mut c_void) -> bool;
        fn CFRelease(object: CfType);
    }
    const UTF8: u32 = 0x0800_0100;
    const SINT64: isize = 4;

    /// An owned CFString key.
    struct Key(CfType);
    impl Key {
        fn new(text: &CStr) -> Self {
            // SAFETY: a NUL-terminated UTF-8 string; released in Drop.
            Self(unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) })
        }
    }
    impl Drop for Key {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: created above and not shared.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    /// Calls `visit` with each registry entry of `class`, then releases it.
    fn each_service(class: &CStr, mut visit: impl FnMut(u32)) {
        let mut iterator = 0_u32;
        // SAFETY: `IOServiceGetMatchingServices` consumes the matching
        // dictionary; the iterator and entries are released here.
        unsafe {
            let matching = IOServiceMatching(class.as_ptr());
            if matching.is_null() || IOServiceGetMatchingServices(0, matching, &mut iterator) != 0 {
                return;
            }
            loop {
                let entry = IOIteratorNext(iterator);
                if entry == 0 {
                    break;
                }
                visit(entry);
                IOObjectRelease(entry);
            }
            IOObjectRelease(iterator);
        }
    }

    /// Calls `visit` with each child of `entry` in the service plane. User
    /// clients are not registered services, so matching can't find them;
    /// they hang off their accelerator like `ioreg` shows.
    fn each_child(entry: u32, mut visit: impl FnMut(u32)) {
        let mut iterator = 0_u32;
        // SAFETY: a valid entry; the iterator and children are released.
        unsafe {
            if IORegistryEntryGetChildIterator(entry, c"IOService".as_ptr(), &mut iterator) != 0 {
                return;
            }
            loop {
                let child = IOIteratorNext(iterator);
                if child == 0 {
                    break;
                }
                visit(child);
                IOObjectRelease(child);
            }
            IOObjectRelease(iterator);
        }
    }

    /// A copied property of `entry`, released when dropped.
    struct Property(CfType);
    impl Property {
        fn of(entry: u32, key: &Key) -> Option<Self> {
            // SAFETY: a valid entry and key; the result is owned (Create rule).
            let value =
                unsafe { IORegistryEntryCreateCFProperty(entry, key.0, std::ptr::null(), 0) };
            // Lazily: an eager `Self(value)` would be dropped, releasing
            // null, when the entry lacks the property.
            (!value.is_null()).then(|| Self(value))
        }
    }
    impl Drop for Property {
        fn drop(&mut self) {
            // SAFETY: owned since `of`.
            unsafe { CFRelease(self.0) };
        }
    }

    fn is(object: CfType, type_id: usize) -> bool {
        // SAFETY: a valid CF object.
        !object.is_null() && unsafe { CFGetTypeID(object) } == type_id
    }

    fn number(object: CfType) -> Option<i64> {
        // SAFETY: type checked before reading.
        unsafe {
            if !is(object, CFNumberGetTypeID()) {
                return None;
            }
            let mut value = 0_i64;
            CFNumberGetValue(object, SINT64, (&raw mut value).cast()).then_some(value)
        }
    }

    fn string(object: CfType) -> Option<String> {
        // SAFETY: type checked; the buffer is writable for its length.
        unsafe {
            if !is(object, CFStringGetTypeID()) {
                return None;
            }
            let mut buffer = [0 as c_char; 256];
            CFStringGetCString(object, buffer.as_mut_ptr(), buffer.len() as isize, UTF8).then(
                || {
                    CStr::from_ptr(buffer.as_ptr())
                        .to_string_lossy()
                        .into_owned()
                },
            )
        }
    }

    /// "pid 395, WindowServer" → 395.
    pub fn creator_pid(creator: &str) -> Option<i32> {
        creator
            .strip_prefix("pid ")?
            .split(',')
            .next()?
            .trim()
            .parse()
            .ok()
    }

    /// Accumulated GPU nanoseconds per process with an open GPU client.
    pub fn gpu_times() -> HashMap<i32, u64> {
        let (creator_key, usage_key, time_key) = (
            Key::new(c"IOUserClientCreator"),
            Key::new(c"AppUsage"),
            Key::new(c"accumulatedGPUTime"),
        );
        let mut times = HashMap::new();
        let mut read = |entry: u32| {
            let Some(pid) = Property::of(entry, &creator_key)
                .and_then(|creator| string(creator.0))
                .and_then(|creator| creator_pid(&creator))
            else {
                return;
            };
            let Some(usage) = Property::of(entry, &usage_key) else {
                return;
            };
            // SAFETY: type checked; indices are within the count.
            let total: u64 = unsafe {
                if !is(usage.0, CFArrayGetTypeID()) {
                    return;
                }
                (0..CFArrayGetCount(usage.0))
                    .map(|i| CFArrayGetValueAtIndex(usage.0, i))
                    .filter(|item| is(*item, CFDictionaryGetTypeID()))
                    .filter_map(|item| number(CFDictionaryGetValue(item, time_key.0)))
                    .map(|ns| ns.max(0) as u64)
                    .sum()
            };
            *times.entry(pid).or_insert(0) += total;
        };
        each_service(c"IOAccelerator", |accelerator| {
            each_child(accelerator, &mut read)
        });
        times
    }

    /// The GPU's own "Device Utilization %", if the driver reports it.
    pub fn device_gpu() -> Option<f32> {
        let (stats_key, utilization_key) = (
            Key::new(c"PerformanceStatistics"),
            Key::new(c"Device Utilization %"),
        );
        let mut found = None;
        each_service(c"IOAccelerator", |entry| {
            if found.is_some() {
                return;
            }
            if let Some(stats) = Property::of(entry, &stats_key)
                && is(stats.0, unsafe { CFDictionaryGetTypeID() })
            {
                // SAFETY: a dictionary, checked above.
                found = number(unsafe { CFDictionaryGetValue(stats.0, utilization_key.0) })
                    .map(|percent| percent as f32);
            }
        });
        found
    }
}

#[cfg(not(target_os = "macos"))]
mod usage {
    /// No per-process GPU accounting here yet.
    pub fn gpu_times() -> std::collections::HashMap<i32, u64> {
        std::collections::HashMap::new()
    }
    pub fn device_gpu() -> Option<f32> {
        None
    }
    pub struct Usage {
        pub cpu_ns: u64,
        pub footprint: u64,
        pub start: u64,
    }
    pub fn pids() -> Vec<i32> {
        std::fs::read_dir("/proc")
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn name(pid: i32) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|name| name.trim_end().to_owned())
            .unwrap_or_else(|_| format!("pid {pid}"))
    }
    /// Linux: /proc/<pid>/stat CPU ticks (assumed 100 Hz) and resident memory.
    pub fn read(pid: i32) -> Option<Usage> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields: Vec<&str> = stat.rsplit(')').next()?.split_whitespace().collect();
        let ticks: u64 =
            fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
        let pages: u64 = fields.get(21)?.parse().ok()?;
        Some(Usage {
            cpu_ns: ticks * 10_000_000,
            footprint: pages * 4096,
            start: fields.get(19)?.parse().ok()?,
        })
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::usage;

    #[test]
    fn creator_names_its_pid() {
        assert_eq!(usage::creator_pid("pid 395, WindowServer"), Some(395));
        assert_eq!(usage::creator_pid("pid 7,x"), Some(7));
        assert_eq!(usage::creator_pid("WindowServer"), None);
    }

    #[test]
    fn window_server_has_gpu_time() {
        // Every logged-in Mac composites through WindowServer on the GPU.
        let times = usage::gpu_times();
        assert!(times.values().any(|ns| *ns > 0), "no GPU accounting read");
    }
}
