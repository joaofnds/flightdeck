use super::super::{Observation, Sample};
use crate::apps::{self, ProcessCreation, ProgramBytes};
use collector::processes::Usage;

#[derive(Clone, Copy)]
pub enum Scenario {
    Ordinary,
    Refused,
    Birth,
    Gap,
    Exec,
    Legacy,
    Bytes,
    Overflow,
}

pub fn sample(scenario: Scenario) -> Box<Sample> {
    let previous_stamp = stamp();
    let selected_stamp = apps::Stamp {
        publication: previous_stamp.publication
            + if matches!(scenario, Scenario::Gap) {
                2
            } else {
                1
            },
        continuous_ns: Some(2_000_000_000),
        awake_ns: Some(2_000_000_000),
        ..previous_stamp
    };
    let programs = programs(scenario);
    let before = [usage(0, 0, 0), usage(0, 0, 0)];
    let mut after = [
        usage(200_000_000, 1_000_000_000, 104_857_600),
        usage(300_000_000, 2_000_000_000, 52_428_800),
    ];
    if matches!(scenario, Scenario::Overflow) {
        after[0] = usage(u64::MAX, u64::MAX, u64::MAX);
    }
    let mut previous = observations(previous_stamp, &before, &programs);
    let mut selected = observations(selected_stamp, &after, &programs);
    scenario_observations(scenario, &mut previous, &mut selected);
    let processes: Vec<_> = previous
        .iter()
        .zip(&selected)
        .map(|(previous, selected)| apps::ProcessInterval {
            previous: if matches!(scenario, Scenario::Birth) {
                None
            } else {
                Some(previous)
            },
            selected,
            start: None,
        })
        .collect();
    let mut aggregate = None;
    apps::Aggregate::initialize(&mut aggregate);
    let aggregate = aggregate.as_mut().unwrap();
    aggregate
        .compute(
            &apps::Interval {
                previous: previous_stamp,
                selected: selected_stamp,
                observations_complete: true,
                events_complete: false,
            },
            &processes,
        )
        .unwrap();

    capture(aggregate, selected_stamp)
}

pub fn capture(aggregate: &apps::Aggregate<'_>, selected: apps::Stamp) -> Box<Sample> {
    let mut output = None;
    Sample::from_aggregate(
        &Observation {
            recording: [5, 6],
            stamp: selected,
            run_tick: 2,
            wall_ns: Some(1_800_000_000_000_000_000),
        },
        Some(stamp()),
        aggregate,
        &mut output,
    )
    .unwrap();
    output.unwrap()
}

pub fn stamp() -> apps::Stamp {
    apps::Stamp {
        boot: [1, 2],
        run: [3, 4],
        publication: 9_007_199_254_740_992,
        continuous_ns: Some(1_000_000_000),
        awake_ns: Some(1_000_000_000),
    }
}

fn programs(scenario: Scenario) -> [ProgramBytes<'static>; 2] {
    if matches!(scenario, Scenario::Bytes) {
        return [
            ProgramBytes {
                executable_name: b"name\xff",
                app_path: None,
            },
            ProgramBytes {
                executable_name: b"name\\xff",
                app_path: None,
            },
        ];
    }

    [
        ProgramBytes {
            executable_name: b"main",
            app_path: Some(b"/A.app"),
        },
        ProgramBytes {
            executable_name: b"helper",
            app_path: Some(b"/A.app"),
        },
    ]
}

fn scenario_observations<'a>(
    scenario: Scenario,
    previous: &mut [apps::ProcessObservation<'a>],
    selected: &mut [apps::ProcessObservation<'a>],
) {
    match scenario {
        Scenario::Ordinary => {
            previous[0].gpu_ns = Some(100_000_000);
            selected[0].gpu_ns = Some(350_000_000);
        }
        Scenario::Refused => selected[1].usage = Err(apps::Unavailable::Refused),
        Scenario::Exec => {
            selected[0].creation.as_mut().unwrap().pid_version = 2;
            selected[1].creation.as_mut().unwrap().pid_version = 2;
        }
        Scenario::Legacy => {
            for observation in previous.iter_mut().chain(selected) {
                observation.program = Err(apps::IdentityUnavailable::Unsupported);
                observation.creation = None;
            }
        }
        Scenario::Birth | Scenario::Gap | Scenario::Bytes | Scenario::Overflow => {}
    }
}

pub(crate) fn usage(cpu: u64, energy: u64, resident: u64) -> Usage {
    Usage {
        awake_ns_at_start: 1,
        user_ns: cpu,
        system_ns: 0,
        energy_nj: energy,
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: 0,
        interrupt_wakeups: 0,
        pageins: 0,
        resident_bytes: resident,
        footprint_bytes: resident,
        disk_read_bytes: cpu,
        disk_written_bytes: energy,
    }
}

fn observations<'a>(
    stamp: apps::Stamp,
    usages: &'a [Usage; 2],
    programs: &'a [ProgramBytes; 2],
) -> Vec<apps::ProcessObservation<'a>> {
    usages
        .iter()
        .zip(programs)
        .enumerate()
        .map(|(index, (usage, program))| apps::ProcessObservation {
            stamp,
            pid: i32::try_from(index + 1).unwrap(),
            creation: Some(ProcessCreation {
                unique_id: 9_007_199_254_740_993 + u64::try_from(index).unwrap(),
                pid_version: 1,
            }),
            program: Ok(program),
            usage: Ok(usage),
            gpu_ns: None,
            threads: None,
            state: apps::ProcessState::Live,
        })
        .collect()
}

pub fn populated(count: usize, name: &[u8]) -> persistence::Result<Box<Sample>> {
    let mut apps = std::collections::BTreeMap::new();
    apps.insert(
        apps::AppKey::Executable(b"shared"),
        apps::AppTotals {
            members: u32::try_from(count).unwrap(),
            live_members: u32::try_from(count).unwrap(),
            ..apps::AppTotals::default()
        },
    );
    let processes = (0..count)
        .map(|index| apps::ProcessTotals {
            pid: i32::try_from(index + 1).unwrap(),
            creation: None,
            app: Some(apps::AppKey::Executable(b"shared")),
            executable_name: Some(name),
            program_unavailable: None,
            cpu_ns: apps::Amount::Unavailable(apps::Unavailable::Baseline),
            energy_nj: apps::Amount::Unavailable(apps::Unavailable::Baseline),
            gpu_ns: apps::Amount::Unavailable(apps::Unavailable::Baseline),
            disk_read_bytes: apps::Amount::Unavailable(apps::Unavailable::Baseline),
            disk_written_bytes: apps::Amount::Unavailable(apps::Unavailable::Baseline),
            disk_io_bytes: apps::Amount::Unavailable(apps::Unavailable::Baseline),
            idle_wakeups: apps::Amount::Unavailable(apps::Unavailable::Baseline),
            pageins: apps::Amount::Unavailable(apps::Unavailable::Baseline),
            threads: None,
            app_attribution: None,
            resident_bytes_last: Some(0),
            footprint_bytes_last: Some(0),
            state: apps::ProcessState::Live,
        })
        .collect();
    let aggregate = apps::Aggregate {
        apps,
        processes,
        ungrouped_members: 0,
        live_ungrouped_members: 0,
        membership_complete: false,
        elapsed_ns: None,
    };
    let mut output = None;
    Sample::from_aggregate(
        &Observation {
            recording: [1, 2],
            stamp: stamp(),
            run_tick: 1,
            wall_ns: None,
        },
        None,
        &aggregate,
        &mut output,
    )?;
    Ok(output.unwrap())
}
