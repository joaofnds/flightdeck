use super::*;

#[test]
fn retains_process_usage_across_exec_after_a_verified_creation() {
    // Goal: a new process retains its interval CPU across exec, without guessing its App split.
    // Method: use a verified Start and changed PID version in the same observed interval.
    let interval = interval();
    let before_program = program(b"/Applications/Before.app");
    let after_program = program(b"/Applications/After.app");
    let mut usage = usage(200_000_000, 3_000_000_000, 100);
    usage.awake_ns_at_start = 1_500_000_000;
    let mut selected = observation(&interval.selected, &after_program, &usage);
    selected.creation.as_mut().unwrap().pid_version = 2;
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

    assert_eq!(result.processes[0].cpu_ns, Amount::Known(200_000_000));
    assert_eq!(result.processes[0].energy_nj, Amount::Known(3_000_000_000));
    assert_eq!(result.processes[0].app_attribution, Some(Unavailable::Exec));
    assert_eq!(
        result.apps[&AppKey::Bundle(b"/Applications/After.app")]
            .cpu_ns
            .known,
        0
    );
}

pub(super) fn interval() -> Box<Interval> {
    Box::new(Interval {
        previous: Stamp {
            boot: [1, 2],
            run: [3, 4],
            publication: 1,
            continuous_ns: Some(1_000_000_000),
            awake_ns: Some(1_000_000_000),
        },
        selected: Stamp {
            boot: [1, 2],
            run: [3, 4],
            publication: 2,
            continuous_ns: Some(2_000_000_000),
            awake_ns: Some(2_000_000_000),
        },
        observations_complete: true,
        events_complete: true,
    })
}

#[allow(clippy::unnecessary_box_returns)] // Tiger Style heap-allocates large fixture values.
pub(super) fn program(path: &[u8]) -> Box<ProgramBytes<'_>> {
    Box::new(ProgramBytes {
        executable_name: b"helper",
        app_path: Some(path),
    })
}

pub(super) fn observation<'a>(
    stamp: &Stamp,
    program: &'a ProgramBytes<'a>,
    usage: &'a Usage,
) -> Box<ProcessObservation<'a>> {
    Box::new(ProcessObservation {
        stamp: *stamp,
        pid: 7,
        creation: Some(ProcessCreation {
            unique_id: 700,
            pid_version: 1,
        }),
        program: Ok(program),
        usage: Ok(usage),
        threads: None,
        state: ProcessState::Live,
    })
}

#[allow(clippy::unnecessary_box_returns)] // Tiger Style heap-allocates large fixture values.
pub(super) fn usage(cpu_ns: u64, energy_nj: u64, resident_bytes: u64) -> Box<Usage> {
    Box::new(Usage {
        awake_ns_at_start: 10,
        user_ns: cpu_ns,
        system_ns: 0,
        energy_nj,
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: 0,
        interrupt_wakeups: 0,
        pageins: 0,
        resident_bytes,
        footprint_bytes: resident_bytes + 1,
        disk_read_bytes: 0,
        disk_written_bytes: 0,
    })
}

#[allow(clippy::unnecessary_box_returns)] // Tiger Style heap-allocates large fixture values.
pub(super) fn aggregate<'a>() -> Box<Aggregate<'a>> {
    let mut output = None;
    Aggregate::initialize(&mut output);
    output.unwrap()
}
