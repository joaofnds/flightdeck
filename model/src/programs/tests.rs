use super::{Capacity, Program, Programs};

#[test]
fn ranks_each_app_by_its_key_as_apps_are_added() {
    // Goal: an App added between two others takes its place in key order and moves the later
    // one down.
    // Method: index git, Brave and then Safari, whose key sorts between Brave's and git's.
    let mut programs = Programs::new(Capacity {
        entries: 4,
        key_bytes: 1024,
    });
    let entries = [
        Program {
            executable_name: b"git",
            app_path: None,
        },
        Program {
            executable_name: b"Brave Browser",
            app_path: Some(b"/Applications/Brave Browser.app"),
        },
        Program {
            executable_name: b"Safari",
            app_path: Some(b"/Applications/Safari.app"),
        },
    ]
    .map(|program| programs.add(program));

    let ranks = entries.map(|entry| programs.rank(programs.app(entry)));

    assert_eq!(ranks, [2, 0, 1]);
}
