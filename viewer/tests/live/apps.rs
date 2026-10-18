use collector::machine::Memory;
use collector::processes::{Process, Task, Usage};
use collector::ring::{
    Dropped, IdentityObservation, IdentityPublication, IdentityUnavailable, ObservationTimes,
    ProcessCreation, ProcessIdentity, Readings, UsageAvailability,
};
use viewer::LiveSession;

use super::identity_fixture::Fixture;

#[test]
fn aggregates_live_apps_from_the_exact_selected_publication() {
    // Two real publications establish literal App deltas beside both captured process members.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish(&mut fixture, 2);
    session.poll().unwrap();
    let selected = session.follow_latest().unwrap();

    let page = session.apps_page(0).unwrap();

    assert_eq!(selected["selected"]["publication"], 2);
    assert_eq!(page["selected"]["stamp"]["publication"], "2");
    assert_eq!(page["app_count"], 1);
    assert_eq!(page["process_count"], 2);
    assert_eq!(page["elapsed_ns"], "1000000000");
    let app = &page["apps"][0];
    assert_eq!(app["display_name"], "A.app");
    assert_eq!(app["key"]["display"], "/Applications/A.app");
    assert_eq!(app["members"], 2);
    assert_eq!(app["cpu_percent_one_core"], 50.0);
    assert_eq!(app["energy_joules"], 3.0);
    assert_eq!(app["energy_watts"], 3.0);
    assert_eq!(app["resident_bytes"]["known"], "157286400");
    assert_eq!(app["membership_complete"], false);
    assert_eq!(app["cpu_ns"]["measured_members"], 2);
    assert_eq!(app["cpu_ns"]["unavailable_members"], 0);
}

#[test]
fn keeps_process_members_beside_their_exact_live_app_subtotals() {
    // Stable captured bytes select the current App without reusing a prior sample's index.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish(&mut fixture, 2);
    session.poll().unwrap();
    session.follow_latest().unwrap();

    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
            0,
        )
        .unwrap();

    assert_eq!(detail["selected"]["stamp"]["publication"], "2");
    assert_eq!(detail["app"]["cpu_percent_one_core"], 50.0);
    assert_eq!(detail["processes"][0]["pid"], 7);
    assert_eq!(detail["processes"][0]["cpu_percent_one_core"], 20.0);
    assert_eq!(detail["processes"][1]["pid"], 8);
    assert_eq!(detail["processes"][1]["cpu_percent_one_core"], 30.0);
    assert_eq!(
        detail["processes"][1]["executable_name"]["display"],
        "helper"
    );
}

#[test]
fn includes_verified_final_usage_beside_the_exited_live_app_member() {
    // Goal: a verified exit contributes its final interval usage but no currently resident memory.
    // Method: replace one scheduled member with its identity-bearing Exit in a real v4 ring.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish_exit(&mut fixture);
    session.poll().unwrap();
    session.follow_latest().unwrap();

    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
            0,
        )
        .unwrap();
    let history = session
        .app_history(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
        )
        .unwrap();

    assert_eq!(detail["app"]["cpu_percent_one_core"], 50.0);
    assert_eq!(detail["app"]["energy_joules"], 3.0);
    assert_eq!(detail["app"]["energy_watts"], 3.0);
    assert_eq!(detail["app"]["members"], 2);
    assert_eq!(detail["app"]["live_members"], 1);
    assert_eq!(detail["app"]["resident_bytes"]["known"], "52428800");
    assert_eq!(detail["processes"][0]["pid"], 7);
    assert_eq!(detail["processes"][0]["state"], "exited_verified");
    assert_eq!(
        detail["processes"][0]["resident_bytes"]["known"],
        "104857600"
    );
    assert_eq!(detail["processes"][0]["cpu_percent_one_core"], 20.0);
    assert_eq!(history["points"][1]["cpu_percent_one_core"], 50.0);
    assert_eq!(history["points"][1]["resident_bytes"]["known"], "52428800");
}

fn publish_exit(fixture: &mut Fixture) {
    ExitScenario::default().publish(fixture);
}

#[test]
fn retains_an_app_that_starts_and_exits_between_scheduled_samples() {
    // Goal: captured Start identity retains an event-only exited App without inventing a rate.
    // Method: publish no scheduled members, then a measured Start and creation-verified Exit.
    let mut fixture = Fixture::new();
    publish_empty(&mut fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish_started_exit(&mut fixture);

    session.poll().unwrap();
    session.follow_latest().unwrap();
    let page = session.apps_page(0).unwrap();

    assert_eq!(page["process_count"], 1);
    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
            0,
        )
        .unwrap();
    assert_eq!(detail["app"]["members"], 1);
    assert_eq!(detail["app"]["live_members"], 0);
    assert_eq!(detail["app"]["resident_bytes"]["known"], "0");
    assert_eq!(
        detail["app"]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
    assert_eq!(detail["app"]["energy_joules"], serde_json::Value::Null);
    assert_eq!(detail["processes"][0]["state"], "exited_verified");
    assert_eq!(
        detail["processes"][0]["resident_bytes"]["known"],
        "104857600"
    );
    assert_eq!(
        detail["processes"][0]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
}

fn publish_empty(fixture: &mut Fixture) {
    fixture
        .writer
        .publish_with_identity(
            &readings(&[], &Memory::default(), 1),
            Some(&ObservationTimes {
                wall_ns: 1_001_000_000_000,
                continuous_ns: 1_000_000_000,
                awake_ns: 1_000_000_000,
            }),
            &IdentityPublication {
                processes: &[],
                events: &[],
            },
        )
        .unwrap();
}

fn publish_started_exit(fixture: &mut Fixture) {
    starts::StartedExit::default().publish(fixture);
}

#[path = "apps/starts.rs"]
mod starts;

#[path = "apps/execs.rs"]
mod execs;

macro_rules! unassociated_exit {
    ($name:ident, $scenario:expr) => {
        #[test]
        fn $name() {
            // Goal: unavailable or ambiguous exit evidence cannot add an attributed rate.
            // Method: change one source association in a real identity-ring publication.
            let mut fixture = Fixture::new();
            publish(&mut fixture, 1);
            let mut session = LiveSession::open(&fixture.root).unwrap();
            $scenario.publish(&mut fixture);
            session.poll().unwrap();
            session.follow_latest().unwrap();

            let page = session.apps_page(0).unwrap();

            assert_eq!(page["process_count"], 1);
            assert_eq!(page["apps"][0]["members"], 1);
            assert_eq!(page["apps"][0]["cpu_percent_one_core"], 30.0);
            assert_eq!(page["membership_complete"], false);
        }
    };
}

unassociated_exit!(
    leaves_exit_usage_unattributed_without_captured_creation,
    ExitScenario {
        creation: None,
        ..ExitScenario::default()
    }
);
unassociated_exit!(
    does_not_join_an_exit_from_another_creation_of_the_same_pid,
    ExitScenario {
        creation: Some(ProcessCreation {
            unique_id: 900,
            pid_version: 1
        }),
        ..ExitScenario::default()
    }
);
unassociated_exit!(
    does_not_count_duplicate_exit_evidence_twice,
    ExitScenario {
        duplicate: true,
        ..ExitScenario::default()
    }
);
#[test]
fn retains_known_exit_subtotals_beside_dropped_event_coverage() {
    // Goal: an unrelated loss cannot erase a separately verified creation's final counters.
    // Method: retain the matching final endpoint beside each kind of dropped event.
    for dropped in [
        Dropped {
            starts: 1,
            ..Dropped::default()
        },
        Dropped {
            execs: 1,
            ..Dropped::default()
        },
        Dropped {
            exits: 1,
            ..Dropped::default()
        },
    ] {
        let mut fixture = Fixture::new();
        publish(&mut fixture, 1);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        ExitScenario {
            dropped,
            ..ExitScenario::default()
        }
        .publish(&mut fixture);
        session.poll().unwrap();
        session.follow_latest().unwrap();

        let page = session.apps_page(0).unwrap();

        assert_eq!(page["process_count"], 2);
        assert_eq!(page["apps"][0]["cpu_percent_one_core"], 50.0);
        assert_eq!(page["apps"][0]["energy_joules"], 3.0);
        assert_eq!(page["membership_complete"], false);
    }
}

#[test]
fn retains_a_verified_exit_after_unrelated_event_ring_overwrite() {
    // Goal: loss of an unrelated retained event leaves known exit counters usable.
    // Method: overwrite the first Exec of a full 32-event ring after the matching final exit.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    ExitScenario {
        prefix_execs: 31,
        ..ExitScenario::default()
    }
    .publish(&mut fixture);
    ExitScenario {
        sample: 3,
        creation: None,
        ..ExitScenario::default()
    }
    .publish(&mut fixture);
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let page = session.apps_page(0).unwrap();

    assert_eq!(page["process_count"], 2);
    assert_eq!(page["apps"][0]["cpu_percent_one_core"], 50.0);
    assert_eq!(page["membership_complete"], false);
}

#[test]
fn retains_process_membership_without_assigning_an_exec_crossing_exit_to_the_old_app() {
    // Goal: a changed executable version cannot inherit the predecessor's App bytes.
    // Method: publish a verified creation with a different PID version at final usage.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    ExitScenario {
        creation: Some(ProcessCreation {
            unique_id: 700,
            pid_version: 2,
        }),
        ..ExitScenario::default()
    }
    .publish(&mut fixture);
    session.poll().unwrap();
    session.follow_latest().unwrap();

    let page = session.apps_page(0).unwrap();

    assert_eq!(page["process_count"], 2);
    assert_eq!(page["ungrouped_members"], 1);
    assert_eq!(page["apps"][0]["members"], 1);
    assert_eq!(page["apps"][0]["cpu_percent_one_core"], 30.0);
}

#[test]
fn replaces_a_scheduled_member_with_its_verified_exit_once() {
    // Goal: scheduled and final observations of one creation form one exited process endpoint.
    // Method: publish the same creation in both scheduled rows and final event metadata.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    ExitScenario {
        scheduled: Some(ProcessCreation {
            unique_id: 700,
            pid_version: 1,
        }),
        ..ExitScenario::default()
    }
    .publish(&mut fixture);
    session.poll().unwrap();
    session.follow_latest().unwrap();

    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
            0,
        )
        .unwrap();

    assert_eq!(detail["app"]["members"], 2);
    assert_eq!(detail["app"]["live_members"], 1);
    assert_eq!(detail["app"]["cpu_percent_one_core"], 50.0);
    assert_eq!(detail["processes"].as_array().unwrap().len(), 2);
    assert_eq!(detail["processes"][0]["state"], "exited_verified");
}

#[test]
fn refuses_final_counters_that_regress_after_the_scheduled_sample() {
    // Goal: a stale final read cannot override a newer scheduled endpoint.
    // Method: put final CPU/energy between the predecessor and the same-tick scheduled sample.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    ExitScenario {
        scheduled: Some(ProcessCreation {
            unique_id: 700,
            pid_version: 1,
        }),
        usage: Ok(Usage {
            user_ns: 300_000_000,
            energy_nj: 1_500_000_000,
            ..process(0, 2).usage
        }),
        ..ExitScenario::default()
    }
    .publish(&mut fixture);
    session.poll().unwrap();
    session.follow_latest().unwrap();

    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
            0,
        )
        .unwrap();

    assert_eq!(detail["app"]["cpu_percent_one_core"], 30.0);
    assert_eq!(detail["app"]["resident_bytes"]["known"], "52428800");
    assert_eq!(detail["processes"][0]["state"], "exited_unverified");
    assert_eq!(
        detail["processes"][0]["cpu_ns"]["unavailable"],
        "exit_unverified"
    );
    assert_eq!(
        detail["processes"][0]["energy_nj"]["unavailable"],
        "exit_unverified"
    );
    assert_eq!(
        detail["processes"][0]["resident_bytes"]["known"],
        "104857600"
    );
    assert_eq!(
        detail["processes"][0]["footprint_bytes"]["known"],
        "110000000"
    );
}

#[test]
fn retains_a_reused_pid_as_distinct_from_its_predecessor_exit() {
    // Goal: a new process using an old PID cannot erase the old creation's final contribution.
    // Method: keep both creation identities at the same PID in one publication interval.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    ExitScenario {
        scheduled: Some(ProcessCreation {
            unique_id: 900,
            pid_version: 1,
        }),
        ..ExitScenario::default()
    }
    .publish(&mut fixture);
    session.poll().unwrap();
    session.follow_latest().unwrap();

    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
            0,
        )
        .unwrap();

    assert_eq!(detail["app"]["members"], 3);
    assert_eq!(detail["app"]["live_members"], 2);
    assert_eq!(detail["app"]["cpu_percent_one_core"], 50.0);
    assert_eq!(detail["app"]["cpu_ns"]["unavailable_members"], 1);
    assert_eq!(detail["processes"][0]["creation"]["unique_id"], "700");
    assert_eq!(detail["processes"][0]["state"], "exited_verified");
    assert_eq!(detail["processes"][1]["creation"]["unique_id"], "900");
    assert_eq!(
        detail["processes"][1]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
}

#[test]
fn keeps_refused_final_usage_unavailable_beside_the_last_resident_gauge() {
    // Goal: a known exit with refused final usage is not a zero delta or present memory.
    // Method: retain verified creation while refusing its final numeric sample.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    ExitScenario {
        usage: Err(collector::processes::Unreadable::Refused),
        ..ExitScenario::default()
    }
    .publish(&mut fixture);
    session.poll().unwrap();
    session.follow_latest().unwrap();

    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
            0,
        )
        .unwrap();

    assert_eq!(detail["app"]["cpu_percent_one_core"], 30.0);
    assert_eq!(detail["app"]["cpu_ns"]["unavailable_members"], 1);
    assert_eq!(detail["app"]["resident_bytes"]["known"], "52428800");
    assert_eq!(detail["processes"][0]["state"], "exited_unverified");
    assert_eq!(
        detail["processes"][0]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
    assert_eq!(
        detail["processes"][0]["resident_bytes"]["known"],
        "104857600"
    );
}

#[test]
fn does_not_invent_a_birth_baseline_for_an_exit_without_a_predecessor() {
    // Goal: a captured exit alone cannot establish its App or a zero counter baseline.
    // Method: open a real source whose first publication already contains the exit.
    let mut fixture = Fixture::new();
    publish_exit(&mut fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();

    let page = session.apps_page(0).unwrap();

    assert_eq!(page["process_count"], 1);
    assert_eq!(
        page["apps"][0]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
}

#[test]
fn reconstructs_verified_exit_totals_after_real_raw_cache_eviction() {
    // Goal: selecting an evicted raw observation preserves its exit-aware App totals.
    // Method: evict two small observations with real 12000-member publications, then rehydrate.
    let mut fixture = Fixture::new_mode(true, 12_000, None);
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish_exit(&mut fixture);
    session.poll().unwrap();
    for second in 1003..=1020 {
        publish_pressure(&mut fixture, second);
        session.poll().unwrap();
    }
    let retained = session.catalog()["tail_ticks"].as_u64().unwrap();

    let selected = session.select(1002, 0).unwrap();
    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
            0,
        )
        .unwrap();

    assert!(retained < 20);
    assert_eq!(selected["selected"]["publication"], 2);
    assert_eq!(detail["app"]["cpu_percent_one_core"], 50.0);
    assert_eq!(detail["app"]["resident_bytes"]["known"], "52428800");
    assert_eq!(detail["processes"][0]["state"], "exited_verified");
}

fn publish_pressure(fixture: &mut Fixture, second: u64) {
    let mut program = None;
    fixture
        .writer
        .intern_program(
            &collector::ring::ProgramBytes {
                executable_name: b"helper",
                app_path: Some(b"/Applications/A.app"),
            },
            &mut program,
        )
        .unwrap();
    let identities: Vec<_> = (1..=12_000)
        .map(|pid| IdentityObservation {
            pid,
            usage: UsageAvailability::Refused,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: u64::try_from(pid + 10_000).unwrap(),
                    pid_version: 1,
                },
                program: program.unwrap(),
            },
        })
        .collect();
    let memory = Memory::default();
    let mut readings = readings(&[], &memory, second);
    readings.processes_refused = 12_000;

    fixture
        .writer
        .publish_with_identity(
            &readings,
            Some(&ObservationTimes {
                wall_ns: second * 1_000_000_000,
                continuous_ns: (second - 1000) * 1_000_000_000,
                awake_ns: (second - 1000) * 1_000_000_000,
            }),
            &IdentityPublication {
                processes: &identities,
                events: &[],
            },
        )
        .unwrap();
}

#[test]
fn leaves_an_exit_unattributed_across_a_missing_publication() {
    // Goal: a retained older creation cannot bridge a missed publication to a final sample.
    // Method: overwrite publication two before polling so the first new retained tick is its exit.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish(&mut fixture, 2);
    ExitScenario {
        sample: 3,
        ..ExitScenario::default()
    }
    .publish(&mut fixture);
    for sample in 4..=10 {
        publish(&mut fixture, sample);
    }
    session.poll().unwrap();

    session.select(1003, 0).unwrap();
    let page = session.apps_page(0).unwrap();

    assert_eq!(page["process_count"], 1);
    assert_eq!(
        page["apps"][0]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
    assert_eq!(page["membership_complete"], false);
}

struct ExitScenario {
    creation: Option<ProcessCreation>,
    usage: Result<Usage, collector::processes::Unreadable>,
    scheduled: Option<ProcessCreation>,
    duplicate: bool,
    prefix_execs: usize,
    dropped: Dropped,
    sample: u64,
}

impl Default for ExitScenario {
    fn default() -> Self {
        Self {
            creation: Some(ProcessCreation {
                unique_id: 700,
                pid_version: 1,
            }),
            usage: Ok(process(0, 2).usage),
            scheduled: None,
            duplicate: false,
            prefix_execs: 0,
            dropped: Dropped::default(),
            sample: 2,
        }
    }
}

impl ExitScenario {
    fn publish(&self, fixture: &mut Fixture) {
        let mut processes = Vec::with_capacity(2);
        let mut identities = Vec::with_capacity(2);
        if let Some(creation) = self.scheduled {
            processes.push(process(0, self.sample));
            identities.push(Self::publish_process_identity(fixture, 0, &creation));
        }
        processes.push(process(1, self.sample));
        identities.push(Self::publish_process_identity(
            fixture,
            1,
            &ProcessCreation {
                unique_id: 701,
                pid_version: 1,
            },
        ));
        let events = self.publish_events();
        let event_identities = self.publish_event_identities(&events);
        let memory = Memory::default();
        let mut readings = readings(&processes, &memory, self.sample);
        readings.events = &events;
        readings.dropped = self.dropped;

        fixture
            .writer
            .publish_with_identity(
                &readings,
                Some(&ObservationTimes {
                    wall_ns: (1000 + self.sample) * 1_000_000_000,
                    continuous_ns: self.sample * 1_000_000_000,
                    awake_ns: self.sample * 1_000_000_000,
                }),
                &IdentityPublication {
                    processes: &identities,
                    events: &event_identities,
                },
            )
            .unwrap();
    }

    fn publish_event_identities(
        &self,
        events: &[collector::tracker::Event],
    ) -> Vec<IdentityObservation> {
        let event_identity = IdentityObservation {
            pid: 7,
            usage: match self.usage {
                Ok(_) => UsageAvailability::Measured,
                Err(collector::processes::Unreadable::Refused) => UsageAvailability::Refused,
                Err(collector::processes::Unreadable::Gone) => UsageAvailability::Gone,
            },
            identity: ProcessIdentity::Unavailable {
                creation: self.creation,
                reason: IdentityUnavailable::Unsupported,
            },
        };
        events
            .iter()
            .map(|event| match event {
                collector::tracker::Event::Exit { .. } => event_identity,
                collector::tracker::Event::Exec { pid } => IdentityObservation {
                    pid: *pid,
                    usage: UsageAvailability::NotApplicable,
                    identity: ProcessIdentity::Unavailable {
                        creation: None,
                        reason: IdentityUnavailable::Unsupported,
                    },
                },
                collector::tracker::Event::Start { .. } => unreachable!(),
            })
            .collect()
    }

    fn publish_events(&self) -> Vec<collector::tracker::Event> {
        assert!(self.prefix_execs <= 31);
        let mut events = Vec::with_capacity(self.prefix_execs + 2);
        for _ in 0..self.prefix_execs {
            events.push(collector::tracker::Event::Exec { pid: 99 });
        }

        events.push(collector::tracker::Event::Exit {
            pid: 7,
            last: self.usage,
            noticed: collector::tracker::Noticed::OnExit,
        });
        if self.duplicate {
            events.push(collector::tracker::Event::Exit {
                pid: 7,
                last: self.usage,
                noticed: collector::tracker::Noticed::OnExit,
            });
        }

        events
    }

    fn publish_process_identity(
        fixture: &mut Fixture,
        index: usize,
        creation: &ProcessCreation,
    ) -> IdentityObservation {
        let mut program = None;
        fixture
            .writer
            .intern_program(
                &collector::ring::ProgramBytes {
                    executable_name: [b"main".as_slice(), b"helper".as_slice()][index],
                    app_path: Some(b"/Applications/A.app"),
                },
                &mut program,
            )
            .unwrap();

        IdentityObservation {
            pid: i32::try_from(index + 7).unwrap(),
            usage: UsageAvailability::Measured,
            identity: ProcessIdentity::Measured {
                creation: *creation,
                program: program.unwrap(),
            },
        }
    }
}

#[test]
fn retains_exact_app_history_while_following_the_fifteen_minute_window() {
    // The first observed App has no rate baseline; only its next actual interval has one.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish(&mut fixture, 2);
    session.poll().unwrap();
    session.follow_latest().unwrap();

    let history = session
        .app_history(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
        )
        .unwrap();

    assert_eq!(history["context_seconds"], 900);
    assert_eq!(history["selected"]["stamp"]["publication"], "2");
    let points = history["points"].as_array().unwrap();
    assert_eq!(points.len(), 2);
    assert_eq!(points[0]["members"], 2);
    assert_eq!(points[1]["members"], 2);
    assert_eq!(points[0]["cpu_percent_one_core"], serde_json::Value::Null);
    assert_eq!(points[1]["cpu_percent_one_core"], 50.0);
    assert_eq!(points[1]["resident_bytes"]["known"], "157286400");
    assert_eq!(points[1]["energy_watts"], 3.0);
    assert_eq!(history["coverage"]["retention_limited"], false);
}

#[test]
fn retains_fifteen_minutes_for_an_ordinary_live_app_population() {
    // A real identity ring keeps 640 distinct Apps throughout 901 observed publications.
    let mut fixture = AppPopulation::new();
    fixture.publish(1);
    let mut session = LiveSession::open(&fixture.root).unwrap();

    fixture.observe_through(&mut session, 901);
    let selection = session.follow_latest().unwrap();
    let history = session
        .app_history(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/History Capacity 0000.app",
        )
        .unwrap();

    let points = history["points"].as_array().unwrap();
    assert_eq!(points.len(), 900);
    assert_eq!(history["coverage"]["retention_limited"], false);
    assert_eq!(history["coverage"]["observations_retained"], 900);
    assert_eq!(selection["selected"]["publication"], 901);
    assert_eq!(history["selected"]["stamp"]["publication"], "901");
    assert_eq!(points[0]["observation"]["stamp"]["publication"], "2");
    assert_eq!(points[899]["observation"]["stamp"]["publication"], "901");
    assert_eq!(points[0]["resident_bytes"]["known"], "200");
    assert_eq!(points[899]["resident_bytes"]["known"], "90100");
    assert_eq!(points[0]["cpu_ns"]["known"], "1000000");
    assert_eq!(points[899]["cpu_ns"]["known"], "1000000");
    assert!(history["coverage"]["cache_bytes"].as_u64().unwrap() < 64 * 1024 * 1024);
    eprintln!("ordinary App history coverage: {}", history["coverage"]);
}

struct AppPopulation {
    root: std::path::PathBuf,
    writer: collector::ring::Writer,
    processes: Vec<Process>,
    identities: Vec<IdentityObservation>,
}

impl AppPopulation {
    fn new() -> Self {
        let token =
            collector::recording::identity_text(collector::recording::random_identity().unwrap())
                .to_string();
        let root = std::env::temp_dir().join(format!("flightdeck-app-history-{token}"));
        collector::recording::private::create_private_dir(&root).unwrap();
        let place = collector::ring::Place {
            lock_path: root.join("ring.lock"),
            name_prefix: format!("/fdah{}", std::process::id()),
        };
        let shape = collector::ring::Shape {
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 640,
            coalition_capacity: 1,
            event_capacity: 1,
        };
        let (writer, registered) = collector::recording::Registration::start_with_identity(
            &root,
            &place,
            shape,
            collector::ring::DictionaryCapacity {
                entries: 640,
                arena_bytes: 65_536,
            },
            0,
        )
        .unwrap()
        .into_parts();
        registered.unwrap();
        let mut fixture = Self {
            root,
            writer,
            processes: Vec::with_capacity(640),
            identities: Vec::with_capacity(640),
        };

        fixture.initialize_apps();

        fixture
    }

    fn initialize_apps(&mut self) {
        for index in 0..640 {
            let path = format!("/Applications/History Capacity {index:04}.app");
            let mut program = None;
            self.writer
                .intern_program(
                    &collector::ring::ProgramBytes {
                        executable_name: b"main",
                        app_path: Some(path.as_bytes()),
                    },
                    &mut program,
                )
                .unwrap();
            let mut process = process(0, 0);
            process.pid = index + 7;
            self.identities.push(IdentityObservation {
                pid: process.pid,
                usage: UsageAvailability::Measured,
                identity: ProcessIdentity::Measured {
                    creation: ProcessCreation {
                        unique_id: u64::try_from(index + 700).unwrap(),
                        pid_version: 1,
                    },
                    program: program.unwrap(),
                },
            });
            self.processes.push(process);
        }
    }

    fn publish(&mut self, publication: u64) {
        for (index, process) in self.processes.iter_mut().enumerate() {
            process.usage.user_ns = publication * 1_000_000;
            process.usage.energy_nj = publication * 2_000_000;
            process.usage.resident_bytes = publication * 100 + u64::try_from(index).unwrap();
            process.usage.footprint_bytes = publication * 200 + u64::try_from(index).unwrap();
        }
        let memory = Memory::default();

        self.writer
            .publish_with_identity(
                &readings(&self.processes, &memory, publication),
                Some(&ObservationTimes {
                    wall_ns: (1000 + publication) * 1_000_000_000,
                    continuous_ns: publication * 1_000_000_000,
                    awake_ns: publication * 1_000_000_000,
                }),
                &IdentityPublication {
                    processes: &self.identities,
                    events: &[],
                },
            )
            .unwrap();
    }

    fn observe_through(&mut self, session: &mut LiveSession, last: u64) {
        let timebase = collector::clock::Timebase::read().unwrap();
        let pid = i32::try_from(std::process::id()).unwrap();
        let before = collector::processes::read_usage(timebase, pid)
            .unwrap()
            .unwrap();
        let started = std::time::Instant::now();

        for publication in 2..=last {
            self.publish(publication);
            session.poll().unwrap();
        }
        let elapsed_ns = started.elapsed().as_nanos();
        let after = collector::processes::read_usage(timebase, pid)
            .unwrap()
            .unwrap();
        let footprint = persistence::resources::Footprint::read().unwrap();

        eprintln!(
            "{}",
            serde_json::json!({"fixture_publications":last,"apps_per_publication":640,
                "elapsed_ns":elapsed_ns,
                "process_cpu_ns":after.user_ns+after.system_ns-before.user_ns-before.system_ns,
                "physical_footprint_bytes":footprint.current_bytes,
                "process_lifetime_footprint_max_bytes":footprint.lifetime_bytes_max})
        );
    }
}

impl Drop for AppPopulation {
    fn drop(&mut self) {
        self.writer.finish();
        collector::ring::unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn retains_the_entire_inclusive_lower_second_with_fractional_wall_clocks() {
    // Nanosecond observation clocks must not trim part of the chart's first included second.
    let mut fixture = Fixture::new();
    publish_at(
        &mut fixture,
        &Moment {
            sample: 1,
            wall_ns: 1_001_100_000_000,
        },
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish_at(
        &mut fixture,
        &Moment {
            sample: 2,
            wall_ns: 1_900_900_000_000,
        },
    );
    session.poll().unwrap();

    let selection = session.follow_latest().unwrap();
    let history = session
        .app_history(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
        )
        .unwrap();

    assert_eq!(selection["trends"].as_array().unwrap().len(), 2);
    assert_eq!(history["points"].as_array().unwrap().len(), 2);
}

struct Moment {
    sample: u64,
    wall_ns: u64,
}

#[test]
fn keeps_compact_app_trends_after_actual_raw_cache_pressure() {
    // Real 12000-member publications evict raw rows without discarding small App histories.
    let mut fixture = Fixture::new_mode(true, 12_000, None);
    fixture.publish(1000);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    for second in 1001..=1016 {
        fixture.publish(second);
        session.poll().unwrap();
    }
    session.follow_latest().unwrap();

    let history = session
        .app_history(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Apps/Current.app",
        )
        .unwrap();
    let catalog = session.catalog();

    assert!(catalog["tail_ticks"].as_u64().unwrap() < 17);
    assert_eq!(history["points"].as_array().unwrap().len(), 17);
    assert_eq!(
        history["points"][0]["observation"]["stamp"]["publication"],
        "1"
    );
    assert_eq!(
        history["points"][16]["observation"]["stamp"]["publication"],
        "17"
    );
    assert_eq!(history["points"][16]["cpu_ns"]["known"], "1");
    assert_eq!(history["points"][16]["resident_bytes"]["known"], "11");
    assert_eq!(history["coverage"]["retention_limited"], false);
    assert!(
        history["coverage"]["cache_bytes"].as_u64().unwrap()
            <= history["coverage"]["cache_bytes_max"].as_u64().unwrap()
    );
}

#[test]
fn keeps_app_history_with_an_explicit_gap_after_its_last_process_disappears() {
    // A selected App ending is normal live state, not a failed dashboard query.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish(&mut fixture, 2);
    session.poll().unwrap();
    let memory = Memory::default();
    fixture
        .writer
        .publish_with_identity(
            &readings(&[], &memory, 3),
            Some(&ObservationTimes {
                wall_ns: 1_003_000_000_000,
                continuous_ns: 3_000_000_000,
                awake_ns: 3_000_000_000,
            }),
            &IdentityPublication {
                processes: &[],
                events: &[],
            },
        )
        .unwrap();
    session.poll().unwrap();
    session.follow_latest().unwrap();
    let kind = viewer::live_app_response::KeyKind::Bundle;
    let key = b"/Applications/A.app";

    let detail = session.app_detail(kind, key, 0).unwrap();
    let history = session.app_history(kind, key).unwrap();

    assert!(detail.is_null());
    assert_eq!(history["points"].as_array().unwrap().len(), 3);
    assert_eq!(history["points"][1]["cpu_percent_one_core"], 50.0);
    assert_eq!(history["points"][2]["members"], serde_json::Value::Null);
    assert_eq!(
        history["points"][2]["resident_bytes"],
        serde_json::Value::Null
    );
}

fn publish(fixture: &mut Fixture, sample: u64) {
    publish_at(
        fixture,
        &Moment {
            sample,
            wall_ns: (1000 + sample) * 1_000_000_000,
        },
    );
}

fn publish_at(fixture: &mut Fixture, moment: &Moment) {
    let mut identities = Vec::with_capacity(2);
    for (index, path) in [
        b"/Applications/A.app/Contents/MacOS/main".as_slice(),
        b"/Applications/A.app/Contents/Helpers/Child.app/Contents/MacOS/helper".as_slice(),
    ]
    .iter()
    .enumerate()
    {
        let mut bytes = None;
        collector::programs::split_program(path, &mut bytes).unwrap();
        let mut program = None;
        fixture
            .writer
            .intern_program(&bytes.unwrap(), &mut program)
            .unwrap();
        identities.push(IdentityObservation {
            pid: i32::try_from(index + 7).unwrap(),
            usage: UsageAvailability::Measured,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: u64::try_from(index + 700).unwrap(),
                    pid_version: 1,
                },
                program: program.unwrap(),
            },
        });
    }
    let processes = [process(0, moment.sample), process(1, moment.sample)];
    let memory = Memory::default();

    fixture
        .writer
        .publish_with_identity(
            &readings(&processes, &memory, moment.sample),
            Some(&ObservationTimes {
                wall_ns: moment.wall_ns,
                continuous_ns: moment.sample * 1_000_000_000,
                awake_ns: moment.sample * 1_000_000_000,
            }),
            &IdentityPublication {
                processes: &identities,
                events: &[],
            },
        )
        .unwrap();
}

fn readings<'a>(processes: &'a [Process], memory: &'a Memory, run_tick: u64) -> Readings<'a> {
    Readings {
        run_tick,
        cpus: &[],
        memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes,
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[],
        dropped: Dropped::default(),
    }
}

fn process(index: usize, sample: u64) -> Process {
    Process {
        pid: i32::try_from(index + 7).unwrap(),
        usage: Usage {
            awake_ns_at_start: 0,
            user_ns: [200_000_000, 300_000_000][index] * sample,
            system_ns: 0,
            energy_nj: [1_000_000_000, 2_000_000_000][index] * sample,
            instructions: 0,
            super_instructions: 0,
            cycles: 0,
            idle_wakeups: 0,
            interrupt_wakeups: 0,
            pageins: 0,
            resident_bytes: [104_857_600, 52_428_800][index],
            footprint_bytes: [110_000_000, 55_000_000][index],
            disk_read_bytes: 0,
            disk_written_bytes: 0,
        },
        task: Task {
            threads: 1,
            faults: 0,
            context_switches: 0,
            mach_syscalls: 0,
            unix_syscalls: 0,
        },
    }
}
