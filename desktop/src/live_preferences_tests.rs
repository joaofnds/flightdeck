use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use gpui::{Bounds, TestAppContext, point, px, size};

use super::*;
use crate::time_range::{LOADED_SECONDS, Range};

#[test]
fn reads_the_defaults_when_nothing_was_saved() {
    // Goal: a first launch opens as Flightdeck always has.
    // Method: read a path where no file was ever written.
    let directory = Directory::new("missing");

    let preferences = load(&directory.file()).unwrap();

    assert_eq!(Preferences::default(), preferences);
}

#[test]
fn reads_back_what_it_saved() {
    // Goal: what the user set in one run is there in the next.
    // Method: save a window frame, a page, a range and a hidden series, then read the file
    // again.
    let directory = Directory::new("round-trip");
    let mut preferences = Preferences::default();
    preferences.remember_view("memory", Range::Last(300), &["cpu\tSafari".into()]);
    preferences.window = Some(frame(900.0, 700.0));

    write(&directory.file(), &preferences).unwrap();

    assert_eq!(preferences, load(&directory.file()).unwrap());
}

#[test]
fn sets_an_unparseable_file_aside_and_reads_the_defaults() {
    // Goal: a damaged file costs the user their preferences but never the launch, and stays
    // on disk for whoever wants to look at it.
    // Method: write text that is no preferences file, read it, and look for the file set aside.
    let directory = Directory::new("unparseable");
    std::fs::write(directory.file(), "{\"window\": ").unwrap();

    let preferences = load(&directory.file()).unwrap();

    assert_eq!(Preferences::default(), preferences);
    assert!(!directory.file().exists());
    assert_eq!(
        "{\"window\": ",
        std::fs::read_to_string(directory.path("preferences.json.corrupt")).unwrap()
    );
}

#[test]
fn sets_a_file_too_large_to_read_aside() {
    // Goal: a file grown past what is read is never parsed, however valid its start.
    // Method: write valid preferences padded one byte past the bound, and read them.
    let directory = Directory::new("oversized");
    let mut text = b"{\"range_minutes\": 5}".to_vec();
    text.resize(FILE_BYTES_MAX + 1, b' ');
    std::fs::write(directory.file(), &text).unwrap();

    let preferences = load(&directory.file()).unwrap();

    assert_eq!(Preferences::default(), preferences);
    assert!(directory.path("preferences.json.corrupt").exists());
}

#[test]
fn drops_a_window_frame_no_window_could_have() {
    // Goal: a damaged frame opens the window at its usual size rather than one nobody can use,
    // and costs nothing else the file holds.
    // Method: save frames with a side too short and too long, and read them.
    let directory = Directory::new("damaged-frame");
    for (width, height) in [(10.0, 700.0), (900.0, 1.0e9)] {
        let preferences = Preferences {
            window: Some(frame(width, height)),
            range_minutes: Some(5),
            ..Preferences::default()
        };
        write(&directory.file(), &preferences).unwrap();

        let read = load(&directory.file()).unwrap();

        assert_eq!(None, read.window, "{width} by {height}");
        assert_eq!(Some(5), read.range_minutes, "{width} by {height}");
    }
}

#[test]
fn keeps_the_range_chosen_from_the_menu() {
    // Goal: the stretch of time picked from the range menu is the one the window opens on.
    // Method: remember a range the menu picks and read it back.
    let mut preferences = Preferences::default();

    preferences.remember_view("overview", Range::minutes(5), &[]);

    assert_eq!(Range::minutes(5), preferences.range());
}

#[test]
fn keeps_the_default_history_chosen_from_the_menu() {
    // Goal: picking the default history again replaces a shorter range kept before.
    // Method: remember a range, then the default history, and read the range back.
    let mut preferences = Preferences::default();
    preferences.remember_view("overview", Range::minutes(5), &[]);

    preferences.remember_view("overview", Range::History, &[]);

    assert_eq!(Range::History, preferences.range());
}

#[test]
fn keeps_the_longest_range_the_menu_offers() {
    // Goal: the longest range, which reaches back as far as the viewer loads, is kept too.
    // Method: remember the range of all the seconds loaded and read it back.
    let mut preferences = Preferences::default();

    preferences.remember_view("overview", Range::Last(LOADED_SECONDS), &[]);

    assert_eq!(Range::Last(LOADED_SECONDS), preferences.range());
}

#[test]
fn keeps_the_menu_range_through_a_selection_on_a_chart() {
    // Goal: a stretch dragged across a chart names seconds of this run, so the next run opens
    // on the range the menu picked before it.
    // Method: remember a range from the menu, then a selection, and read the range back.
    let mut preferences = Preferences::default();
    preferences.remember_view("overview", Range::minutes(10), &[]);

    preferences.remember_view(
        "overview",
        Range::Between {
            first: 10,
            last: 90,
        },
        &[],
    );

    assert_eq!(Range::minutes(10), preferences.range());
}

#[test]
fn reads_a_kept_range_as_the_menu_reads_its_minutes() {
    // Goal: a file edited by hand, or by a build that loads more history, never opens the
    // window on a range this build cannot show.
    // Method: read no minutes at all, and more minutes than this build loads.
    let longest = LOADED_SECONDS / 60;
    for (minutes, range) in [
        (0, Range::History),
        (longest + 1, Range::Last(LOADED_SECONDS)),
    ] {
        let preferences = Preferences {
            range_minutes: Some(minutes),
            ..Preferences::default()
        };

        assert_eq!(range, preferences.range(), "{minutes} minutes");
    }
}

#[test]
fn keeps_a_bounded_number_of_hidden_series() {
    // Goal: the file stays small whatever the user hides.
    // Method: hide more series than are kept, and one whose key is too long to keep.
    let mut hidden: Vec<String> = (0..=HIDDEN_SERIES_MAX)
        .map(|index| format!("cpu\tApp {index}"))
        .collect();
    hidden.insert(0, "x".repeat(SERIES_KEY_BYTES_MAX + 1));
    let mut preferences = Preferences::default();

    preferences.remember_view("overview", Range::History, &hidden);

    assert_eq!(HIDDEN_SERIES_MAX, preferences.hidden_series.len());
    assert_eq!("cpu\tApp 0", preferences.hidden_series[0]);
}

#[test]
fn bounds_the_hidden_series_it_reads() {
    // Goal: a file grown by hand cannot hand the window more hidden series than it keeps.
    // Method: write more series than are kept, past the bound the save holds, and read them.
    let directory = Directory::new("hidden");
    let hidden: Vec<String> = (0..=HIDDEN_SERIES_MAX)
        .map(|index| format!("cpu\tApp {index}"))
        .collect();
    let text = serde_json::json!({ "hidden_series": hidden }).to_string();
    std::fs::write(directory.file(), text).unwrap();

    let read = load(&directory.file()).unwrap();

    assert_eq!(HIDDEN_SERIES_MAX, read.hidden_series.len());
}

#[test]
fn places_the_window_where_it_was_on_its_display() {
    // Goal: the window opens where the user left it, on any display, since macOS gives its
    // corner relative to the display it stands on.
    // Method: place a frame on a display beside the main one.
    let display = Bounds::new(point(px(1920.0), px(0.0)), size(px(1920.0), px(1080.0)));

    let placed = placement_on_display(&frame(900.0, 700.0), display);

    assert_eq!(
        Some(Bounds::new(
            point(px(40.0), px(30.0)),
            size(px(900.0), px(700.0))
        )),
        placed
    );
}

#[test]
fn places_no_window_whose_corner_left_its_display() {
    // Goal: a window never opens where the user cannot reach it, such as on a display that
    // is now smaller.
    // Method: place a frame whose corner lies past its display's edge.
    let display = Bounds::new(point(px(0.0), px(0.0)), size(px(30.0), px(1080.0)));

    assert_eq!(None, placement_on_display(&frame(900.0, 700.0), display));
}

#[gpui::test]
fn opens_a_window_whose_display_is_gone_at_its_size(cx: &mut TestAppContext) {
    // Goal: unplugging the display the window stood on costs its place but not its size.
    // Method: keep a frame on a display no longer attached, and place a window.
    let preferences = Preferences {
        window: Some(frame(900.0, 700.0)),
        ..Preferences::default()
    };

    let (bounds, display) = cx.update(|cx| {
        cx.set_global(store(PathBuf::from("/nonexistent"), preferences));
        placement(size(px(1320.0), px(900.0)), cx)
    });

    assert_eq!(None, display);
    let WindowBounds::Windowed(bounds) = bounds else {
        panic!("{bounds:?}");
    };
    assert_eq!(size(px(900.0), px(700.0)), bounds.size);
}

#[gpui::test]
fn opens_on_the_page_the_user_left_open(cx: &mut TestAppContext) {
    // Goal: a launch that names no page opens the window on the page last shown.
    // Method: keep a page, then ask for the page of a launch that named none.
    let mut preferences = Preferences::default();
    preferences.remember_view("memory", Range::History, &[]);

    let opened = cx.update(|cx| {
        cx.set_global(store(PathBuf::from("/nonexistent"), preferences));
        page(None, cx)
    });

    assert_eq!("memory", opened);
}

#[gpui::test]
fn opens_on_the_page_the_launch_named(cx: &mut TestAppContext) {
    // Goal: a page named at launch opens over the page last shown.
    // Method: keep a page, then ask for the page of a launch that named another.
    let mut preferences = Preferences::default();
    preferences.remember_view("memory", Range::History, &[]);

    let opened = cx.update(|cx| {
        cx.set_global(store(PathBuf::from("/nonexistent"), preferences));
        page(Some("cpu".into()), cx)
    });

    assert_eq!("cpu", opened);
}

#[gpui::test]
fn opens_on_the_default_page_when_none_was_kept(cx: &mut TestAppContext) {
    // Goal: with no page kept, a launch that names none opens the default page.
    // Method: ask for the page with preferences that hold no page, then with none kept at all,
    // as for a timed run.
    let kept_nothing = cx.update(|cx| {
        cx.set_global(store(PathBuf::from("/nonexistent"), Preferences::default()));
        page(None, cx)
    });
    let keeps_nothing = cx.update(|cx| {
        cx.remove_global::<Store>();
        page(None, cx)
    });

    assert_eq!(DEFAULT_PAGE, kept_nothing);
    assert_eq!(DEFAULT_PAGE, keeps_nothing);
}

#[gpui::test]
fn opens_at_the_range_the_user_left(cx: &mut TestAppContext) {
    // Goal: a launch that names no range opens the window at the range last shown.
    // Method: keep a range, then ask for the range of a launch that named none.
    let mut preferences = Preferences::default();
    preferences.remember_view("memory", Range::Last(300), &[]);

    let opened = cx.update(|cx| {
        cx.set_global(store(PathBuf::from("/nonexistent"), preferences));
        range(None, cx)
    });

    assert_eq!(Range::Last(300), opened);
}

#[gpui::test]
fn opens_at_the_range_the_launch_named(cx: &mut TestAppContext) {
    // Goal: a range named at launch opens over the range last shown.
    // Method: keep a range, then ask for the range of a launch that named another.
    let mut preferences = Preferences::default();
    preferences.remember_view("memory", Range::Last(300), &[]);

    let opened = cx.update(|cx| {
        cx.set_global(store(PathBuf::from("/nonexistent"), preferences));
        range(Some(Range::Last(3600)), cx)
    });

    assert_eq!(Range::Last(3600), opened);
}

#[gpui::test]
fn opens_a_window_that_keeps_no_preferences_at_the_range_named(cx: &mut TestAppContext) {
    // Goal: a timed run, which keeps no preferences, opens at the range its launch named.
    // Method: with no preferences kept, ask for the range with the hour named.
    let opened = cx.update(|cx| range(Some(Range::Last(3600)), cx));

    assert_eq!(Range::Last(3600), opened);
}

#[gpui::test]
fn opens_a_window_that_keeps_no_preferences_and_names_no_range_at_the_history(
    cx: &mut TestAppContext,
) {
    // Goal: a timed run that names no range opens at the default history.
    // Method: with no preferences kept, ask for the range with none named.
    let opened = cx.update(|cx| range(None, cx));

    assert_eq!(Range::History, opened);
}

#[test]
fn drops_a_page_this_build_does_not_have() {
    // Goal: a page removed or renamed since the file was saved opens the window on its usual
    // page, and costs nothing else the file holds.
    // Method: write a page no route has, with a range, and read them.
    let directory = Directory::new("unknown-page");
    let text = serde_json::json!({ "page": "removed", "range_minutes": 5 }).to_string();
    std::fs::write(directory.file(), text).unwrap();

    let read = load(&directory.file()).unwrap();

    assert_eq!(None, read.page);
    assert_eq!(Some(5), read.range_minutes);
}

#[gpui::test]
fn keeps_no_preferences_from_a_file_it_cannot_read(cx: &mut TestAppContext) {
    // Goal: a file Flightdeck cannot read is never written over with preferences of this run.
    // Method: make the file unreadable, install from it, and look for the preferences kept.
    let directory = Directory::new("unreadable");
    std::fs::write(directory.file(), "{}").unwrap();
    std::fs::set_permissions(directory.file(), std::fs::Permissions::from_mode(0o000)).unwrap();

    let kept = cx.update(|cx| {
        install(directory.file(), cx);
        cx.has_global::<Store>()
    });

    std::fs::set_permissions(directory.file(), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!kept);
}

#[test]
fn saves_again_after_a_write_failed() {
    // Goal: a write that fails once, such as on a full disk, still saves the change once the
    // disk recovers.
    // Method: block the directory the file goes in, save, unblock it, and save again.
    let directory = Directory::new("failed-write");
    let blocked = directory.path("blocked");
    std::fs::write(&blocked, "").unwrap();
    let mut preferences = Preferences::default();
    preferences.remember_view("overview", Range::minutes(5), &[]);
    let mut kept = store(blocked.join("preferences.json"), Preferences::default());
    kept.current = preferences.clone();

    kept.save();
    std::fs::remove_file(&blocked).unwrap();
    kept.save();

    assert_eq!(
        preferences,
        load(&blocked.join("preferences.json")).unwrap()
    );
}

fn frame(width: f32, height: f32) -> Frame {
    Frame {
        display: Some("display".into()),
        x: 40.0,
        y: 30.0,
        width,
        height,
        state: FrameState::Windowed,
    }
}

fn store(path: PathBuf, preferences: Preferences) -> Store {
    Store {
        path,
        saved: preferences.clone(),
        current: preferences,
        failed: None,
    }
}

/// A directory of its own for each test, so tests run in parallel without sharing a file,
/// removed when the test ends however it ends.
struct Directory {
    root: PathBuf,
}

impl Directory {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "flightdeck-preferences-{name}-{}",
            std::process::id()
        ));
        if root.exists() {
            std::fs::remove_dir_all(&root).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn file(&self) -> PathBuf {
        self.path("preferences.json")
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            eprintln!("Remove {}: {error}", self.root.display());
        }
    }
}
