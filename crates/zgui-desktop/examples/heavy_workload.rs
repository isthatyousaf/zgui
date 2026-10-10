//! The heavy dashboard comparison scene (see zgui_workload::heavy), shared
//! with the GPUI and QuickGUI adapters: every live value is a reactive text
//! or style reading the `live` tick, and the table is a virtual list.
//! Run with ZGUI_MODE=both ZGUI_SECONDS=10.
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};
use zgui_desktop::{Application, WindowOptions};
use zgui_workload::{Mode, heavy::*};

#[path = "support/heavy_view.rs"]
mod view;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    Application::new()
        .window(WindowOptions {
            title: "zgui heavy".into(),
            width: WIDTH as f64,
            height: HEIGHT as f64,
            resizable: false,
            ..Default::default()
        })
        .run(|cx| {
            let work = Rc::new(RefCell::new(Heavy::from_env()));
            let live = cx.ui.signal(0_u64);
            let scroll = cx.ui.signal(0_f32);
            let log = cx.ui.signal(work.borrow().visible_log());
            cx.render(view::dashboard(live.clone(), scroll.clone(), log.clone()));
            cx.on_closed({
                let work = work.clone();
                move || {
                    println!(
                        "{{\"framework\":\"zgui\",\"scene\":\"heavy\",\"ticks\":{}}}",
                        work.borrow().frames
                    )
                }
            });
            let runtime = cx.ui.runtime.clone();
            let window = cx.window.clone();
            cx.tasks.spawn(async move {
                let start = Instant::now();
                let (mode, seconds) = {
                    let work = work.borrow();
                    (work.mode, work.seconds)
                };
                let end = (seconds > 0.).then(|| start + Duration::from_secs_f64(seconds));
                if mode == Mode::Idle {
                    if let Some(end) = end {
                        zgui::timer::sleep_until(end).await;
                        window.close();
                    }
                    return;
                }
                let period = Heavy::period();
                let mut next = start + period;
                loop {
                    zgui::timer::sleep_until(end.map_or(next, |end| end.min(next))).await;
                    if end.is_some_and(|end| Instant::now() >= end) {
                        window.close();
                        break;
                    }
                    runtime.batch(|| {
                        let mut work = work.borrow_mut();
                        work.tick();
                        if matches!(mode, Mode::Stream | Mode::Both) {
                            live.set(work.live);
                            log.set(work.visible_log());
                        }
                        if matches!(mode, Mode::Scroll | Mode::Both) {
                            scroll.set(work.scroll);
                        }
                    });
                    next += period;
                    let now = Instant::now();
                    if next <= now {
                        next = now + period;
                    }
                }
            });
        })
}
