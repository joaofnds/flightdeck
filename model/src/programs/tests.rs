use super::{Capacity, Program, Programs};

#[test]
fn ranks_each_app_by_its_key_as_apps_are_added() {
    // Goal: an App added between two others takes its place in key order and moves the later
    // one down, whatever order its entries come in.
    // Method: index git as entry 3, Brave as entry 0 and then Safari as entry 1, whose key
    // sorts between Brave's and git's.
    let mut programs = Programs::new(Capacity {
        entries: 4,
        key_bytes: 1024,
    });
    let programs_of = [
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
    ];
    let entries = [3, 0, 1];
    for (entry, program) in entries.into_iter().zip(programs_of) {
        programs.add(entry, program);
    }

    let ranks = entries.map(|entry| programs.rank(programs.app(entry)));

    assert_eq!(ranks, [2, 0, 1]);
}

#[test]
fn knows_only_the_entries_it_met() {
    // Goal: the ring adapter can tell an entry the index has met from one it must read first.
    // Method: index entry 2 alone and ask about it and the entries beside it.
    let mut programs = Programs::new(Capacity {
        entries: 4,
        key_bytes: 1024,
    });

    programs.add(
        2,
        Program {
            executable_name: b"git",
            app_path: None,
        },
    );

    assert_eq!(
        [0, 1, 2, 3].map(|entry| programs.knows(entry)),
        [false, false, true, false]
    );
}
