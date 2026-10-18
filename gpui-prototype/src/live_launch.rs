use std::path::PathBuf;
use std::rc::Rc;

use gpui::{App, Bounds, WindowBounds, WindowOptions, px, size};

use crate::live_dashboard::{LiveDashboard, ROUTES};
use crate::live_source::Worker;
use crate::performance::Options;

#[path = "collector_lifecycle_application.rs"]
mod collection;
#[path = "collector_lifecycle.rs"]
mod lifecycle;
#[path = "collector_lifecycle_readiness.rs"]
mod readiness;

pub use collection::{collector_status, request_quit, retry_collector};

pub fn probe(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let root = recording(options)?;
    let mut collector = if options.source.start_collector {
        let mut collector = collector_session(&root, options)?;
        collector.tick(false);
        if let Some(failure) = collector.failure() {
            return Err(failure.to_owned().into());
        }

        Some(collector)
    } else {
        None
    };
    let prepared = match &mut collector {
        Some(collector) => readiness::wait(collector),
        None => Ok(()),
    };
    let outcome = prepared.and_then(|()| {
        if options.source.probe == crate::performance::ProbeMode::Soak {
            crate::live_probe::soak(root, options.seconds.unwrap_or(900))
        } else {
            crate::live_probe::run(root, options.seconds.unwrap_or(30))
        }
    });
    let stopped = match &mut collector {
        Some(collector) => collector.stop_and_wait(),
        None => Ok(()),
    };
    if let Err(error) = outcome {
        if let Err(shutdown) = stopped {
            eprintln!("Owned collector shutdown: {shutdown}");
        }
        return Err(error.into());
    }

    stopped?;
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
    let collector = collector_session(&root, &options)?;
    let worker = Worker::start(root)?;
    let application = crate::diagnostics::application();
    crate::diagnostics::verify_text_backend(&application.text_system())?;
    application.run(move |cx: &mut App| {
        collection::install(collector, cx);
        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                request_quit(cx);
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
                request_quit(cx);
            }
        }
    });
    Ok(())
}

fn create_dashboard(worker: Worker, options: Options, cx: &mut App) -> gpui::Entity<LiveDashboard> {
    let entity =
        crate::native_workflows::create_dashboard(worker, options.page, options.seconds, cx);
    entity.update(cx, |dashboard, _| {
        dashboard.animate = true;
        dashboard.reveal_hardware = options.hardware;
    });
    if let Err(error) = collection::start_timer(&entity, cx) {
        entity.update(cx, |dashboard, _| dashboard.error = Some(error));
    }
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

fn collector_session(
    root: &std::path::Path,
    options: &Options,
) -> Result<lifecycle::CollectorSession, Box<dyn std::error::Error>> {
    let mode = if options.source.start_collector {
        lifecycle::Mode::Explicit
    } else {
        if options.source.recording.is_some() {
            lifecycle::Mode::Attach
        } else {
            lifecycle::Mode::Auto
        }
    };
    Ok(lifecycle::CollectorSession::new(lifecycle::Configuration {
        root: root.to_owned(),
        binary: lifecycle::binary(&std::env::current_exe()?)?,
        isolated: options.source.recording.is_some(),
        mode,
    }))
}
