use super::tests::{aggregate, interval, observation, program, usage};
use super::*;

macro_rules! measured_interval {
    (
        $name:ident, $elapsed:expr, $cpu:expr, $energy:expr,
        $percent:expr, $watts:expr, $joules:expr, $read:expr, $written:expr, $combined:expr
    ) => {
        #[test]
        fn $name() {
            // Goal: rates use continuous elapsed time rather than cadence or awake time.
            // Method: compute one App over distinct continuous and awake clock intervals.
            let mut interval = interval();
            interval.selected.continuous_ns = Some(1_000_000_000 + $elapsed);
            interval.previous.awake_ns = Some(10_000_000_000);
            interval.selected.awake_ns = Some(10_100_000_000);
            let program = program(b"/Applications/A.app");
            let mut before_usage = usage(10, 20, 100);
            before_usage.disk_read_bytes = 9_007_199_254_740_993;
            before_usage.disk_written_bytes = 9_007_199_254_741_993;
            let mut selected_usage = usage(10 + $cpu, 20 + $energy, 200);
            selected_usage.disk_read_bytes = before_usage.disk_read_bytes + 4096;
            selected_usage.disk_written_bytes = before_usage.disk_written_bytes + 1024;
            let before = observation(&interval.previous, &program, &before_usage);
            let selected = observation(&interval.selected, &program, &selected_usage);
            let mut result = aggregate();

            result
                .compute(
                    &interval,
                    &[ProcessInterval {
                        previous: Some(&before),
                        selected: &selected,
                        start: None,
                    }],
                )
                .unwrap();

            let app = &result.apps[&AppKey::Bundle(b"/Applications/A.app")];
            assert_eq!(result.elapsed_ns, Some($elapsed));
            assert_eq!(
                cpu_percent_one_core(CpuRate {
                    cpu_ns: app.cpu_ns.known,
                    elapsed_ns: result.elapsed_ns.unwrap()
                }),
                Some($percent)
            );
            assert_eq!(
                energy_watts(EnergyRate {
                    energy_nj: app.energy_nj.known,
                    elapsed_ns: result.elapsed_ns.unwrap()
                }),
                Some($watts)
            );
            assert!((energy_joules(app.energy_nj.known) - $joules).abs() < f64::EPSILON);
            assert_eq!(
                disk_rates(app, result.elapsed_ns.unwrap()),
                [Some($read), Some($written), Some($combined)]
            );
        }
    };
}

fn disk_rates(app: &AppTotals, elapsed_ns: u64) -> [Option<f64>; 3] {
    [
        app.disk_read_bytes.known,
        app.disk_written_bytes.known,
        app.disk_io_bytes.known,
    ]
    .map(|bytes| bytes_per_second(BytesRate { bytes, elapsed_ns }))
}
measured_interval!(
    uses_two_seconds_for_rates,
    2_000_000_000,
    500_000_000,
    3_000_000_000,
    25.0,
    1.5,
    3.0,
    2048.0,
    512.0,
    2560.0
);
measured_interval!(
    uses_subsecond_elapsed_for_rates,
    250_000_000,
    125_000_000,
    500_000_000,
    50.0,
    2.0,
    0.5,
    16384.0,
    4096.0,
    20480.0
);

macro_rules! source_birth_boundary {
    ($name:ident, $birth:expr, $expected:expr) => {
        #[test]
        fn $name() {
            // Goal: a new-creation baseline requires birth inside (awake lower, awake upper].
            // Method: compute a first observation with verified identity at one boundary.
            let interval = interval();

            let actual = first_observation(&interval, $birth);

            assert_eq!(actual, $expected);
        }
    };
}
source_birth_boundary!(
    refuses_birth_at_lower_awake_boundary,
    1_000_000_000,
    Amount::Unavailable(Unavailable::Baseline)
);
source_birth_boundary!(
    accepts_birth_one_ns_after_lower_boundary,
    1_000_000_001,
    Amount::Known(400)
);
source_birth_boundary!(
    accepts_birth_at_upper_awake_boundary,
    2_000_000_000,
    Amount::Known(400)
);
source_birth_boundary!(
    refuses_birth_one_ns_after_upper_boundary,
    2_000_000_001,
    Amount::Unavailable(Unavailable::Baseline)
);

fn first_observation(interval: &Interval, birth_ns: u64) -> Amount {
    let program = program(b"/Applications/A.app");
    let mut usage = usage(400, 600, 800);
    usage.awake_ns_at_start = birth_ns;
    let selected = observation(&interval.selected, &program, &usage);
    let start = VerifiedStart {
        stamp: interval.selected,
        pid: selected.pid,
        creation: selected.creation.unwrap(),
        program: Ok(&program),
    };
    let mut result = aggregate();

    result
        .compute(
            interval,
            &[ProcessInterval {
                previous: None,
                selected: &selected,
                start: Some(&start),
            }],
        )
        .unwrap();

    result.processes[0].cpu_ns
}

#[test]
fn preserves_rates_across_generated_continuous_durations() {
    // Goal: rate arithmetic scales with varied actual durations, independently of awake clocks.
    // Method: generate 128 fixed-seed durations with exact quarter-core and three-watt deltas.
    let mut seed = 0x7bd1_ee64_018c_a597_u64;
    for case in 0..128 {
        let elapsed_ns = (generated(&mut seed) % 1_000_000 + 1) * 4_000;
        let mut interval = interval();
        interval.selected.continuous_ns = Some(1_000_000_000 + elapsed_ns);
        interval.previous.awake_ns = Some(50_000_000_000);
        interval.selected.awake_ns = Some(50_000_000_001 + elapsed_ns / 2);
        let program = program(b"/Applications/A.app");
        let before_usage = usage(10, 20, 100);
        let selected_usage = usage(10 + elapsed_ns / 4, 20 + elapsed_ns * 3, 200);
        let before = observation(&interval.previous, &program, &before_usage);
        let selected = observation(&interval.selected, &program, &selected_usage);
        let mut result = aggregate();

        result
            .compute(
                &interval,
                &[ProcessInterval {
                    previous: Some(&before),
                    selected: &selected,
                    start: None,
                }],
            )
            .unwrap();

        let app = &result.apps[&AppKey::Bundle(b"/Applications/A.app")];
        assert_eq!(
            result.elapsed_ns,
            Some(elapsed_ns),
            "case={case}, elapsed={elapsed_ns}"
        );
        assert_eq!(
            cpu_percent_one_core(CpuRate {
                cpu_ns: app.cpu_ns.known,
                elapsed_ns: result.elapsed_ns.unwrap()
            }),
            Some(25.0),
            "case={case}, elapsed={elapsed_ns}"
        );
        assert_eq!(
            energy_watts(EnergyRate {
                energy_nj: app.energy_nj.known,
                elapsed_ns: result.elapsed_ns.unwrap()
            }),
            Some(3.0),
            "case={case}, elapsed={elapsed_ns}"
        );
    }
}

#[test]
fn accepts_exactly_generated_births_inside_the_awake_interval() {
    // Goal: only births in the source awake interval receive a zero baseline.
    // Method: generate 128 independent awake windows and births around their boundaries.
    let mut seed = 0x00f4_12dc_4aa6_7e92_u64;
    for case in 0..128 {
        let lower_ns = generated(&mut seed) % 1_000_000_000 + 1;
        let width_ns = generated(&mut seed) % 1_000_000 + 1;
        let birth_offset_ns = generated(&mut seed) % (width_ns * 2 + 2);
        let mut interval = interval();
        interval.previous.awake_ns = Some(lower_ns);
        interval.selected.awake_ns = Some(lower_ns + width_ns);
        let expected = expected_birth(&BirthPosition {
            width_ns,
            birth_offset_ns,
        });

        let actual = first_observation(&interval, lower_ns + birth_offset_ns);

        assert_eq!(
            actual, expected,
            "case={case}, lower={lower_ns}, width={width_ns}, offset={birth_offset_ns}"
        );
    }
}

struct BirthPosition {
    width_ns: u64,
    birth_offset_ns: u64,
}

fn expected_birth(position: &BirthPosition) -> Amount {
    if (1..=position.width_ns).contains(&position.birth_offset_ns) {
        Amount::Known(400)
    } else {
        Amount::Unavailable(Unavailable::Baseline)
    }
}

fn generated(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    *seed
}
