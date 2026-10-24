use super::start;
use crate::performance::Options;
use crate::time_range::Range;

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
