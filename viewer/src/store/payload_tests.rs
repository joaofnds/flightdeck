use super::ProcessRow;
use crate::apps::{Amount, ProcessState, Unavailable};
use crate::live_app_response::Process;

macro_rules! retained_amount_case {
    ($name:ident, $amount:expr) => {
        #[test]
        fn $name() {
            // Goal: a retained row preserves this amount in every metric without a sentinel.
            // Method: capture a canonical process and compare its public metric values.
            let expected = [$amount; 11];
            let source = process(expected);

            let row = ProcessRow::capture(&source, None);

            assert_eq!(amounts(&row), expected);
        }
    };
}

retained_amount_case!(retains_measured_zero, Amount::Known(0));
retained_amount_case!(retains_measured_maximum, Amount::Known(u64::MAX));
retained_amount_case!(retains_measured_high_bit, Amount::Known(1 << 63));
retained_amount_case!(
    retains_refused_amounts,
    Amount::Unavailable(Unavailable::Refused)
);
retained_amount_case!(retains_gone_amounts, Amount::Unavailable(Unavailable::Gone));
retained_amount_case!(
    retains_unknown_amounts,
    Amount::Unavailable(Unavailable::Unknown)
);
retained_amount_case!(
    retains_interval_amounts,
    Amount::Unavailable(Unavailable::Interval)
);
retained_amount_case!(
    retains_creation_amounts,
    Amount::Unavailable(Unavailable::Creation)
);
retained_amount_case!(
    retains_baseline_amounts,
    Amount::Unavailable(Unavailable::Baseline)
);
retained_amount_case!(
    retains_reset_amounts,
    Amount::Unavailable(Unavailable::CounterReset)
);
retained_amount_case!(
    retains_overflowed_amounts,
    Amount::Unavailable(Unavailable::Overflow)
);
retained_amount_case!(retains_exec_amounts, Amount::Unavailable(Unavailable::Exec));
retained_amount_case!(
    retains_program_amounts,
    Amount::Unavailable(Unavailable::Program)
);
retained_amount_case!(
    retains_unverified_exit_amounts,
    Amount::Unavailable(Unavailable::ExitUnverified)
);

#[test]
fn retains_independent_values_and_availability_across_metrics() {
    // Goal: changing one metric cannot alter another metric's value or availability.
    // Method: round-trip fixed-seed generated canonical rows, reporting the failing seed.
    let mut seed = 0x67f3_29c4_a10b_8e55_u64;
    for _ in 0..4096 {
        let before = seed;
        let expected = std::array::from_fn(|_| generated_amount(&mut seed));
        let source = process(expected);

        let row = ProcessRow::capture(&source, None);

        assert_eq!(amounts(&row), expected, "generated seed {before}");
    }
}

#[test]
fn complete_process_rows_fit_the_preflight_storage_allowance() {
    // Goal: the measured million-process live span can coexist with retained readers.
    // Method: reject a row layout above the lossless compact representation's allowance.
    assert!(
        size_of::<ProcessRow>() <= 152,
        "row bytes {}",
        size_of::<ProcessRow>()
    );
}

fn amounts(row: &ProcessRow) -> [Amount; 11] {
    [
        row.cpu_ns(),
        row.energy_nj(),
        row.gpu_ns(),
        row.disk_read_bytes(),
        row.disk_written_bytes(),
        row.disk_io_bytes(),
        row.resident_bytes(),
        row.footprint_bytes(),
        row.idle_wakeups(),
        row.pageins(),
        row.threads(),
    ]
}

fn process(amounts: [Amount; 11]) -> Process {
    Process {
        pid: 7,
        creation: None,
        app_index: Some(0),
        executable_name: None,
        program_unavailable: None,
        cpu_ns: amounts[0],
        energy_nj: amounts[1],
        gpu_ns: amounts[2],
        disk_read_bytes: amounts[3],
        disk_written_bytes: amounts[4],
        disk_io_bytes: amounts[5],
        resident_bytes: amounts[6],
        footprint_bytes: amounts[7],
        idle_wakeups: amounts[8],
        pageins: amounts[9],
        threads: amounts[10],
        app_attribution: Some(Unavailable::Unknown),
        state: ProcessState::Live,
    }
}

fn generated_amount(seed: &mut u64) -> Amount {
    *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    match *seed % 14 {
        0 => Amount::Known(0),
        1 => Amount::Known(u64::MAX),
        2 => Amount::Known(*seed),
        3 => Amount::Unavailable(Unavailable::Refused),
        4 => Amount::Unavailable(Unavailable::Gone),
        5 => Amount::Unavailable(Unavailable::Unknown),
        6 => Amount::Unavailable(Unavailable::Interval),
        7 => Amount::Unavailable(Unavailable::Creation),
        8 => Amount::Unavailable(Unavailable::Baseline),
        9 => Amount::Unavailable(Unavailable::CounterReset),
        10 => Amount::Unavailable(Unavailable::Overflow),
        11 => Amount::Unavailable(Unavailable::Exec),
        12 => Amount::Unavailable(Unavailable::Program),
        13 => Amount::Unavailable(Unavailable::ExitUnverified),
        _ => unreachable!("the generator chooses one of fourteen amounts"),
    }
}
