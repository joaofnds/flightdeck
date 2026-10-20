use super::*;

#[test]
fn a_ranking_error_retains_only_a_bounded_notice() {
    // Goal: a large error from the viewer cannot enlarge the ranking failure the page keeps.
    // Method: hand the selected observation's ranks a 4096-character error.
    let result = from_selected(Err("失".repeat(4096)));

    let Rankings::Unavailable(error) = result else {
        panic!("{result:?}")
    };
    assert_eq!(error.chars().count(), 513);
    assert!(error.ends_with('…'));
}
