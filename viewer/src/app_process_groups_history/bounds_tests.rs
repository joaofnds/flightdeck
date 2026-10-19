use std::sync::Arc;

use persistence::Error;

use super::ProcessGroupReading;
use super::reduce::{Capture, Phase, Reduction};
use crate::Cancellation;
use crate::live_app_response::{KeyKind, Measure, Observation};

#[test]
fn refuses_the_4097th_context_observation_after_accepting_4096() {
    // Distinct publications in one second reach the operation's count bound without a time gap.
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let selected = observation(4097);
    let mut reduction = Reduction::new(&selected, KeyKind::Bundle, b"/A.app", &operation).unwrap();
    observe_points(&mut reduction, 4096);
    let extra = observation(4097);

    let result = reduction.begin(&capture(&extra), Phase::Observe);

    assert!(matches!(
        result,
        Err(Error::Bound("process cohort observations"))
    ));
}

#[test]
fn refuses_the_24001st_distinct_name_across_individually_bounded_observations() {
    // Twenty-four thousand names in one observation leave no dictionary slot for a later new name.
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let selected = observation(2);
    let mut reduction = Reduction::new(&selected, KeyKind::Bundle, b"/A.app", &operation).unwrap();
    let first = observation(1);
    assert!(reduction.begin(&capture(&first), Phase::Observe).unwrap());
    observe_names(&mut reduction, 0, 24_000, 8);
    assert!(
        reduction
            .begin(&capture(&selected), Phase::Observe)
            .unwrap()
    );
    let reading = reading();

    let result = reduction.group(b"extra-name", &reading);

    assert!(matches!(result, Err(Error::Bound("process cohort names"))));
}

#[test]
fn refuses_aggregate_key_bytes_before_the_name_count_limit() {
    // Eight batches admit 2048 maximum-length names; name 2049 exceeds eight MiB of key bytes.
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let selected = observation(9);
    let mut reduction = Reduction::new(&selected, KeyKind::Bundle, b"/A.app", &operation).unwrap();
    observe_long_names(&mut reduction);
    assert!(
        reduction
            .begin(&capture(&selected), Phase::Observe)
            .unwrap()
    );
    let extra = name(2048, 4095);
    let reading = reading();

    let result = reduction.group(&extra, &reading);

    assert_eq!(2048 * 4095, 8_386_560);
    assert_eq!(2049 * 4095, 8_390_655);
    assert!(matches!(
        result,
        Err(Error::Bound("process cohort key bytes"))
    ));
}

fn observe_points(reduction: &mut Reduction<'_>, count: u64) {
    assert!(count <= 4096);
    for publication in 1..=count {
        let observation = observation(publication);
        assert!(
            reduction
                .begin(&capture(&observation), Phase::Observe)
                .unwrap()
        );
    }
}

fn observe_names(reduction: &mut Reduction<'_>, first: u32, count: u32, bytes: usize) {
    assert!(count <= 24_000);
    assert!(first <= 24_000);
    let reading = reading();
    for index in first..first + count {
        reduction.group(&name(index, bytes), &reading).unwrap();
    }
}

fn observe_long_names(reduction: &mut Reduction<'_>) {
    for batch in 0..8_u32 {
        let observation = observation(u64::from(batch + 1));
        assert!(
            reduction
                .begin(&capture(&observation), Phase::Observe)
                .unwrap()
        );
        observe_names(reduction, batch * 256, 256, 4095);
    }
}

fn name(index: u32, bytes: usize) -> Vec<u8> {
    assert!(bytes <= 4095);
    assert!(bytes >= 8);
    let mut name = format!("{index:08}").into_bytes();
    name.resize(bytes, b'x');
    name
}

fn reading() -> ProcessGroupReading {
    let mut measures = [Measure {
        known: 1,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    }; 7];
    measures[4].known = 2;
    ProcessGroupReading {
        present: true,
        members: 1,
        measures,
    }
}

fn capture(observation: &Observation) -> Capture<'_> {
    Capture {
        observation,
        elapsed_ns: Some(1),
        source_available: true,
        membership_complete: true,
        unnamed_members: Some(0),
        live: true,
    }
}

fn observation(publication: u64) -> Observation {
    assert!(publication <= 4097);
    Observation {
        recording: [1, 2],
        stamp: crate::apps::Stamp {
            boot: [3, 4],
            run: [5, 6],
            publication,
            continuous_ns: Some(publication),
            awake_ns: Some(publication),
        },
        run_tick: publication,
        wall_ns: Some(1_000_000_000_000 + publication),
    }
}
