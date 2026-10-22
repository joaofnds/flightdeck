use std::path::PathBuf;
use std::rc::Rc;

use gpui::{App, WindowOptions, px, size};

use crate::live_dashboard::{LiveDashboard, ROUTES};
use crate::live_source::Worker;
use crate::performance::Options;

#[path = "live_background.rs"]
mod background;
#[path = "collector_lifecycle_application.rs"]
mod collection;
#[path = "collector_lifecycle.rs"]
mod lifecycle;
#[path = "live_preferences.rs"]
mod preferences;
#[path = "collector_lifecycle_readiness.rs"]
mod readiness;

pub use collection::{collector_status, retry_collector};

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

    let control = control(&options)?;
    let root = recording(&options)?;
    let collector = collector_session(&root, &options)?;
    let worker = match (
        options.source.recording.is_some(),
        options.source.start_collector,
    ) {
        (true, false) => Worker::start_recorded(root.clone())?,
        _ => Worker::start(root.clone())?,
    };
    let reopening = reopening(root, &options, control.is_some());
    let preferences_path = preferences_path(&options, control.is_some());
    let opening = Opening {
        worker,
        start: Start {
            page: options.page,
            seconds: options.seconds,
            hardware: options.hardware,
        },
        control,
    };
    let application = crate::diagnostics::application();
    crate::diagnostics::verify_text_backend(&application.text_system())?;
    if reopening.is_some() {
        application.on_reopen(background::show);
    }
    application.run(move |cx: &mut App| {
        collection::install(collector, cx);
        if let Some(preferences_path) = preferences_path {
            preferences::install(preferences_path, cx);
        }
        if let Some(reopening) = reopening {
            background::install(reopening, cx);
            background::install_menu_bar(cx);
        } else {
            cx.on_window_closed(|cx, _window_id| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
        }
        if let Err(error) = open_window(opening, cx) {
            eprintln!("Live window failed: {error}");
            cx.quit();
        }
    });
    Ok(())
}

/// What the window opens on.
struct Start {
    page: String,
    seconds: Option<u32>,
    hardware: bool,
}

/// What a live window is made of when it opens.
struct Opening {
    worker: Worker,
    start: Start,
    control: Option<Control>,
}

/// Collection outlives the window only for a launch that follows the live readings on the
/// screen. A recording opened read-only collects nothing, a timed run quits by itself, and a
/// window under the control socket is never on the screen.
fn reopening(root: PathBuf, options: &Options, controlled: bool) -> Option<background::Reopening> {
    if !personal(options, controlled) {
        return None;
    }

    if options.source.recording.is_some() {
        if !options.source.start_collector {
            return None;
        }
    }

    Some(background::Reopening {
        root,
        page: options.page.clone(),
        hardware: options.hardware,
    })
}

/// Whether the window is the user's own, on the screen until they close it. A window under
/// the control socket is never on the screen, and a timed run quits by itself to report how
/// it ran.
fn personal(options: &Options, controlled: bool) -> bool {
    if controlled {
        return false;
    }

    options.seconds.is_none()
}

/// Where the user's preferences are kept, or none for a window that is not theirs. A launch
/// that cannot find them goes on without them.
fn preferences_path(options: &Options, controlled: bool) -> Option<PathBuf> {
    if !personal(options, controlled) {
        return None;
    }

    match preferences::path() {
        Ok(path) => Some(path),
        Err(error) => {
            eprintln!("Flightdeck keeps no preferences this run: {error}");
            None
        }
    }
}

fn open_window(opening: Opening, cx: &mut App) -> Result<(), String> {
    let Opening {
        worker,
        start,
        control,
    } = opening;
    let title = worker.mode().title();
    let hidden = control.is_some();
    let (bounds, display_id) = preferences::placement(size(px(1320.0), px(900.0)), cx);
    let window = WindowOptions {
        window_bounds: Some(bounds),
        display_id,
        window_min_size: Some(size(
            px(preferences::WINDOW_SIDE_MIN),
            px(preferences::WINDOW_SIDE_MIN),
        )),
        // A window under the control socket stays off the screen and never takes focus.
        show: !hidden,
        focus: !hidden,
        // The window bar draws the title row itself, with the window buttons inside it as
        // the approved prototype shows them.
        titlebar: Some(gpui::TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(18.0), px(17.0))),
        }),
        ..Default::default()
    };

    cx.open_window(window, move |window, cx| {
        let dashboard = create_dashboard(worker, start, cx);
        background::keep_reader(&dashboard, cx);
        // The window under the control socket stands in for a shown window and is never on the
        // screen, so it reads its page whatever macOS reports.
        if control.is_none() {
            crate::live_dashboard::observe_visibility(&dashboard, window, cx);
        }
        install_control(control, &dashboard, window, cx);
        preferences::follow(&dashboard, window, cx);
        dashboard
    })
    .map_err(|error| error.to_string())?;

    if !hidden {
        cx.activate(true);
    }
    Ok(())
}

#[cfg(feature = "automation")]
type Control = std::sync::mpsc::Receiver<crate::control::Command>;
#[cfg(not(feature = "automation"))]
type Control = std::convert::Infallible;

/// Binds the control socket before the window opens, so a failure stops the launch.
fn control(options: &Options) -> Result<Option<Control>, Box<dyn std::error::Error>> {
    let Some(path) = &options.control else {
        return Ok(None);
    };

    #[cfg(feature = "automation")]
    return Ok(Some(crate::control::serve(path)?));
    #[cfg(not(feature = "automation"))]
    return Err(format!(
        "--control {} needs a build with --features automation",
        path.display()
    )
    .into());
}

#[cfg(feature = "automation")]
fn install_control(
    control: Option<Control>,
    dashboard: &gpui::Entity<LiveDashboard>,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    if let Some(control) = control {
        crate::control::install(control, dashboard.clone(), window, cx);
    }
}

/// Without the feature no control can exist, since `control` refuses the flag.
#[cfg(not(feature = "automation"))]
fn install_control(
    control: Option<Control>,
    _dashboard: &gpui::Entity<LiveDashboard>,
    _window: &mut gpui::Window,
    _cx: &mut App,
) {
    if let Some(control) = control {
        match control {}
    }
}

fn create_dashboard(worker: Worker, start: Start, cx: &mut App) -> gpui::Entity<LiveDashboard> {
    let entity = crate::native_workflows::create_dashboard(worker, start.page, start.seconds, cx);
    entity.update(cx, |dashboard, _| {
        dashboard.animate = true;
        dashboard.reveal_hardware = start.hardware;
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

    Ok(application_support()?.join("flightdeck.recording"))
}

/// The directory Flightdeck keeps the user's files in.
fn application_support() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is unavailable")?;
    Ok(PathBuf::from(home).join("Library/Application Support/Flightdeck"))
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
