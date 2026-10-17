use collector::machine::Memory;
use collector::processes::{Process, Task, Usage};
use collector::ring::{
    Dropped, IdentityObservation, IdentityPublication, ObservationTimes, ProcessCreation,
    ProcessIdentity, Readings, UsageAvailability,
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
    assert_eq!(points[0]["cpu_percent_one_core"], serde_json::Value::Null);
    assert_eq!(points[1]["cpu_percent_one_core"], 50.0);
    assert_eq!(points[1]["resident_bytes"]["known"], "157286400");
    assert_eq!(points[1]["energy_watts"], 3.0);
    assert_eq!(history["coverage"]["retention_limited"], false);
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
