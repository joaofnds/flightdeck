use super::tests::{aggregate, interval, observation, program, usage};
use super::*;

struct GpuTimes {
    before: Option<u64>,
    selected: Option<u64>,
}

struct GpuResult {
    processes: Vec<Amount>,
    app: AppTotals,
}

#[test]
fn sums_the_gpu_time_each_member_spent_over_the_interval() {
    // Goal: an App's GPU time is the sum of what each member's GPU clients gained between the
    // two observations. Method: two members of one App with GPU times at both observations.
    let times = [
        GpuTimes {
            before: Some(1_000),
            selected: Some(4_000),
        },
        GpuTimes {
            before: Some(500),
            selected: Some(700),
        },
    ];

    let actual = summarize(&times);

    assert_eq!(actual.processes, [Amount::Known(3_000), Amount::Known(200)]);
    assert_eq!(
        actual.app.gpu_ns,
        Subtotal {
            known: 3_200,
            measured_members: 2,
            unavailable_members: 0,
            overflowed: false
        }
    );
}

#[test]
fn reads_a_member_without_gpu_time_at_either_observation_as_unknown() {
    // Goal: a missing GPU time, which a process without GPU clients and a failed GPU read both
    // leave, never reads as zero. Method: one member lacks the earlier time and one the later.
    let times = [
        GpuTimes {
            before: None,
            selected: Some(4_000),
        },
        GpuTimes {
            before: Some(500),
            selected: None,
        },
    ];

    let actual = summarize(&times);

    assert_eq!(
        actual.processes,
        [Amount::Unavailable(Unavailable::Unknown); 2]
    );
    assert_eq!(
        actual.app.gpu_ns,
        Subtotal {
            known: 0,
            measured_members: 0,
            unavailable_members: 2,
            overflowed: false
        }
    );
}

#[test]
fn reads_a_gpu_time_that_fell_as_a_counter_reset() {
    // Goal: GPU time that falls, as when a process closes one of its GPU clients, invents no
    // amount. Method: a member whose later GPU time is below its earlier one.
    let times = [GpuTimes {
        before: Some(4_000),
        selected: Some(1_000),
    }];

    let actual = summarize(&times);

    assert_eq!(
        actual.processes,
        [Amount::Unavailable(Unavailable::CounterReset)]
    );
}

#[test]
fn counts_the_gpu_time_of_a_process_started_in_the_interval_from_zero() {
    // Goal: a process verified to start inside the interval spent all its GPU time in it, as
    // its CPU time and energy are counted. Method: a verified Start and no earlier observation.
    let interval = interval();
    let program = program(b"/A.app");
    let mut usage = usage(200, 300, 400);
    usage.awake_ns_at_start = 1_500_000_000;
    let mut selected = observation(&interval.selected, &program, &usage);
    selected.gpu_ns = Some(9_000);
    let start = VerifiedStart {
        stamp: Stamp {
            continuous_ns: Some(1_500_000_000),
            ..interval.selected
        },
        pid: 7,
        creation: ProcessCreation {
            unique_id: 700,
            pid_version: 1,
        },
        program: Ok(&program),
    };
    let input = [ProcessInterval {
        previous: None,
        selected: &selected,
        start: Some(&start),
    }];
    let mut result = aggregate();

    result.compute(&interval, &input).unwrap();

    assert_eq!(result.processes[0].gpu_ns, Amount::Known(9_000));
}

fn summarize(times: &[GpuTimes]) -> Box<GpuResult> {
    assert!(times.len() <= 2);
    let interval = interval();
    let program = program(b"/A.app");
    let before_usage = usage(100, 200, 300);
    let selected_usage = usage(400, 600, 800);
    let previous = summarize_observations(&SummarizeSide {
        stamp: &interval.previous,
        program: &program,
        usage: &before_usage,
        gpu_ns: times.iter().map(|time| time.before).collect(),
    });
    let selected = summarize_observations(&SummarizeSide {
        stamp: &interval.selected,
        program: &program,
        usage: &selected_usage,
        gpu_ns: times.iter().map(|time| time.selected).collect(),
    });
    let processes: Vec<_> = previous
        .iter()
        .zip(&selected)
        .map(|(previous, selected)| ProcessInterval {
            previous: Some(previous),
            selected,
            start: None,
        })
        .collect();
    let mut aggregate = aggregate();

    aggregate.compute(&interval, &processes).unwrap();

    Box::new(GpuResult {
        processes: aggregate
            .processes
            .iter()
            .map(|process| process.gpu_ns)
            .collect(),
        app: aggregate.apps.remove(&AppKey::Bundle(b"/A.app")).unwrap(),
    })
}

struct SummarizeSide<'a> {
    stamp: &'a Stamp,
    program: &'a ProgramBytes<'a>,
    usage: &'a Usage,
    gpu_ns: Vec<Option<u64>>,
}

fn summarize_observations<'a>(side: &SummarizeSide<'a>) -> Vec<Box<ProcessObservation<'a>>> {
    assert!(side.gpu_ns.len() <= 2);
    side.gpu_ns
        .iter()
        .enumerate()
        .map(|(index, gpu_ns)| {
            let mut value = observation(side.stamp, side.program, side.usage);
            value.pid += i32::try_from(index).unwrap();
            value.gpu_ns = *gpu_ns;
            value
        })
        .collect()
}

#[test]
fn keeps_the_gpu_time_of_a_process_that_exec_d_out_of_its_new_app() {
    // Goal: a process that replaced its program during the interval keeps its GPU time, which
    // the App it runs now does not take, as with its CPU time. Method: a verified start under
    // one bundle and the selected observation under another, with GPU time at the selected one.
    let interval = interval();
    let before_program = program(b"/Applications/Before.app");
    let after_program = program(b"/Applications/After.app");
    let mut usage = usage(200_000_000, 3_000_000_000, 100);
    usage.awake_ns_at_start = 1_500_000_000;
    let mut selected = observation(&interval.selected, &after_program, &usage);
    selected.creation.as_mut().unwrap().pid_version = 2;
    selected.gpu_ns = Some(250_000_000);
    let start = VerifiedStart {
        stamp: Stamp {
            continuous_ns: Some(1_500_000_000),
            ..interval.selected
        },
        pid: 7,
        creation: ProcessCreation {
            unique_id: 700,
            pid_version: 1,
        },
        program: Ok(&before_program),
    };
    let input = [ProcessInterval {
        previous: None,
        selected: &selected,
        start: Some(&start),
    }];
    let mut result = aggregate();

    result.compute(&interval, &input).unwrap();

    assert_eq!(result.processes[0].gpu_ns, Amount::Known(250_000_000));
    assert_eq!(result.processes[0].app_attribution, Some(Unavailable::Exec));
    let app = &result.apps[&AppKey::Bundle(b"/Applications/After.app")];
    assert_eq!(app.gpu_ns.known, 0);
}
