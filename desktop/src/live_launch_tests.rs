use collector::recording::private::{atomic_write, create_private_dir};
use collector::recording::{identity_text, random_identity};

use super::{delete_older_recording, start};
use crate::performance::Options;
use crate::time_range::Range;

#[test]
fn a_default_launch_deletes_an_older_recording() {
    // Goal: the default launch clears the path its recorder needs for a new recording.
    // Method: launch on an older root with no --recording, and look for the root afterwards.
    let parent = older_recording_parent();
    let root = parent.join("flightdeck.recording");
    let options = Options::parse_arguments(&[]).unwrap();

    delete_older_recording(&root, &options);

    let exists = root.exists();
    std::fs::remove_dir_all(parent).unwrap();
    assert!(!exists);
}

#[test]
fn a_named_recording_stays_where_the_launch_named_it() {
    // Goal: a recording the user names is theirs to keep, so no launch deletes it.
    // Method: launch on an older root that --recording names, and look for it afterwards.
    let parent = older_recording_parent();
    let root = parent.join("flightdeck.recording");
    let arguments = ["--recording".to_owned(), root.display().to_string()];
    let options = Options::parse_arguments(&arguments).unwrap();

    delete_older_recording(&root, &options);

    let exists = root.exists();
    std::fs::remove_dir_all(parent).unwrap();
    assert!(exists);
}

/// A new directory holding an older version's recording, named `flightdeck.recording`.
fn older_recording_parent() -> std::path::PathBuf {
    let token = identity_text(random_identity().unwrap());
    let parent = std::env::temp_dir().join(format!("flightdeck-launch-{token}"));
    create_private_dir(&parent).unwrap();
    let root = parent.join("flightdeck.recording");
    create_private_dir(&root).unwrap();
    atomic_write(&root, "identity", b"00000000000000010000000000000002\n").unwrap();
    parent
}

#[test]
fn opens_the_window_on_the_page_the_launch_named() {
    // Goal: a page named with --page reaches the window, so it opens over the page kept.
    // Method: read the start of a launch that names a page and of one that names none.
    let named = Options::parse_arguments(&["--page".to_owned(), "cpu".to_owned()]).unwrap();
    let unnamed = Options::parse_arguments(&[]).unwrap();

    assert_eq!(Some("cpu".to_owned()), start(&named).named_page);
    assert_eq!(None, start(&unnamed).named_page);
}

#[test]
fn opens_the_window_at_the_range_the_launch_named() {
    // Goal: a range named with --minutes reaches the window, so it opens over the range kept.
    // Method: read the start of launches that name the hour, the default history and none.
    let hour = Options::parse_arguments(&["--minutes".to_owned(), "60".to_owned()]).unwrap();
    let history = Options::parse_arguments(&["--minutes".to_owned(), "15".to_owned()]).unwrap();
    let unnamed = Options::parse_arguments(&[]).unwrap();

    assert_eq!(Some(Range::Last(3600)), start(&hour).named_range);
    assert_eq!(Some(Range::History), start(&history).named_range);
    assert_eq!(None, start(&unnamed).named_range);
}
