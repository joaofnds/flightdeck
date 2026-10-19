use super::*;

const KEYS: [AppKey<'static>; 4] = [
    AppKey::Bundle(b"/Applications/Brave Browser.app"),
    AppKey::Bundle(b"/Applications/Ghostty.app"),
    AppKey::Executable(b"claude"),
    AppKey::Executable(b"kernel_task"),
];

fn search(next: usize, key: &AppKey<'_>) -> Result<usize, usize> {
    // The indexes run backwards so a position and the index it holds differ.
    let ordered = [3, 2, 1, 0];
    let keys = [KEYS[3], KEYS[2], KEYS[1], KEYS[0]];
    search_ordered(&ordered, next, key, |index| keys[usize::from(index)])
}

#[test]
fn search_ordered_finds_each_key_whatever_slot_it_is_told_to_try_first() {
    // Goal: the slot to try first only saves work and never changes what a search finds.
    // Method: search every key from every starting slot, past the end included.
    for (position, key) in KEYS.iter().enumerate() {
        for next in 0..=KEYS.len() {
            assert_eq!(search(next, key), Ok(position));
        }
    }
}

#[test]
fn search_ordered_returns_the_insertion_position_of_a_missing_key() {
    // Goal: a key not yet present reports where it belongs in key order.
    // Method: search keys before, between and after the present ones from each slot.
    let missing = [
        (AppKey::Bundle(b"/Applications/Arc.app"), 0),
        (AppKey::Bundle(b"/Applications/Zed.app"), 2),
        (AppKey::Executable(b"zsh"), 4),
    ];

    for (key, position) in missing {
        for next in 0..=KEYS.len() {
            assert_eq!(search(next, &key), Err(position));
        }
    }
}
