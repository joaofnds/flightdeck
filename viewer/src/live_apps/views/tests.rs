use super::*;
use crate::apps::{Amount, AppKey, Stamp, Unavailable};
use crate::live_apps::{Exec, ProgramSpan, Start};
use collector::processes::Usage;
use collector::ring::{Identity, ProcessCreation, RunIdentity};

struct Scenario {
    previous: Box<Frame>,
    selected: Box<Frame>,
}

impl Scenario {
    fn new() -> Self {
        let mut previous = frame(1);
        previous.arena = b"worker/Applications/B.app".to_vec();
        previous.execs.push(Exec {
            number: 0,
            pid: 7,
            creation: Some(creation()),
            program: Ok(program()),
        });
        let mut selected = frame(2);
        selected.exits.push(Exit {
            number: 1,
            pid: 7,
            creation: Some(creation()),
            usage: Ok(usage()),
            ambiguous: false,
        });

        Self { previous, selected }
    }

    fn start(&mut self) {
        self.previous.execs.clear();
        self.previous.starts.push(Start {
            number: 0,
            pid: 7,
            creation: Some(creation()),
            program: Ok(program()),
            ambiguous: false,
            exec_number: None,
        });
    }

    fn assert_app(&self) {
        self.selected
            .with_aggregate(Some(&self.previous), |aggregate| {
                assert_eq!(aggregate.processes.len(), 1);
                let app = &aggregate.apps[&AppKey::Bundle(b"/Applications/B.app")];
                assert_eq!(app.members, 1);
                assert_eq!(app.live_members, 0);
                assert_eq!(app.resident_bytes.known, 0);
                assert_eq!(
                    aggregate.processes[0].resident_bytes_last,
                    Some(104_857_600)
                );
                assert_eq!(
                    aggregate.processes[0].cpu_ns,
                    Amount::Unavailable(Unavailable::Baseline)
                );
                assert_eq!(
                    aggregate.processes[0].energy_nj,
                    Amount::Unavailable(Unavailable::Baseline)
                );
                Ok(())
            })
            .unwrap();
    }

    fn assert_absent(&self) {
        self.selected
            .with_aggregate(Some(&self.previous), |aggregate| {
                assert!(aggregate.processes.is_empty());
                assert!(aggregate.apps.is_empty());
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn retains_a_previous_exec_without_creating_a_numeric_baseline() {
    // Goal: prior program metadata names an exited process without inventing a measured delta.
    // Method: aggregate adjacent owned event-only frames after their source has gone.
    Scenario::new().assert_app();
}

#[test]
fn retains_a_previous_start_without_creating_a_numeric_baseline() {
    // Goal: a prior unambiguous Start supplies the same identity-only evidence as Exec.
    // Method: replace the predecessor event kind while keeping exact creation and final usage.
    let mut scenario = Scenario::new();
    scenario.start();

    scenario.assert_app();
}

#[test]
fn prefers_the_selected_exec_over_the_previous_event() {
    // Goal: current captured bytes supersede prior identity even if its version is unchanged.
    // Method: add a matching Exec for C before the selected Exit after prior Exec B.
    let mut scenario = Scenario::new();
    scenario.selected.arena = b"worker/Applications/C.app".to_vec();
    scenario.selected.execs.push(Exec {
        number: 1,
        pid: 7,
        creation: Some(creation()),
        program: Ok(program()),
    });
    scenario.selected.exits[0].number = 2;

    scenario
        .selected
        .with_aggregate(Some(&scenario.previous), |aggregate| {
            assert_eq!(aggregate.apps.len(), 1);
            assert!(
                aggregate
                    .apps
                    .contains_key(&AppKey::Bundle(b"/Applications/C.app"))
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn prefers_the_selected_start_over_the_previous_event() {
    // Goal: an acquired current Start is newer than an otherwise matching prior program.
    // Method: retain C Start bytes before Exit after a matching prior B Exec.
    let mut scenario = Scenario::new();
    scenario.selected.arena = b"worker/Applications/C.app".to_vec();
    scenario.selected.starts.push(Start {
        number: 1,
        pid: 7,
        creation: Some(creation()),
        program: Ok(program()),
        ambiguous: false,
        exec_number: None,
    });
    scenario.selected.exits[0].number = 2;

    scenario
        .selected
        .with_aggregate(Some(&scenario.previous), |aggregate| {
            assert!(
                aggregate
                    .apps
                    .contains_key(&AppKey::Bundle(b"/Applications/C.app"))
            );
            assert_eq!(aggregate.processes.len(), 1);
            Ok(())
        })
        .unwrap();
}

#[test]
fn blocks_prior_bytes_after_a_current_start_with_another_image_version() {
    // Goal: incompatible newer Start evidence cannot fall through to a stale prior App name.
    // Method: put acquired version 3 before a version 2 verified Exit of the known process.
    let mut scenario = Scenario::new();
    scenario.selected.starts.push(Start {
        number: 1,
        pid: 7,
        creation: Some(ProcessCreation {
            unique_id: 700,
            pid_version: 3,
        }),
        program: Err(IdentityUnavailable::Refused),
        ambiguous: false,
        exec_number: None,
    });
    scenario.selected.exits[0].number = 2;

    scenario
        .selected
        .with_aggregate(Some(&scenario.previous), |aggregate| {
            assert!(aggregate.apps.is_empty());
            assert_eq!(aggregate.ungrouped_members, 1);
            assert_eq!(
                aggregate.processes[0].program_unavailable,
                Some(IdentityUnavailable::Changed)
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn ignores_start_metadata_observed_after_the_selected_exit() {
    // Goal: a later event cannot rewrite the program known at an earlier verified Exit.
    // Method: place an ambiguous Start after Exit and retain the matching prior Exec bytes.
    let mut scenario = Scenario::new();
    scenario.selected.starts.push(Start {
        number: 2,
        pid: 7,
        creation: Some(creation()),
        program: Err(IdentityUnavailable::Refused),
        ambiguous: true,
        exec_number: None,
    });

    scenario.assert_app();
}

#[test]
fn blocks_prior_bytes_when_duplicate_starts_straddle_the_selected_exit() {
    // Goal: a duplicate found after Exit cannot hide the ambiguous Start observed before it.
    // Method: place two ambiguous Starts around Exit, so binary search may find the later one.
    let mut scenario = Scenario::new();
    scenario.selected.starts = [1, 3]
        .into_iter()
        .map(|number| Start {
            number,
            pid: 7,
            creation: Some(creation()),
            program: Err(IdentityUnavailable::Refused),
            ambiguous: true,
            exec_number: None,
        })
        .collect();
    scenario.selected.exits[0].number = 2;

    scenario
        .selected
        .with_aggregate(Some(&scenario.previous), |aggregate| {
            assert!(aggregate.apps.is_empty());
            assert_eq!(aggregate.ungrouped_members, 1);
            Ok(())
        })
        .unwrap();
}

#[test]
fn preserves_raw_previous_event_bytes_in_the_app_key() {
    // Goal: prior event fallback groups by original captured bytes rather than display text.
    // Method: place a non-UTF-8 byte in the predecessor's owned App path.
    let mut scenario = Scenario::new();
    scenario.previous.arena[20] = 255;

    scenario
        .selected
        .with_aggregate(Some(&scenario.previous), |aggregate| {
            assert!(
                aggregate
                    .apps
                    .contains_key(&AppKey::Bundle(b"/Applications/\xff.app"))
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn admits_prior_events_at_the_combined_program_view_bound() {
    // Goal: including previous event views remains within the existing transient allocation bound.
    // Method: aggregate one Exec beside 23,999 unrelated scheduled predecessor identities.
    let mut scenario = Scenario::new();
    scenario.previous.members = (100..24_099)
        .map(|pid| crate::live_apps::Member {
            pid,
            creation: Some(ProcessCreation {
                unique_id: u64::try_from(pid).unwrap(),
                pid_version: 1,
            }),
            program: Err(IdentityUnavailable::Unsupported),
            usage: Err(Unavailable::Refused),
            gpu_ns: None,
            threads: None,
        })
        .collect();

    scenario.assert_app();
}

macro_rules! excludes_predecessor {
    ($name:ident, $change:expr) => {
        mod $name {
            use super::*;

            fn observe(run_tick: u64) {
                let mut scenario = Scenario::new();
                scenario.selected.run_tick = run_tick;
                $change(&mut scenario);

                scenario.assert_absent();
            }

            #[test]
            fn adjacent() {
                // Goal: incompatible predecessor metadata cannot name an App.
                // Method: alter one fact of an otherwise matching adjacent pair.
                observe(2);
            }

            #[test]
            fn after_a_missed_tick() {
                // Goal: a tick gap does not weaken predecessor evidence exclusions.
                // Method: apply the same conflict across a skipped scheduled tick.
                observe(3);
            }
        }
    };
}

excludes_predecessor!(excludes_another_recording, |s: &mut Scenario| {
    s.previous.identity.recording = Identity([90, 91]);
});
excludes_predecessor!(excludes_another_boot, |s: &mut Scenario| {
    s.previous.stamp.boot = [90, 91];
});
excludes_predecessor!(excludes_another_run, |s: &mut Scenario| {
    s.previous.stamp.run = [90, 91];
});
excludes_predecessor!(excludes_a_publication_gap, |s: &mut Scenario| {
    s.selected.stamp.publication = 3;
});
excludes_predecessor!(excludes_a_missing_clock, |s: &mut Scenario| {
    s.previous.stamp.continuous_ns = None;
});
excludes_predecessor!(excludes_a_reversed_clock, |s: &mut Scenario| {
    s.previous.stamp.continuous_ns = Some(3_000_000_000);
});
excludes_predecessor!(excludes_a_reused_pid, |s: &mut Scenario| {
    s.previous.execs[0].creation.as_mut().unwrap().unique_id = 701;
});
excludes_predecessor!(excludes_another_image_version, |s: &mut Scenario| {
    s.previous.execs[0].creation.as_mut().unwrap().pid_version = 3;
});
excludes_predecessor!(excludes_an_ambiguous_start, |s: &mut Scenario| {
    s.start();
    s.previous.starts[0].ambiguous = true;
});
excludes_predecessor!(excludes_an_interrupted_start, |s: &mut Scenario| {
    s.start();
    s.previous.starts[0].exec_number = Some(1);
    s.selected.exits[0].number = 2;
});

macro_rules! current_unavailable {
    ($name:ident, $creation:expr, $reason:expr) => {
        mod $name {
            use super::*;

            fn observe(run_tick: u64) {
                let mut scenario = Scenario::new();
                scenario.selected.run_tick = run_tick;
                scenario.selected.execs.push(Exec {
                    number: 1,
                    pid: 7,
                    creation: $creation,
                    program: Err($reason),
                });
                scenario.selected.exits[0].number = 2;

                scenario
                    .selected
                    .with_aggregate(Some(&scenario.previous), |aggregate| {
                        assert!(aggregate.apps.is_empty());
                        assert_eq!(aggregate.ungrouped_members, 1);
                        assert_eq!(aggregate.processes[0].program_unavailable, Some($reason));
                        assert_eq!(
                            aggregate.processes[0].resident_bytes_last,
                            Some(104_857_600)
                        );
                        Ok(())
                    })
                    .unwrap();
            }

            #[test]
            fn adjacent() {
                // Goal: newer unavailable Exec evidence blocks stale prior App bytes.
                // Method: put an unavailable Exec before the verified selected Exit.
                observe(2);
            }

            #[test]
            fn after_a_missed_tick() {
                // Goal: a tick gap cannot permit stale bytes after an unavailable Exec.
                // Method: put the conflict before Exit across a skipped scheduled tick.
                observe(3);
            }
        }
    };
}

current_unavailable!(
    blocks_prior_bytes_after_a_refused_exec,
    Some(creation()),
    IdentityUnavailable::Refused
);
current_unavailable!(
    blocks_prior_bytes_after_an_unassociated_exec,
    None,
    IdentityUnavailable::Changed
);

#[test]
fn keeps_scheduled_predecessor_identity_ahead_of_prior_event_fallback() {
    // Goal: the additional event path preserves scheduled predecessor precedence.
    // Method: provide conflicting captured event bytes beside a matching scheduled member.
    let mut scenario = Scenario::new();
    scenario.previous.members.push(crate::live_apps::Member {
        pid: 7,
        creation: Some(creation()),
        program: Err(IdentityUnavailable::Refused),
        usage: Ok(usage()),
        gpu_ns: None,
        threads: None,
    });

    scenario
        .selected
        .with_aggregate(Some(&scenario.previous), |aggregate| {
            assert!(aggregate.apps.is_empty());
            assert_eq!(aggregate.ungrouped_members, 1);
            assert_eq!(
                aggregate.processes[0].program_unavailable,
                Some(IdentityUnavailable::Refused)
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn preserves_the_supplied_frame_stamp_when_it_cannot_supply_evidence() {
    // Goal: Sample.previous records supplied metadata even when no predecessor is usable.
    // Method: capture an exit across a publication gap and inspect its exact predecessor stamp.
    let mut scenario = Scenario::new();
    scenario.selected.stamp.publication = 3;
    let mut sample = None;
    crate::live_app_response::Sample::capture(
        &scenario.selected,
        Some(&scenario.previous),
        &mut sample,
    )
    .unwrap();
    let sample = sample.unwrap();
    assert_eq!(sample.previous, Some(scenario.previous.stamp));
    assert_eq!(sample.elapsed_ns, None);
    assert!(sample.apps().is_empty());
}

fn assert_app_wall_interval(wall_ns: Option<u64>) {
    let mut scenario = Scenario::new();
    scenario.selected.wall_ns = wall_ns;
    scenario
        .selected
        .with_aggregate(Some(&scenario.previous), |aggregate| {
            assert_eq!(aggregate.elapsed_ns, Some(1_000_000_000));
            assert_eq!(aggregate.processes.len(), 1);
            assert_eq!(
                aggregate.processes[0].resident_bytes_last,
                Some(104_857_600)
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn keeps_app_intervals_when_selected_wall_time_is_equal() {
    // Goal: equal wall clocks do not suppress a valid App continuous-time interval.
    // Method: aggregate an ordinary pair with equal wall time.
    assert_app_wall_interval(Some(1_001_000_000_000));
}

#[test]
fn keeps_app_intervals_when_selected_wall_time_rolls_back() {
    // Goal: wall rollback does not import machine eligibility into the App proof.
    // Method: aggregate an ordinary pair with selected wall time before the predecessor.
    assert_app_wall_interval(Some(1_000_000_000_000));
}

#[test]
fn keeps_app_intervals_without_selected_wall_time() {
    // Goal: only the predecessor wall clock is required for App interval eligibility.
    // Method: aggregate an ordinary pair with absent selected wall time.
    assert_app_wall_interval(None);
}

#[test]
fn keeps_live_unavailable_gauges_absent_across_a_run_tick_gap() {
    // Goal: usable prior evidence never replaces an unavailable current live gauge.
    // Method: keep the same captured identity with measured prior and refused current usage.
    let mut scenario = Scenario::new();
    scenario.selected.run_tick = 3;
    scenario.selected.exits.clear();
    scenario.selected.arena.clone_from(&scenario.previous.arena);
    scenario.previous.members.push(crate::live_apps::Member {
        pid: 7,
        creation: Some(creation()),
        program: Ok(program()),
        usage: Ok(usage()),
        gpu_ns: None,
        threads: None,
    });
    scenario.selected.members.push(crate::live_apps::Member {
        pid: 7,
        creation: Some(creation()),
        program: Ok(program()),
        usage: Err(Unavailable::Refused),
        gpu_ns: None,
        threads: None,
    });
    scenario
        .selected
        .with_aggregate(Some(&scenario.previous), |aggregate| {
            assert_eq!(aggregate.elapsed_ns, None);
            assert_eq!(aggregate.apps.len(), 1);
            assert_eq!(aggregate.processes.len(), 1);
            assert_eq!(aggregate.processes[0].state, ProcessState::Live);
            assert_eq!(aggregate.processes[0].resident_bytes_last, None);
            assert_eq!(aggregate.processes[0].footprint_bytes_last, None);
            assert_eq!(
                aggregate.processes[0].cpu_ns,
                Amount::Unavailable(Unavailable::Interval)
            );
            let app = &aggregate.apps[&AppKey::Bundle(b"/Applications/B.app")];
            assert_eq!(app.members, 1);
            assert_eq!(app.live_members, 1);
            assert_eq!(app.resident_bytes.known, 0);
            assert_eq!(app.resident_bytes.measured_members, 0);
            assert_eq!(app.resident_bytes.unavailable_members, 1);
            Ok(())
        })
        .unwrap();
}

fn creation() -> ProcessCreation {
    ProcessCreation {
        unique_id: 700,
        pid_version: 2,
    }
}

fn program() -> ProgramSpan {
    ProgramSpan {
        start: 0,
        name_end: 6,
        end: 25,
    }
}

fn frame(publication: u64) -> Box<Frame> {
    Box::new(Frame {
        identity: RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, 4]),
            boot: Identity([5, 6]),
        },
        stamp: Stamp {
            boot: [5, 6],
            run: [3, 4],
            publication,
            continuous_ns: Some(publication * 1_000_000_000),
            awake_ns: Some(publication * 1_000_000_000),
        },
        run_tick: publication,
        wall_ns: Some((1000 + publication) * 1_000_000_000),
        members: Vec::new(),
        starts: Vec::new(),
        execs: Vec::new(),
        exits: Vec::new(),
        arena: Vec::new(),
        usage_refused: 0,
        usage_gone: 0,
    })
}

fn usage() -> Usage {
    Usage {
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
