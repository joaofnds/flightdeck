use collector::ring::IdentityPublication;

#[test]
fn rehydrates_adjacent_event_only_app_identity() {
    // Goal: selecting an evicted raw sample preserves the adjacent captured App identity.
    // Method: retain both ring publications and scalar points, then discard raw cached rows.
    let mut fixture = identity_fixture::Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish(&mut fixture, 2);
    session.poll().unwrap();
    session.follow_latest().unwrap();
    let kind = KeyKind::Bundle;
    let key = b"/Applications/B.app";
    let expected = session.app_detail(kind, key, 0).unwrap();
    session.observations.clear();
    session.cache_bytes = 0;
    let cursor = session.sources[0].reader.next_publication();
    let event_cursor = session.sources[0].reader.next_event();
    let losses = session.catalog()["reader_losses"].clone();

    session.select(1002, 0).unwrap();
    let detail = session.app_detail(kind, key, 0).unwrap();

    assert_eq!(session.apps_page(0).unwrap()["process_count"], 1);
    assert_eq!(detail, expected);
    assert_eq!(detail["app"]["members"], 1);
    assert_eq!(detail["app"]["live_members"], 0);
    assert_eq!(detail["app"]["resident_bytes"]["known"], "0");
    assert_eq!(detail["app"]["cpu_percent_one_core"], Value::Null);
    assert_eq!(detail["app"]["energy_joules"], Value::Null);
    assert_eq!(
        detail["processes"][0]["resident_bytes"]["known"],
        "104857600"
    );
    assert_eq!(session.sources[0].reader.next_publication(), cursor);
    assert_eq!(session.sources[0].reader.next_event(), event_cursor);
    assert_eq!(session.catalog()["reader_losses"], losses);
    assert!(!session.poll().unwrap());
}

fn publish(fixture: &mut identity_fixture::Fixture, publication: u64) {
    use collector::ring::{
        IdentityUnavailable, ProcessCreation, ProcessIdentity, ProgramBytes, UsageAvailability,
    };
    let creation = ProcessCreation {
        unique_id: 700,
        pid_version: 2,
    };
    let mut program = None;
    fixture
        .writer
        .intern_program(
            &ProgramBytes {
                executable_name: b"worker",
                app_path: Some(b"/Applications/B.app"),
            },
            &mut program,
        )
        .unwrap();
    let event = if publication == 1 {
        collector::tracker::Event::Exec { pid: 7 }
    } else {
        collector::tracker::Event::Exit {
            pid: 7,
            last: Ok(publish_usage()),
            noticed: collector::tracker::Noticed::OnExit,
        }
    };
    let identity = IdentityObservation {
        pid: 7,
        usage: if publication == 1 {
            UsageAvailability::NotApplicable
        } else {
            UsageAvailability::Measured
        },
        identity: if publication == 1 {
            ProcessIdentity::Measured {
                creation,
                program: program.unwrap(),
            }
        } else {
            ProcessIdentity::Unavailable {
                creation: Some(creation),
                reason: IdentityUnavailable::Unsupported,
            }
        },
    };
    publish_observation(fixture, publication, &event, &identity);
}

fn publish_observation(
    fixture: &mut identity_fixture::Fixture,
    publication: u64,
    event: &collector::tracker::Event,
    identity: &IdentityObservation,
) {
    fixture
        .writer
        .publish_with_identity(
            &Readings {
                run_tick: publication,
                cpus: &[collector::machine::CPUTicks {
                    user: 1,
                    system: 0,
                    nice: 0,
                    idle: 1,
                }],
                memory: &collector::machine::Memory::default(),
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: std::slice::from_ref(event),
                dropped: Dropped::default(),
            },
            Some(&ObservationTimes {
                wall_ns: (1000 + publication) * 1_000_000_000,
                continuous_ns: publication * 1_000_000_000,
                awake_ns: publication * 1_000_000_000,
            }),
            &IdentityPublication {
                processes: &[],
                events: std::slice::from_ref(identity),
            },
        )
        .unwrap();
}

fn publish_usage() -> collector::processes::Usage {
    collector::processes::Usage {
        awake_ns_at_start: 0,
        user_ns: 400_000_000,
        system_ns: 0,
        energy_nj: 2_000_000_000,
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: 0,
        interrupt_wakeups: 0,
        pageins: 0,
        resident_bytes: 104_857_600,
        footprint_bytes: 110_000_000,
        disk_read_bytes: 0,
        disk_written_bytes: 0,
    }
}
