use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;

use gpui::{App, AppContext as _, Application, Bounds, WindowBounds, WindowOptions, px, size};

use crate::live_dashboard::{LiveDashboard, ROUTES};
use crate::live_source::Worker;
use crate::live_timer::Timer;
use crate::performance::Options;

pub fn probe(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let root = recording(options)?;
    let collector = if options.source.start_collector {
        Some(Collector::start(&root, options.source.recording.is_some())?)
    } else {
        None
    };
    crate::live_probe::run(root, options.seconds.unwrap_or(30))?;
    drop(collector);
    Ok(())
}

pub fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    if !ROUTES.iter().any(|(route, _)| *route == options.page) {
        return Err(format!("Unknown live route {}", options.page).into());
    }
    if options.scrub {
        return Err("--scrub requires --reference".into());
    }
    if options.smoke() {
        return Err("--smoke requires --reference".into());
    }
    let root = recording(&options)?;
    let collector = if options.source.start_collector {
        Some(Collector::start(&root, options.source.recording.is_some())?)
    } else {
        None
    };
    let worker = Worker::start(root)?;
    let application = Application::new();
    crate::diagnostics::verify_text_backend(&application.text_system())?;
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1320.0), px(900.0)), cx);
        let window = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Flightdeck GPUI · Live".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        match cx.open_window(window, move |_, cx| create_dashboard(worker, options, cx)) {
            Ok(_) => cx.activate(true),
            Err(error) => {
                eprintln!("Live window failed: {error}");
                cx.quit();
            }
        }
    });
    drop(collector);
    Ok(())
}

fn create_dashboard(worker: Worker, options: Options, cx: &mut App) -> gpui::Entity<LiveDashboard> {
    let entity = cx.new(|_| {
        let mut dashboard = LiveDashboard::new(worker, options.page, options.seconds);
        dashboard.animate = true;
        dashboard.reveal_hardware = options.hardware;
        dashboard
    });
    let weak = entity.downgrade();
    let application = cx.to_async();
    let timer = Timer::start(move || {
        if let Err(error) = application.update(|cx| {
            if let Err(error) = weak.update(cx, LiveDashboard::tick) {
                eprintln!("Live view update: {error}");
            }
        }) {
            eprintln!("Live application update: {error}");
        }
    });
    entity.update(cx, |dashboard, _| match timer {
        Ok(timer) => dashboard.timer = Some(timer),
        Err(error) => dashboard.error = Some(error),
    });
    let state = Rc::clone(&entity.read(cx).hardware_state);
    if let Err(error) = crate::hardware_clock::install(&entity, &state, cx) {
        entity.update(cx, |dashboard, _| dashboard.error = Some(error));
    }
    entity
}

pub fn recording(options: &Options) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = &options.source.recording {
        return Ok(path.clone());
    }
    let home = std::env::var_os("HOME").ok_or("HOME is unavailable")?;
    Ok(PathBuf::from(home).join("Library/Application Support/Flightdeck/flightdeck.recording"))
}

struct Collector {
    child: Child,
}

impl Collector {
    fn start(root: &std::path::Path, isolated: bool) -> Result<Self, Box<dyn std::error::Error>> {
        let executable = std::env::current_exe()?;
        let helper = executable
            .parent()
            .ok_or("Executable has no directory")?
            .parent()
            .ok_or("Executable has no bundle parent")?
            .join("Helpers/collector");
        let binary = if helper.is_file() {
            helper
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/release/collector")
        };
        let ring = if isolated {
            format!("flightdeck-gpui-{}", std::process::id())
        } else {
            "flightdeck".into()
        };
        let child = Command::new(binary)
            .arg("--ring")
            .arg(ring)
            .arg("--recording")
            .arg(root)
            .arg("--parent-pid")
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        eprintln!("Started owned collector {}", child.id());
        Ok(Self { child })
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(status)) => eprintln!("Owned collector exited: {status}"),
            Ok(None) => match i32::try_from(self.child.id()) {
                Ok(pid) => {
                    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
                        eprintln!("Collector shutdown: {}", std::io::Error::last_os_error());
                    }
                }
                Err(error) => eprintln!("Collector pid: {error}"),
            },
            Err(error) => eprintln!("Collector status: {error}"),
        }
    }
}
