use super::start;
use crate::performance::Options;

#[test]
fn opens_the_window_on_the_page_the_launch_named() {
    // Goal: a page named with --page reaches the window, so it opens over the page kept.
    // Method: read the start of a launch that names a page and of one that names none.
    let named = Options::parse_arguments(&["--page".to_owned(), "cpu".to_owned()]).unwrap();
    let unnamed = Options::parse_arguments(&[]).unwrap();

    assert_eq!(Some("cpu".to_owned()), start(&named).named_page);
    assert_eq!(None, start(&unnamed).named_page);
}
