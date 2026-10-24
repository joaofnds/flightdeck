//! What the user set in the live window and expects to find again: where the window stands
//! and how large it is, the page it shows, the stretch of time the charts show, and the chart
//! series hidden from their legends. One file holds them, so they outlast closing the window
//! and quitting the app.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use gpui::{
    App, Bounds, DisplayId, Entity, Global, Pixels, Size, Window, WindowBounds, point, px, size,
};
use serde::{Deserialize, Serialize};

use crate::live_dashboard::{LiveDashboard, ROUTES};
use crate::performance::DEFAULT_PAGE;
use crate::time_range::Range;

#[cfg(test)]
#[path = "live_preferences_tests.rs"]
mod tests;

/// The most series kept hidden.
const HIDDEN_SERIES_MAX: usize = 256;
/// The longest key of a hidden series kept, in bytes.
const SERIES_KEY_BYTES_MAX: usize = 256;
/// The largest file read. JSON writes a byte of a key as at most six.
const FILE_BYTES_MAX: usize = 1024 * 1024;
/// The shortest side of a live window in points, which macOS holds the window to.
pub(super) const WINDOW_SIDE_MIN: f32 = 200.0;
/// The longest side of a frame in points. Past it the frame is damaged.
const WINDOW_SIDE_MAX: f32 = 32_768.0;

const _: () = assert!(HIDDEN_SERIES_MAX * (SERIES_KEY_BYTES_MAX * 6 + 8) + 4096 < FILE_BYTES_MAX);
const _: () = assert!(WINDOW_SIDE_MIN < WINDOW_SIDE_MAX);

/// Where the live window's preferences are kept. They are the user's, whichever recording
/// the window shows.
pub(super) fn path() -> Result<PathBuf, String> {
    Ok(super::application_support()?.join("preferences.json"))
}

/// Reads the preferences at `path` and keeps them for every live window this run opens.
/// Preferences that cannot be read are reported, and none are kept, so the file that holds
/// them is never written over.
pub(super) fn install(path: PathBuf, cx: &mut App) {
    let preferences = match load(&path) {
        Ok(preferences) => preferences,
        Err(error) => {
            eprintln!("Flightdeck keeps no preferences this run: {error}");
            return;
        }
    };

    cx.set_global(Store {
        path,
        saved: preferences.clone(),
        current: preferences,
        failed: None,
    });
    cx.on_app_quit(install_quit_callback).detach();
}

fn install_quit_callback(cx: &mut App) -> std::future::Ready<()> {
    if cx.has_global::<Store>() {
        cx.global_mut::<Store>().save();
    }
    std::future::ready(())
}

/// The page a live window opens on: the one the launch named, else the one the user left
/// open, else the default page.
pub(super) fn page(named_page: Option<String>, cx: &App) -> String {
    if let Some(page) = named_page {
        return page;
    }

    let Some(page) = cx
        .try_global::<Store>()
        .and_then(|store| store.current.page.as_ref())
    else {
        return DEFAULT_PAGE.to_owned();
    };

    page.clone()
}

/// The range a live window opens at: the one the launch named, else the one the user left it
/// at, else the default history.
pub(super) fn range(named_range: Option<Range>, cx: &App) -> Range {
    if let Some(range) = named_range {
        return range;
    }

    let Some(store) = cx.try_global::<Store>() else {
        return Range::History;
    };

    store.current.range()
}

/// Whether the user opted in to moving fans and airflow on Overview.
pub(super) fn animate_fans(cx: &App) -> bool {
    cx.try_global::<Store>()
        .is_some_and(|store| store.current.animate_fans)
}

/// Keeps the choice immediately and reports a failed save to the Settings page.
pub(super) fn set_animate_fans(enabled: bool, cx: &mut App) -> Result<(), String> {
    if !cx.has_global::<Store>() {
        return Err(
            "Preferences are unavailable. This change applies to this session only.".into(),
        );
    }

    let store = cx.global_mut::<Store>();
    store.current.animate_fans = enabled;
    store.save();
    match animation_save_notice(cx) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(super) fn animation_save_notice(cx: &App) -> Option<String> {
    let Some(store) = cx.try_global::<Store>() else {
        return Some(
            "Preferences are unavailable. This change applies to this session only.".into(),
        );
    };

    if store.current.animate_fans == store.saved.animate_fans {
        return None;
    }

    store.failed.as_ref().map(|(_, error)| {
        format!("Could not save Animate fans. This change applies to this session only. {error}")
    })
}

/// Where a live window opens: where the last one stood, at its size there, or centered at
/// `default` when no window was kept. A window whose display is gone, or no longer holds its
/// corner, opens centered at the size it was kept at.
pub(super) fn placement(default: Size<Pixels>, cx: &App) -> (WindowBounds, Option<DisplayId>) {
    let Some(frame) = cx
        .try_global::<Store>()
        .and_then(|store| store.current.window.as_ref())
    else {
        return placement_centered(default, cx);
    };

    let display = frame.display.as_deref().and_then(|saved| {
        cx.displays()
            .into_iter()
            .find(|display| display_uuid(display.as_ref()).as_deref() == Some(saved))
    });
    let Some((bounds, display)) = display.and_then(|display| {
        placement_on_display(frame, display.bounds()).map(|bounds| (bounds, display.id()))
    }) else {
        return placement_centered(frame.size(), cx);
    };

    let bounds = match frame.state {
        FrameState::Windowed => WindowBounds::Windowed(bounds),
        FrameState::Maximized => WindowBounds::Maximized(bounds),
        FrameState::Fullscreen => WindowBounds::Fullscreen(bounds),
    };
    (bounds, Some(display))
}

fn placement_centered(size: Size<Pixels>, cx: &App) -> (WindowBounds, Option<DisplayId>) {
    (
        WindowBounds::Windowed(Bounds::centered(None, size, cx)),
        None,
    )
}

/// The frame on a display of `display` bounds, or none when its top left corner, where the
/// window is dragged from, is off the display. macOS gives a window's corner relative to its
/// display, so the display's own origin plays no part.
fn placement_on_display(frame: &Frame, display: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
    let corner = point(px(frame.x), px(frame.y));
    let relative = Bounds::new(point(px(0.0), px(0.0)), display.size);
    if relative.contains(&corner) {
        Some(Bounds::new(corner, frame.size()))
    } else {
        None
    }
}

/// Shows the series last hidden in the live window that just opened, and keeps its frame from
/// now on.
pub(super) fn follow(dashboard: &Entity<LiveDashboard>, window: &mut Window, cx: &mut App) {
    let Some(store) = cx.try_global::<Store>() else {
        return;
    };

    let hidden = store.current.hidden_series.clone();
    assert!(hidden.len() <= HIDDEN_SERIES_MAX);
    dashboard.update(cx, |dashboard, cx| {
        dashboard.hidden = hidden;
        cx.observe_window_bounds(window, |_, window, cx| {
            // A frame no window could keep, such as one macOS reports mid-transition, leaves
            // the last one kept.
            if let Some(frame) = Frame::of(window, cx) {
                cx.global_mut::<Store>().current.window = Some(frame);
            }
        })
        .detach();
    });
    // The window closes with its page, so what changed since the last tick is kept here.
    cx.observe_release(dashboard, |dashboard, cx| {
        remember(dashboard, cx);
    })
    .detach();
}

/// Keeps what the page shows now and writes the file when anything kept has changed.
pub(super) fn remember(dashboard: &LiveDashboard, cx: &mut App) {
    if !cx.has_global::<Store>() {
        return;
    }

    let store = cx.global_mut::<Store>();
    store
        .current
        .remember_view(&dashboard.request.route, dashboard.range, &dashboard.hidden);
    store.save();
}

/// The preferences of this run, what the file holds of them, and the preferences whose
/// write failed last.
struct Store {
    path: PathBuf,
    current: Preferences,
    saved: Preferences,
    failed: Option<(Preferences, String)>,
}

impl Global for Store {}

impl Store {
    fn save(&mut self) {
        if self.current == self.saved {
            self.failed = None;
            return;
        }

        match write(&self.path, &self.current) {
            Ok(()) => {
                self.saved = self.current.clone();
                self.failed = None;
            }
            Err(error) => {
                // The write is tried again on each tick until it succeeds, and reported once
                // for each change rather than on every tick.
                if self.failed.as_ref().map(|(preferences, _)| preferences) != Some(&self.current) {
                    eprintln!("Save Flightdeck preferences: {error}");
                    self.failed = Some((self.current.clone(), error));
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    window: Option<Frame>,
    /// The page the window showed last, one of the sidebar's routes.
    page: Option<String>,
    /// The minutes of the range the menu picked last, or none for the default history.
    range_minutes: Option<u64>,
    hidden_series: Vec<String>,
    /// Motion is opt-in, including when older preferences omit this field.
    animate_fans: bool,
}

impl Preferences {
    /// Keeps `page`, `range` when the menu picked it, and `hidden`, as far as they are kept.
    fn remember_view(&mut self, page: &str, range: Range, hidden: &[String]) {
        if self.page.as_deref() != Some(page) {
            self.page = Some(page.to_owned());
        }
        match range {
            Range::History => self.range_minutes = None,
            // The menu picks whole minutes, so the division is exact.
            Range::Last(seconds) => self.range_minutes = Some(seconds / 60),
            // A selection names seconds of this run, which mean nothing to the next.
            Range::Between { .. } => {}
        }
        if !kept_series(hidden).eq(self.hidden_series.iter()) {
            self.hidden_series = kept_series(hidden).cloned().collect();
        }
    }

    /// The range kept, read as the menu reads its minutes.
    fn range(&self) -> Range {
        let Some(minutes) = self.range_minutes else {
            return Range::History;
        };

        // No range is zero minutes long.
        if minutes == 0 {
            return Range::History;
        }

        Range::minutes(minutes)
    }
}

fn kept_series(hidden: &[String]) -> impl Iterator<Item = &String> {
    hidden
        .iter()
        .filter(|key| key.len() <= SERIES_KEY_BYTES_MAX)
        .take(HIDDEN_SERIES_MAX)
}

/// A window's frame in points on the display it stood on, which macOS names by a UUID that
/// outlasts a restart where its number does not. Every frame kept fits a window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Frame {
    display: Option<String>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    state: FrameState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FrameState {
    Windowed,
    Maximized,
    Fullscreen,
}

impl Frame {
    /// The frame `window` has now, or none when no window could keep it.
    fn of(window: &Window, cx: &App) -> Option<Self> {
        let (bounds, state) = match window.window_bounds() {
            WindowBounds::Windowed(bounds) => (bounds, FrameState::Windowed),
            WindowBounds::Maximized(bounds) => (bounds, FrameState::Maximized),
            WindowBounds::Fullscreen(bounds) => (bounds, FrameState::Fullscreen),
        };
        let frame = Self {
            display: window
                .display(cx)
                .and_then(|display| display_uuid(display.as_ref())),
            x: bounds.origin.x.as_f32(),
            y: bounds.origin.y.as_f32(),
            width: bounds.size.width.as_f32(),
            height: bounds.size.height.as_f32(),
            state,
        };
        if frame.fits() { Some(frame) } else { None }
    }

    /// Whether a window could have this frame, which a frame read from a damaged file may not.
    fn fits(&self) -> bool {
        for side in [self.width, self.height] {
            if !(WINDOW_SIDE_MIN..=WINDOW_SIDE_MAX).contains(&side) {
                return false;
            }
        }
        if !self.x.is_finite() {
            return false;
        }

        self.y.is_finite()
    }

    fn size(&self) -> Size<Pixels> {
        assert!(self.fits());
        size(px(self.width), px(self.height))
    }
}

fn display_uuid(display: &dyn gpui::PlatformDisplay) -> Option<String> {
    match display.uuid() {
        Ok(uuid) => Some(uuid.to_string()),
        Err(error) => {
            eprintln!("Read the display of the Flightdeck window: {error}");
            None
        }
    }
}

/// The preferences at `path`, the defaults when none were saved, and the defaults too when
/// the file is damaged, which is moved aside so the next save cannot overwrite it.
fn load(path: &Path) -> Result<Preferences, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Preferences::default());
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let limit = u64::try_from(FILE_BYTES_MAX + 1).map_err(|error| error.to_string())?;
    let mut text = Vec::new();
    file.take(limit)
        .read_to_end(&mut text)
        .map_err(|error| format!("{}: {error}", path.display()))?;

    let parsed = if FILE_BYTES_MAX < text.len() {
        Err(format!("larger than {FILE_BYTES_MAX} bytes"))
    } else {
        serde_json::from_slice::<Preferences>(&text).map_err(|error| error.to_string())
    };
    match parsed {
        Ok(preferences) => Ok(load_parsed(path, preferences)),
        Err(error) => {
            let aside = sibling(path, ".corrupt");
            std::fs::rename(path, &aside)
                .map_err(|rename| format!("{}: {rename}", path.display()))?;
            eprintln!(
                "Flightdeck preferences damaged, moved to {}: {error}",
                aside.display()
            );
            Ok(Preferences::default())
        }
    }
}

/// The parsed preferences held to what is kept: a frame no window could have and a page this
/// build does not have are dropped, and the hidden series are bounded.
fn load_parsed(path: &Path, mut preferences: Preferences) -> Preferences {
    if let Some(page) = &preferences.page {
        if !ROUTES.iter().any(|(route, _)| route == page) {
            eprintln!(
                "Flightdeck preferences at {} name no page this build has, {page}, so the \
                 window opens on its usual page",
                path.display()
            );
            preferences.page = None;
        }
    }
    if let Some(frame) = &preferences.window {
        if !frame.fits() {
            eprintln!(
                "Flightdeck preferences at {} hold a damaged window frame, so the window opens \
                 at its usual size",
                path.display()
            );
            preferences.window = None;
        }
    }
    preferences.hidden_series = kept_series(&preferences.hidden_series).cloned().collect();
    preferences
}

/// Writes `preferences` whole to a file beside `path` and renames it over `path`, so a crash
/// leaves the old file or the new one and never part of either. The file beside it carries
/// the process id, so two Flightdeck processes saving at once never write into one file.
fn write(path: &Path, preferences: &Preferences) -> Result<(), String> {
    assert!(preferences.hidden_series.len() <= HIDDEN_SERIES_MAX);
    if let Some(page) = &preferences.page {
        assert!(ROUTES.iter().any(|(route, _)| route == page));
    }
    let describe = |error: std::io::Error| format!("{}: {error}", path.display());
    let text = serde_json::to_vec_pretty(preferences).map_err(|error| error.to_string())?;
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory).map_err(describe)?;
    }

    let partial = sibling(path, &format!(".partial-{}", std::process::id()));
    let mut file = std::fs::File::create(&partial).map_err(describe)?;
    file.write_all(&text).map_err(describe)?;
    file.sync_all().map_err(describe)?;
    std::fs::rename(&partial, path).map_err(describe)
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}
