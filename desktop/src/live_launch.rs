use std::path::PathBuf;

use gpui::{App, WindowOptions, px, size};
use viewer::store::Store;

use crate::live_dashboard::{LiveDashboard, ROUTES};
use crate::live_source::Worker;
use crate::performance::Options;
use crate::time_range::Range;

#[path = "live_app_nap.rs"]
mod app_nap;
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

pub(crate) fn animate_fans(cx: &App) -> bool {
    preferences::animate_fans(cx)
}

pub(crate) fn animation_save_notice(cx: &App) -> Option<String> {
    preferences::animation_save_notice(cx)
}

pub(crate) fn set_animate_fans(enabled: bool, cx: &mut App) -> Result<(), String> {
    preferences::set_animate_fans(enabled, cx)
}

pub fn probe(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let root = recording(options)?;
    let mut collector = if options.source.start_collector {
        let mut collector = collector_session(&root, options)?;
        collector.tick();
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

pub fn run(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    if !ROUTES.iter().any(|(route, _)| *route == options.page()) {
        return Err(format!("Unknown live route {}", options.page()).into());
    }

    if options.scrub {
        return Err("--scrub requires --reference".into());
    }

    if options.smoke() {
        return Err("--smoke requires --reference".into());
    }

    let control = control(options)?;
    let root = recording(options)?;
    let collector = collector_session(&root, options)?;
    // A recording opened read-only has no ring to follow, so the store does not hold it yet.
    let (worker, store) = match (
        options.source.recording.is_some(),
        options.source.start_collector,
    ) {
        (true, false) => (Worker::start_recorded(root.clone())?, None),
        _ => (Worker::start(root.clone())?, Some(Store::start(&root)?)),
    };
    let reopening = reopening(root, options, control.is_some());
    let preferences_path = preferences_path(options, control.is_some());
    let opening = Opening {
        worker,
        state: None,
        start: start(options),
        control,
    };
    let application = crate::diagnostics::application();
    crate::diagnostics::verify_text_backend(&application.text_system())?;
    if reopening.is_some() {
        application.on_reopen(background::show);
    }
    application.run(move |cx: &mut App| {
        cx.set_global(app_nap::AppNapExemption::begin());
        if let Some(store) = store {
            crate::live_dashboard::install_store(store, cx);
        }

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
    /// The page `--page` named, which opens over the page the user left open.
    named_page: Option<String>,
    /// The range `--minutes` named, which opens over the range the user left.
    named_range: Option<Range>,
    seconds: Option<u32>,
    hardware: bool,
}

#[cfg(test)]
#[path = "live_launch_tests.rs"]
mod tests;

fn start(options: &Options) -> Start {
    Start {
        named_page: options.named_page.clone(),
        named_range: options
            .named_minutes
            .map(|minutes| Range::minutes(u64::from(minutes))),
        seconds: options.seconds,
        hardware: options.hardware,
    }
}

/// What a live window is made of when it opens.
struct Opening {
    worker: Worker,
    state: Option<crate::live_dashboard::WindowState>,
    start: Start,
    control: Option<Control>,
}

/// Collection outlives the window only for a launch that follows the live readings on the
/// screen. A recording opened read-only collects nothing, a timed run quits by itself, and a
/// window under the control socket keeps an isolated session.
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
        hardware: options.hardware,
    })
}

/// Whether the window is the user's own, on the screen until they close it. A window under
/// the control socket is isolated from preferences, and a timed run quits to report how
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
        state,
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
        // A controlled window starts hidden until an explicit show command.
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
        let dashboard = create_dashboard(worker, start, state, cx);
        crate::hardware_motion::observe_window(&dashboard, window, cx);
        background::keep_reader(&dashboard, cx);
        // Controlled windows read their pages even while hidden.
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

fn create_dashboard(
    worker: Worker,
    start: Start,
    state: Option<crate::live_dashboard::WindowState>,
    cx: &mut App,
) -> gpui::Entity<LiveDashboard> {
    let restored = state.is_some();
    let page = preferences::page(start.named_page, cx);
    let range = preferences::range(start.named_range, cx);
    let entity = match state {
        Some(state) => crate::native_workflows::restore_dashboard(worker, state, cx),
        None => crate::native_workflows::create_dashboard(worker, page, start.seconds, cx),
    };
    entity.update(cx, |dashboard, cx| {
        dashboard.reveal_hardware = start.hardware;
        if !restored {
            dashboard.set_range(range, cx);
        }
    });
    if let Err(error) = collection::start_timer(&entity, cx) {
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
