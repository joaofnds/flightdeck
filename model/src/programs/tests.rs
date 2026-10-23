use super::{AppKey, Capacity, Program, Programs};

#[test]
fn ranks_each_app_by_its_key_as_apps_are_added() {
    // Goal: an App added between two others takes its place in key order and moves the later
    // one down, whatever order its entries come in.
    // Method: index git as entry 3, Brave as entry 0 and then Safari as entry 1, whose key
    // sorts between Brave's and git's.
    let mut programs = Programs::new(Capacity {
        entries: 4,
        program_bytes: 1024,
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
fn groups_entries_by_their_key_bytes_as_they_are() {
    // Goal: two entries of one program are one App, and two names that differ in their bytes
    // are two Apps, even where the one is not UTF-8 and the other spells its byte out.
    // Method: index claude twice, a name with the byte 0xff and the name spelling it as text.
    let mut programs = Programs::new(Capacity {
        entries: 4,
        program_bytes: 1024,
    });
    let names: [&[u8]; 4] = [b"claude", b"claude", b"bad\xff", b"bad\\xff"];
    for (entry, name) in (0..).zip(names) {
        programs.add(
            entry,
            Program {
                executable_name: name,
                app_path: None,
            },
        );
    }

    let apps = [0, 1, 2, 3].map(|entry| programs.app(entry));

    assert_eq!(apps[0], apps[1]);
    assert_ne!(apps[1], apps[2]);
    assert_ne!(apps[2], apps[3]);
    assert_eq!(programs.key(apps[2]), AppKey::Executable(b"bad\xff"));
}

#[test]
fn knows_only_the_entries_it_met() {
    // Goal: the ring adapter can tell an entry the index has met from one it must read first.
    // Method: index entry 2 alone and ask about it and the entries beside it.
    let mut programs = Programs::new(Capacity {
        entries: 4,
        program_bytes: 1024,
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

#[test]
fn numbers_each_program_of_a_source_without_a_dictionary_once() {
    // Goal: a source that names each process's program by its bytes gets one entry for each
    // program, as the run's dictionary gives one, so an exec between two programs of one bundle
    // changes the process's entry while both entries are one App.
    // Method: intern git, two programs of Safari's bundle and Safari again, and read each
    // entry's App and executable name.
    let mut programs = Programs::new(Capacity {
        entries: 4,
        program_bytes: 1024,
    });
    let safari = Some(&b"/Applications/Safari.app"[..]);
    let interned = [
        Program {
            executable_name: b"git",
            app_path: None,
        },
        Program {
            executable_name: b"Safari",
            app_path: safari,
        },
        Program {
            executable_name: b"com.apple.WebKit.WebContent",
            app_path: safari,
        },
        Program {
            executable_name: b"Safari",
            app_path: safari,
        },
    ]
    .map(|program| programs.intern(program));

    let apps = interned.map(|entry| entry.map(|entry| programs.app(entry)));
    let names = [1, 2].map(|entry| programs.program(entry).executable_name);

    assert_eq!(interned, [Some(0), Some(1), Some(2), Some(1)]);
    assert_ne!(apps[0], apps[1]);
    assert_eq!(apps[1], apps[2]);
    assert_eq!(names, [&b"Safari"[..], b"com.apple.WebKit.WebContent"]);
}

mod when_the_index_is_full {
    use super::{Capacity, Program, Programs};

    #[test]
    fn interns_no_program_past_its_entries() {
        // Goal: an index sized for one program refuses a second rather than growing.
        // Method: intern git into an index of one entry, then Safari, then git again.
        let mut programs = Programs::new(Capacity {
            entries: 1,
            program_bytes: 1024,
        });
        let git = Program {
            executable_name: b"git",
            app_path: None,
        };
        let safari = Program {
            executable_name: b"Safari",
            app_path: None,
        };

        let interned = [git, safari, git].map(|program| programs.intern(program));

        assert_eq!(interned, [Some(0), None, Some(0)]);
    }

    #[test]
    fn interns_no_program_past_its_bytes() {
        // Goal: an index whose program bytes are spent refuses a new program.
        // Method: intern git into an index of 4 program bytes, then Safari, whose name is 6
        // bytes.
        let mut programs = Programs::new(Capacity {
            entries: 4,
            program_bytes: 4,
        });

        let interned = [b"git".as_slice(), b"Safari"].map(|name| {
            programs.intern(Program {
                executable_name: name,
                app_path: None,
            })
        });

        assert_eq!(interned, [Some(0), None]);
    }
}
