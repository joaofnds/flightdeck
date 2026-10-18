use super::*;

#[test]
fn keeps_exec_exit_membership_when_selected_from_committed_history() {
    // Goal: a last-15-minute selection keeps event-only Apps after live data retirement.
    // Method: seal the real Exec/Exit fixture and compare fresh selected detail with live output.
    let mut fixture = Fixture::new();
    publish_empty(&mut fixture);
    let mut live = LiveSession::open(&fixture.root).unwrap();
    Scenario::default().publish(&mut fixture);
    live.poll().unwrap();
    live.follow_latest().unwrap();
    let kind = viewer::live_app_response::KeyKind::Bundle;
    let key = b"/Applications/B.app";
    let expected = live.app_detail(kind, key, 0).unwrap();
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut cold = LiveSession::open(&fixture.root).unwrap();

    cold.select(1002, 0).unwrap();
    let page = cold.apps_page(0).unwrap();
    assert_eq!(page["process_count"], 1);
    let detail = cold.app_detail(kind, key, 0).unwrap();
    let history = cold.app_history(kind, key).unwrap();

    assert_eq!(detail, expected);
    let points = history["points"].as_array().unwrap();
    assert_eq!(points.len(), 2);
    assert_eq!(points[1]["observation"], detail["selected"]);
    assert_eq!(points[1]["members"], 1);
    assert_eq!(points[1]["resident_bytes"], detail["app"]["resident_bytes"]);
    assert_eq!(points[1]["cpu_ns"], detail["app"]["cpu_ns"]);
}

#[test]
fn retains_an_app_observed_at_exec_before_its_verified_exit() {
    // Goal: an event-only process keeps its observed App without inventing interval rates.
    // Method: publish captured Exec/Exit identity in a real ring and read the live App response.
    let mut fixture = Fixture::new();
    publish_empty(&mut fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    Scenario::default().publish(&mut fixture);

    session.poll().unwrap();
    session.follow_latest().unwrap();
    let page = session.apps_page(0).unwrap();

    assert_eq!(page["process_count"], 1);
    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/B.app",
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
        detail["processes"][0]["executable_name"]["display"],
        "worker"
    );
    assert_eq!(
        detail["processes"][0]["resident_bytes"]["known"],
        "104857600"
    );
}

#[test]
fn retains_the_new_app_without_attributing_a_cross_exec_interval() {
    // Goal: a changed image names the new App while leaving its cross-Exec rates unavailable.
    // Method: replace scheduled App A's main process with an observed App B Exec and Exit.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    Scenario::default().publish(&mut fixture);

    session.poll().unwrap();
    session.follow_latest().unwrap();
    let page = session.apps_page(0).unwrap();
    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/B.app",
            0,
        )
        .unwrap();

    assert_eq!(page["process_count"], 1);
    assert_eq!(detail["app"]["members"], 1);
    assert_eq!(detail["app"]["live_members"], 0);
    assert_eq!(
        detail["app"]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
    assert_eq!(detail["app"]["energy_joules"], serde_json::Value::Null);
    assert_eq!(detail["processes"][0]["app_attribution"], "exec");
    assert_eq!(detail["processes"][0]["cpu_percent_one_core"], 20.0);
    assert_eq!(detail["processes"][0]["energy_joules"], 1.0);
}

#[test]
fn prefers_later_captured_program_bytes_over_a_matching_predecessor_version() {
    // Goal: stable version metadata cannot erase a later captured change of App path/name.
    // Method: select Exec B after predecessor A with the same full creation, then verify Exit B.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let creation = ProcessCreation {
        unique_id: 700,
        pid_version: 1,
    };
    Scenario {
        exec_creation: creation,
        exit_creation: creation,
        ..Scenario::default()
    }
    .publish(&mut fixture);

    session.poll().unwrap();
    session.follow_latest().unwrap();
    let page = session.apps_page(0).unwrap();

    assert_eq!(page["apps"][0]["key"]["display"], "/Applications/B.app");
    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/B.app",
            0,
        )
        .unwrap();
    assert_eq!(
        detail["app"]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
    assert_eq!(detail["app"]["energy_joules"], serde_json::Value::Null);
    assert_eq!(detail["processes"][0]["app_attribution"], "exec");
    assert_eq!(detail["processes"][0]["cpu_percent_one_core"], 20.0);
}

#[test]
fn associates_exec_bytes_beside_both_scheduled_and_start_programs() {
    // Goal: mixed program populations cannot relabel an Exec from another arena/view offset.
    // Method: capture live A, Start D and Exec/Exit B, then inspect B through the public API.
    let mut fixture = Fixture::new();
    publish_empty(&mut fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    Scenario {
        scheduled: Some(ProcessCreation {
            unique_id: 800,
            pid_version: 1,
        }),
        order: &[Kind::Start, Kind::StartExit, Kind::Exec, Kind::Exit],
        ..Scenario::default()
    }
    .publish(&mut fixture);

    session.poll().unwrap();
    session.follow_latest().unwrap();
    let page = session.apps_page(0).unwrap();
    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/B.app",
            0,
        )
        .unwrap();

    assert_eq!(page["process_count"], 3);
    assert_eq!(page["app_count"], 3);
    assert_eq!(detail["processes"][0]["pid"], 7);
    assert_eq!(
        detail["processes"][0]["executable_name"]["display"],
        "worker"
    );
    assert_eq!(detail["app"]["live_members"], 0);
    let started = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/D.app",
            0,
        )
        .unwrap();
    assert_eq!(started["processes"][0]["pid"], 9);
    assert_eq!(
        started["processes"][0]["executable_name"]["display"],
        "start"
    );
    assert_eq!(started["app"]["resident_bytes"]["known"], "0");
    assert_eq!(
        started["processes"][0]["resident_bytes"]["known"],
        "52428800"
    );
}

#[test]
fn retains_the_latest_observed_image_before_exit_once() {
    // Goal: successive Exec observations select the final matching image without duplicating PID.
    // Method: capture B then C for one creation and read the exact C App response.
    let mut fixture = Fixture::new();
    publish_empty(&mut fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    Scenario {
        exit_creation: ProcessCreation {
            unique_id: 700,
            pid_version: 3,
        },
        order: &[Kind::Exec, Kind::NextExec, Kind::Exit],
        ..Scenario::default()
    }
    .publish(&mut fixture);

    session.poll().unwrap();
    session.follow_latest().unwrap();
    let page = session.apps_page(0).unwrap();

    assert_eq!(page["process_count"], 1);
    assert_eq!(page["app_count"], 1);
    assert_eq!(page["apps"][0]["key"]["display"], "/Applications/C.app");
}

#[test]
fn owns_exec_program_bytes_after_the_live_source_is_reused() {
    // Goal: reusing source buffers cannot replace a captured historical Exec App name.
    // Method: follow an Exec/Exit, publish new members, then select the retained old second.
    let mut fixture = Fixture::new();
    publish_empty(&mut fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    Scenario::default().publish(&mut fixture);
    session.poll().unwrap();
    publish(&mut fixture, 3);
    session.poll().unwrap();

    session.select(1002, 0).unwrap();
    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/B.app",
            0,
        )
        .unwrap();

    assert_eq!(detail["selected"]["stamp"]["publication"], "2");
    assert_eq!(
        detail["processes"][0]["executable_name"]["display"],
        "worker"
    );
    assert_eq!(detail["processes"][0]["creation"]["pid_version"], 2);
}

macro_rules! interrupted_predecessor {
    ($name:ident, $kind:expr) => {
        #[test]
        fn $name() {
            // Goal: later uncertainty cannot inherit an older App even at the same PID version.
            // Method: follow scheduled A with an unavailable Exec and a verified same-version Exit.
            let mut fixture = Fixture::new();
            publish(&mut fixture, 1);
            let mut session = LiveSession::open(&fixture.root).unwrap();
            let creation = ProcessCreation {
                unique_id: 700,
                pid_version: 1,
            };
            Scenario {
                exec_creation: creation,
                exit_creation: creation,
                order: &[$kind, Kind::Exit],
                ..Scenario::default()
            }
            .publish(&mut fixture);

            session.poll().unwrap();
            session.follow_latest().unwrap();
            let page = session.apps_page(0).unwrap();

            assert_eq!(page["process_count"], 1);
            assert_eq!(page["app_count"], 0);
            assert_eq!(page["ungrouped_members"], 1);
        }
    };
}

interrupted_predecessor!(
    refuses_stale_app_bytes_after_an_unassociated_exec,
    Kind::UnassociatedExec
);
interrupted_predecessor!(
    retains_a_refused_exec_over_older_known_app_bytes,
    Kind::RefusedExec
);

macro_rules! unjoined_exec {
    ($name:ident, $scenario:expr) => {
        #[test]
        fn $name() {
            // Goal: unassociated executable observations never name a verified final process.
            // Method: vary one source identity/order boundary through the real live App API.
            let mut fixture = Fixture::new();
            publish_empty(&mut fixture);
            let mut session = LiveSession::open(&fixture.root).unwrap();
            $scenario.publish(&mut fixture);

            session.poll().unwrap();
            session.follow_latest().unwrap();
            let page = session.apps_page(0).unwrap();

            assert_eq!(page["process_count"], 0);
            assert_eq!(page["app_count"], 0);
            assert_eq!(page["membership_complete"], false);
        }
    };
}

unjoined_exec!(
    refuses_an_exec_from_another_process_creation,
    Scenario {
        exit_creation: ProcessCreation {
            unique_id: 701,
            pid_version: 2
        },
        ..Scenario::default()
    }
);
unjoined_exec!(
    refuses_an_exec_from_another_executable_version,
    Scenario {
        exit_creation: ProcessCreation {
            unique_id: 700,
            pid_version: 3
        },
        ..Scenario::default()
    }
);
unjoined_exec!(
    refuses_an_exec_observed_after_the_exit,
    Scenario {
        order: &[Kind::Exit, Kind::Exec],
        ..Scenario::default()
    }
);
unjoined_exec!(
    refuses_to_bridge_a_later_unassociated_exec,
    Scenario {
        order: &[Kind::Exec, Kind::UnassociatedExec, Kind::Exit],
        ..Scenario::default()
    }
);

macro_rules! committed_attribution {
    ($name:ident, $scenario:expr, $key:expr, $apps:expr, $ungrouped:expr) => {
        #[test]
        fn $name() {
            // Goal: committed event attribution follows the same captured facts as live output.
            // Method: seal the real scenario and compare the reopened selected App detail.
            let scenario = $scenario;
            let mut fixture = Fixture::new();
            publish(&mut fixture, 1);
            let mut live = LiveSession::open(&fixture.root).unwrap();
            scenario.publish(&mut fixture);
            live.poll().unwrap();
            live.follow_latest().unwrap();
            let kind = viewer::live_app_response::KeyKind::Bundle;
            let expected = live.app_detail(kind, $key, 0).unwrap();
            let expected_page = live.apps_page(0).unwrap();
            fixture.seal();
            collector::recording::Registry {
                runs: Vec::new(),
                evictions: 0,
            }
            .write(&fixture.root)
            .unwrap();
            let mut cold = LiveSession::open(&fixture.root).unwrap();

            cold.select(1002, 0).unwrap();
            let page = cold.apps_page(0).unwrap();
            assert_eq!(page["process_count"], 1);
            assert_eq!(page["app_count"], $apps);
            assert_eq!(page["ungrouped_members"], $ungrouped);
            assert_eq!(page["membership_complete"], false);
            assert_eq!(page, expected_page);
            let detail = cold.app_detail(kind, $key, 0).unwrap();
            let history = cold.app_history(kind, $key).unwrap();

            assert_eq!(detail, expected);
            let points = history["points"].as_array().unwrap();
            assert_eq!(points.len(), 2);
            assert_eq!(points[1]["members"], detail["app"]["members"]);
            assert_eq!(points[1]["cpu_ns"], detail["app"]["cpu_ns"]);
            assert_eq!(points[1]["energy_nj"], detail["app"]["energy_nj"]);
            assert_eq!(points[1]["resident_bytes"], detail["app"]["resident_bytes"]);
        }
    };
}

committed_attribution!(
    restores_captured_start_exit_app_facts,
    Scenario {
        order: &[Kind::Start, Kind::StartExit],
        ..Scenario::default()
    },
    b"/Applications/D.app",
    1,
    0
);
committed_attribution!(
    restores_the_latest_compatible_exec_before_exit,
    Scenario {
        order: &[Kind::Exec, Kind::NextExec, Kind::Exit],
        exit_creation: ProcessCreation {
            unique_id: 700,
            pid_version: 3
        },
        ..Scenario::default()
    },
    b"/Applications/C.app",
    1,
    0
);
committed_attribution!(
    keeps_refused_exec_identity_unattributed_after_reopen,
    Scenario {
        order: &[Kind::RefusedExec, Kind::Exit],
        ..Scenario::default()
    },
    b"/Applications/A.app",
    0,
    1
);
committed_attribution!(
    keeps_an_unassociated_exec_from_reusing_the_previous_app_after_reopen,
    Scenario {
        order: &[Kind::Exec, Kind::UnassociatedExec, Kind::Exit],
        ..Scenario::default()
    },
    b"/Applications/A.app",
    0,
    1
);
committed_attribution!(
    restores_scheduled_predecessor_verified_exit_usage,
    Scenario {
        order: &[Kind::Exit],
        exit_creation: ProcessCreation {
            unique_id: 700,
            pid_version: 1
        },
        ..Scenario::default()
    },
    b"/Applications/A.app",
    1,
    0
);

committed_attribution!(
    keeps_refused_exec_over_equal_version_predecessor_bytes_after_reopen,
    Scenario {
        order: &[Kind::RefusedExec, Kind::Exit],
        exec_creation: ProcessCreation {
            unique_id: 700,
            pid_version: 1
        },
        exit_creation: ProcessCreation {
            unique_id: 700,
            pid_version: 1
        },
        ..Scenario::default()
    },
    b"/Applications/A.app",
    0,
    1
);

#[test]
fn keeps_duplicate_exit_evidence_ambiguous_after_reopen() {
    // Goal: duplicate exit witnesses cannot manufacture a second endpoint or fail App reads.
    // Method: seal distinct duplicate Exit events and require the independent helper subtotal.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1);
    let mut live = LiveSession::open(&fixture.root).unwrap();
    ExitScenario {
        duplicate: true,
        ..ExitScenario::default()
    }
    .publish(&mut fixture);
    live.poll().unwrap();
    live.follow_latest().unwrap();
    let expected = live.apps_page(0).unwrap();
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut cold = LiveSession::open(&fixture.root).unwrap();

    cold.select(1002, 0).unwrap();
    let page = cold.apps_page(0).unwrap();
    let history = cold
        .app_history(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Applications/A.app",
        )
        .unwrap();

    assert_eq!(page["process_count"], 1);
    assert_eq!(page["apps"][0]["cpu_percent_one_core"], 30.0);
    assert_eq!(page["membership_complete"], false);
    assert_eq!(page, expected);
    assert_eq!(history["points"][1]["members"], 1);
    assert_eq!(history["points"][1]["cpu_percent_one_core"], 30.0);
}

struct Scenario {
    exec_creation: ProcessCreation,
    exit_creation: ProcessCreation,
    scheduled: Option<ProcessCreation>,
    order: &'static [Kind],
}

#[derive(Clone, Copy)]
enum Kind {
    Start,
    StartExit,
    Exec,
    NextExec,
    UnassociatedExec,
    RefusedExec,
    Exit,
}

impl Default for Scenario {
    fn default() -> Self {
        Self {
            exec_creation: ProcessCreation {
                unique_id: 700,
                pid_version: 2,
            },
            exit_creation: ProcessCreation {
                unique_id: 700,
                pid_version: 2,
            },
            order: &[Kind::Exec, Kind::Exit],
            scheduled: None,
        }
    }
}

impl Scenario {
    fn publish(&self, fixture: &mut Fixture) {
        let events: Vec<_> = self.order.iter().copied().map(Self::event).collect();
        let identities: Vec<_> = self
            .order
            .iter()
            .copied()
            .map(|kind| self.identity(kind, fixture))
            .collect();
        let memory = Memory::default();
        let mut processes = Vec::new();
        let mut process_identities = Vec::new();
        if let Some(creation) = self.scheduled {
            processes.push(process(0, 2));
            process_identities.push(ExitScenario::publish_process_identity(
                fixture,
                0,
                &ProcessCreation {
                    unique_id: 700,
                    pid_version: 1,
                },
            ));
            processes.push(process(1, 2));
            process_identities.push(ExitScenario::publish_process_identity(
                fixture, 1, &creation,
            ));
        }
        let mut selected = readings(&processes, &memory, 2);
        selected.events = &events;

        fixture
            .writer
            .publish_with_identity(
                &selected,
                Some(&ObservationTimes {
                    wall_ns: 1_002_000_000_000,
                    continuous_ns: 2_000_000_000,
                    awake_ns: 2_000_000_000,
                }),
                &IdentityPublication {
                    processes: &process_identities,
                    events: &identities,
                },
            )
            .unwrap();
    }

    fn event(kind: Kind) -> collector::tracker::Event {
        match kind {
            Kind::Start => collector::tracker::Event::Start {
                pid: 9,
                coalition: None,
            },
            Kind::StartExit => collector::tracker::Event::Exit {
                pid: 9,
                last: Ok(process(1, 2).usage),
                noticed: collector::tracker::Noticed::OnExit,
            },
            Kind::Exit => collector::tracker::Event::Exit {
                pid: 7,
                last: Ok(process(0, 2).usage),
                noticed: collector::tracker::Noticed::OnExit,
            },
            Kind::Exec | Kind::NextExec | Kind::UnassociatedExec | Kind::RefusedExec => {
                collector::tracker::Event::Exec { pid: 7 }
            }
        }
    }

    fn identity(&self, kind: Kind, fixture: &mut Fixture) -> IdentityObservation {
        match kind {
            Kind::StartExit => IdentityObservation {
                pid: 9,
                usage: UsageAvailability::Measured,
                identity: ProcessIdentity::Unavailable {
                    creation: Some(ProcessCreation {
                        unique_id: 900,
                        pid_version: 1,
                    }),
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
            Kind::RefusedExec => IdentityObservation {
                pid: 7,
                usage: UsageAvailability::NotApplicable,
                identity: ProcessIdentity::Unavailable {
                    creation: Some(self.exec_creation),
                    reason: IdentityUnavailable::Refused,
                },
            },
            Kind::UnassociatedExec => IdentityObservation {
                pid: 7,
                usage: UsageAvailability::NotApplicable,
                identity: ProcessIdentity::Unavailable {
                    creation: None,
                    reason: IdentityUnavailable::Changed,
                },
            },
            Kind::Start | Kind::Exec | Kind::NextExec => self.program(kind, fixture),
        }
    }

    fn program(&self, kind: Kind, fixture: &mut Fixture) -> IdentityObservation {
        let (name, app, version): (&[u8], &[u8], i32) = match kind {
            Kind::Start => (b"start", b"/Applications/D.app", 1),
            Kind::Exec => (
                b"worker",
                b"/Applications/B.app",
                self.exec_creation.pid_version,
            ),
            Kind::NextExec => (
                b"next",
                b"/Applications/C.app",
                self.exec_creation.pid_version + 1,
            ),
            Kind::UnassociatedExec | Kind::RefusedExec | Kind::StartExit | Kind::Exit => {
                unreachable!("program observation only")
            }
        };
        let mut program = None;
        fixture
            .writer
            .intern_program(
                &collector::ring::ProgramBytes {
                    executable_name: name,
                    app_path: Some(app),
                },
                &mut program,
            )
            .unwrap();

        IdentityObservation {
            pid: if matches!(kind, Kind::Start) { 9 } else { 7 },
            usage: UsageAvailability::NotApplicable,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: if matches!(kind, Kind::Start) {
                        900
                    } else {
                        self.exec_creation.unique_id
                    },
                    pid_version: version,
                },
                program: program.unwrap(),
            },
        }
    }
}
