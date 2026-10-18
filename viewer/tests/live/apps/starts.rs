use super::*;

pub(super) struct StartedExit {
    start_creation: ProcessCreation,
    exit_creation: ProcessCreation,
    unavailable: Option<IdentityUnavailable>,
    order: &'static [Kind],
    dropped: Dropped,
}

#[derive(Clone, Copy)]
enum Kind {
    Start,
    Exec,
    Exit,
}

impl Default for StartedExit {
    fn default() -> Self {
        let creation = ProcessCreation {
            unique_id: 700,
            pid_version: 1,
        };
        Self {
            start_creation: creation,
            exit_creation: creation,
            unavailable: None,
            order: &[Kind::Start, Kind::Exit],
            dropped: Dropped::default(),
        }
    }
}

impl StartedExit {
    pub(super) fn publish(&self, fixture: &mut Fixture) {
        let mut start = ExitScenario::publish_process_identity(fixture, 0, &self.start_creation);
        start.usage = UsageAvailability::NotApplicable;
        if let Some(reason) = self.unavailable {
            start.identity = ProcessIdentity::Unavailable {
                creation: Some(self.start_creation),
                reason,
            };
        }
        let events: Vec<_> = self.order.iter().copied().map(Self::event).collect();
        let identities: Vec<_> = self
            .order
            .iter()
            .map(|kind| self.identity(*kind, &start))
            .collect();
        let memory = Memory::default();
        let mut readings = readings(&[], &memory, 2);
        readings.events = &events;
        readings.dropped = self.dropped;
        fixture
            .writer
            .publish_with_identity(
                &readings,
                Some(&ObservationTimes {
                    wall_ns: 1_002_000_000_000,
                    continuous_ns: 2_000_000_000,
                    awake_ns: 2_000_000_000,
                }),
                &IdentityPublication {
                    processes: &[],
                    events: &identities,
                },
            )
            .unwrap();
    }

    fn event(kind: Kind) -> collector::tracker::Event {
        match kind {
            Kind::Start => collector::tracker::Event::Start {
                pid: 7,
                coalition: None,
            },
            Kind::Exec => collector::tracker::Event::Exec { pid: 7 },
            Kind::Exit => collector::tracker::Event::Exit {
                pid: 7,
                last: Ok(Usage {
                    awake_ns_at_start: 1_100_000_000,
                    ..process(0, 2).usage
                }),
                noticed: collector::tracker::Noticed::OnExit,
            },
        }
    }

    fn identity(&self, kind: Kind, start: &IdentityObservation) -> IdentityObservation {
        match kind {
            Kind::Start => *start,
            Kind::Exec => IdentityObservation {
                pid: 7,
                usage: UsageAvailability::NotApplicable,
                identity: ProcessIdentity::Unavailable {
                    creation: None,
                    reason: IdentityUnavailable::Unsupported,
                },
            },
            Kind::Exit => IdentityObservation {
                pid: 7,
                usage: UsageAvailability::Measured,
                identity: ProcessIdentity::Unavailable {
                    creation: Some(self.exit_creation),
                    reason: IdentityUnavailable::Unsupported,
                },
            },
        }
    }
}

macro_rules! started_exit {
    ($name:ident, $scenario:expr, $processes:expr, $apps:expr) => {
        #[test]
        fn $name() {
            // Goal: only an ordered, unambiguous creation/program match attributes an exit.
            // Method: vary one item of the captured Start/Exit sequence in a real v4 ring.
            let mut fixture = Fixture::new();
            publish_empty(&mut fixture);
            let mut session = LiveSession::open(&fixture.root).unwrap();
            $scenario.publish(&mut fixture);

            session.poll().unwrap();
            session.follow_latest().unwrap();
            let page = session.apps_page(0).unwrap();

            assert_eq!(page["process_count"], $processes);
            assert_eq!(page["app_count"], $apps);
            assert_eq!(page["membership_complete"], false);
        }
    };
}

started_exit!(
    refuses_to_join_a_reused_pid_to_an_earlier_start,
    StartedExit {
        exit_creation: ProcessCreation {
            unique_id: 701,
            pid_version: 1
        },
        ..StartedExit::default()
    },
    0,
    0
);
started_exit!(
    refuses_to_inherit_a_start_program_across_exec,
    StartedExit {
        exit_creation: ProcessCreation {
            unique_id: 700,
            pid_version: 2
        },
        ..StartedExit::default()
    },
    0,
    0
);
started_exit!(
    retains_verified_usage_when_the_start_program_is_unavailable,
    StartedExit {
        unavailable: Some(IdentityUnavailable::Refused),
        ..StartedExit::default()
    },
    1,
    0
);
started_exit!(
    refuses_ambiguous_duplicate_starts,
    StartedExit {
        order: &[Kind::Start, Kind::Start, Kind::Exit],
        ..StartedExit::default()
    },
    0,
    0
);
started_exit!(
    refuses_a_start_discovered_after_its_exit,
    StartedExit {
        order: &[Kind::Exit, Kind::Start],
        ..StartedExit::default()
    },
    0,
    0
);
started_exit!(
    refuses_an_intervening_exec_even_when_version_metadata_matches,
    StartedExit {
        order: &[Kind::Start, Kind::Exec, Kind::Exit],
        ..StartedExit::default()
    },
    0,
    0
);
started_exit!(
    accepts_a_start_discovered_after_an_exec_notice,
    StartedExit {
        order: &[Kind::Exec, Kind::Start, Kind::Exit],
        ..StartedExit::default()
    },
    1,
    1
);
started_exit!(
    keeps_an_exit_when_a_later_exec_notice_reuses_its_pid,
    StartedExit {
        order: &[Kind::Start, Kind::Exit, Kind::Exec],
        ..StartedExit::default()
    },
    1,
    1
);
started_exit!(
    keeps_individually_verified_start_and_exit_despite_unrelated_loss,
    StartedExit {
        dropped: Dropped {
            starts: 1,
            ..Dropped::default()
        },
        ..StartedExit::default()
    },
    1,
    1
);

#[test]
fn retains_a_verified_event_only_app_without_a_previous_publication() {
    // Goal: an event-only App needs no earlier scheduled row to retain its captured identity.
    // Method: open the live session on the publication containing its Start and Exit.
    let mut fixture = Fixture::new();
    StartedExit::default().publish(&mut fixture);

    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();
    let page = session.apps_page(0).unwrap();

    assert_eq!(page["process_count"], 1);
    assert_eq!(page["apps"][0]["display_name"], "A.app");
    assert_eq!(page["apps"][0]["live_members"], 0);
    assert_eq!(
        page["apps"][0]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
}

#[test]
fn retains_event_only_program_bytes_after_the_source_buffers_are_reused() {
    // Goal: the cached event-only App owns its captured program bytes independently.
    // Method: reuse source tick/identity buffers for another publication, then select the exit.
    let mut fixture = Fixture::new();
    publish_empty(&mut fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    StartedExit::default().publish(&mut fixture);
    session.poll().unwrap();
    session.follow_latest().unwrap();
    publish(&mut fixture, 3);
    session.poll().unwrap();
    session.follow_latest().unwrap();

    session.select(1002, 0).unwrap();
    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
            0,
        )
        .unwrap();

    assert_eq!(detail["selected"]["stamp"]["publication"], "2");
    assert_eq!(detail["app"]["members"], 1);
    assert_eq!(detail["app"]["live_members"], 0);
    assert_eq!(detail["processes"][0]["executable_name"]["display"], "main");
    assert_eq!(detail["processes"][0]["state"], "exited_verified");
}

#[test]
fn associates_each_short_lived_app_beside_scheduled_members() {
    // Goal: each event-only process inherits its own Start program after sorting beside live rows.
    // Method: reverse two distinct App Starts, retain scheduled App A, and read literal details.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish_mixed_starts(&mut fixture);

    session.poll().unwrap();
    session.follow_latest().unwrap();
    let page = session.apps_page(0).unwrap();
    let first = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/B.app",
            0,
        )
        .unwrap();
    let second = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/C.app",
            0,
        )
        .unwrap();

    assert_eq!(page["app_count"], 3);
    assert_eq!(page["process_count"], 4);
    assert_eq!(first["app"]["key"]["display"], "/Applications/B.app");
    assert_eq!(first["app"]["live_members"], 0);
    assert_eq!(first["app"]["resident_bytes"]["known"], "0");
    assert_eq!(first["processes"][0]["pid"], 9);
    assert_eq!(first["processes"][0]["creation"]["unique_id"], "900");
    assert_eq!(
        first["processes"][0]["executable_name"]["display"],
        "helper_b"
    );
    assert_eq!(first["processes"][0]["state"], "exited_verified");
    assert_eq!(
        first["processes"][0]["resident_bytes"]["known"],
        "104857600"
    );
    assert_eq!(
        first["processes"][0]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
    assert_eq!(second["app"]["key"]["display"], "/Applications/C.app");
    assert_eq!(second["app"]["live_members"], 0);
    assert_eq!(second["app"]["resident_bytes"]["known"], "0");
    assert_eq!(second["processes"][0]["pid"], 10);
    assert_eq!(second["processes"][0]["creation"]["unique_id"], "901");
    assert_eq!(
        second["processes"][0]["executable_name"]["display"],
        "helper_c"
    );
    assert_eq!(second["processes"][0]["state"], "exited_verified");
    assert_eq!(
        second["processes"][0]["resident_bytes"]["known"],
        "52428800"
    );
    assert_eq!(
        second["processes"][0]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
}

fn publish_mixed_starts(fixture: &mut Fixture) {
    let mut identities = Vec::with_capacity(2);
    for index in 0..2 {
        identities.push(ExitScenario::publish_process_identity(
            fixture,
            index,
            &ProcessCreation {
                unique_id: u64::try_from(index + 700).unwrap(),
                pid_version: 1,
            },
        ));
    }
    let mut events = Vec::with_capacity(4);
    let mut event_identities = Vec::with_capacity(4);
    for (kind, pid) in [
        (Kind::Start, 10),
        (Kind::Start, 9),
        (Kind::Exit, 9),
        (Kind::Exit, 10),
    ] {
        events.push(mixed_start_event(kind, pid));
        event_identities.push(mixed_start_identity(fixture, kind, pid));
    }
    let processes = [process(0, 2), process(1, 2)];
    let memory = Memory::default();
    let mut readings = readings(&processes, &memory, 2);
    readings.events = &events;
    fixture
        .writer
        .publish_with_identity(
            &readings,
            Some(&ObservationTimes {
                wall_ns: 1_002_000_000_000,
                continuous_ns: 2_000_000_000,
                awake_ns: 2_000_000_000,
            }),
            &IdentityPublication {
                processes: &identities,
                events: &event_identities,
            },
        )
        .unwrap();
}

fn mixed_start_event(kind: Kind, pid: i32) -> collector::tracker::Event {
    match kind {
        Kind::Start => collector::tracker::Event::Start {
            pid,
            coalition: None,
        },
        Kind::Exit => collector::tracker::Event::Exit {
            pid,
            last: Ok(process(usize::try_from(pid - 9).unwrap(), 2).usage),
            noticed: collector::tracker::Noticed::OnExit,
        },
        Kind::Exec => unreachable!("mixed fixture has no exec"),
    }
}

fn mixed_start_identity(fixture: &mut Fixture, kind: Kind, pid: i32) -> IdentityObservation {
    let index = usize::try_from(pid - 9).unwrap();
    let creation = ProcessCreation {
        unique_id: u64::try_from(index + 900).unwrap(),
        pid_version: 1,
    };
    let mut program = None;
    fixture
        .writer
        .intern_program(
            &collector::ring::ProgramBytes {
                executable_name: [b"helper_b".as_slice(), b"helper_c".as_slice()][index],
                app_path: Some(
                    [
                        b"/Applications/B.app".as_slice(),
                        b"/Applications/C.app".as_slice(),
                    ][index],
                ),
            },
            &mut program,
        )
        .unwrap();

    match kind {
        Kind::Start => IdentityObservation {
            pid,
            usage: UsageAvailability::NotApplicable,
            identity: ProcessIdentity::Measured {
                creation,
                program: program.unwrap(),
            },
        },
        Kind::Exit => IdentityObservation {
            pid,
            usage: UsageAvailability::Measured,
            identity: ProcessIdentity::Unavailable {
                creation: Some(creation),
                reason: IdentityUnavailable::Unsupported,
            },
        },
        Kind::Exec => unreachable!("mixed fixture has no exec"),
    }
}
