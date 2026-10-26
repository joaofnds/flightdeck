use std::sync::Arc;

use super::Ring;
use crate::live::identity_fixture::Fixture;
use crate::store::{AppsSecond, ByteBudget, MachineSecond, recording, startup_tests::usage};

fn sealed_predecessor() -> (Fixture, recording::Terminal) {
    sealed_predecessor_at_tick(59)
}

fn sealed_predecessor_at_tick(run_tick: u64) -> (Fixture, recording::Terminal) {
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(run_tick);
    fixture.publish_with_usage(100, &usage(0));
    fixture.seal_minute(0);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let outcome =
        recording::load_payload(&fixture.root, &budget, &operation, |_, _, _| {}).unwrap();
    assert_eq!(outcome.left_out, None);
    assert_eq!(budget.used(), 0);
    (fixture, outcome.terminal.unwrap())
}

fn observe_next(mut fixture: Fixture, mut ring: Ring) -> (MachineSecond, AppsSecond) {
    fixture.publish_with_usage(101, &usage(1));
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut observed = Vec::new();
    ring.read(|_, machine, apps| {
        let mut payload = AppsSecond::with_room();
        payload.take(apps, None, Some(&budget));
        observed.push((*machine, payload));
        true
    })
    .unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].0.publication, 2);
    observed.pop().unwrap()
}

fn observe_handoff(fixture: Fixture, terminal: recording::Terminal) -> (MachineSecond, AppsSecond) {
    let mut ring = Ring::new(fixture.root.clone());
    ring.accept(Some(terminal));
    ring.follow().unwrap();

    observe_next(fixture, ring)
}

fn changed_clocks(
    change: impl FnOnce(&mut persistence::Observation),
) -> (MachineSecond, AppsSecond) {
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_usage(100, &usage(0));
    let mut observation = persistence::Observation {
        publication: 1,
        wall_ns: Some(100_000_000_000),
        continuous_ns: Some(59_000_000_000),
        awake_ns: Some(59_000_000_000),
    };
    change(&mut observation);
    fixture.seal_with_observations(&[observation]);
    let marker = persistence::layout::WindowKey {
        run: fixture.writer.identity().unwrap().run,
        window: 0,
    }
    .marker(&fixture.root);
    let sealed = crate::recording::SealedMinute::open(&fixture.root, &marker).unwrap();
    let mut machine = crate::recording::MachineMinute::default();
    crate::recording::read_machine(&sealed, &mut machine).unwrap();
    let mut process = None;
    sealed
        .visit_samples(&mut process, |_, sample| {
            assert!(sample.is_ok());
            Ok(())
        })
        .unwrap();
    assert!(process.is_some());
    let terminal = recording::Terminal {
        run: *sealed.identity(),
        machine,
        process,
    };

    observe_handoff(fixture, terminal)
}

fn assert_rates(observed: &(MachineSecond, AppsSecond), machine: bool, process: bool) {
    assert_eq!(observed.0.follows, machine);
    assert_eq!(observed.0.cpu_percent, machine.then_some(50.0));
    let payload = observed.1.processes.as_ref().unwrap();
    assert_eq!(payload.previous.is_some(), process);
    assert_eq!(observed.1.elapsed_ns, process.then_some(1_000_000_000));
}

#[test]
fn overlapping_ring_prefix_is_skipped_to_the_next_publication() {
    // Goal: an overlapping prefix cannot overwrite the terminal predecessor.
    // Method: retain the sealed tick in the ring and require only its successor on read.
    let (fixture, terminal) = sealed_predecessor();
    let observed = observe_handoff(fixture, terminal);

    assert_rates(&observed, true, true);
}

#[test]
fn missing_awake_clock_refuses_process_rates_only() {
    // Goal: machine and process validity remain independent through the handoff.
    // Method: seal a real missing awake clock and observe its ring successor.
    let observed = changed_clocks(|observation| observation.awake_ns = None);

    assert_rates(&observed, true, false);
}

#[test]
fn reversed_awake_clock_refuses_process_rates_only() {
    // Goal: awake reversal rejects process rates without rejecting a machine interval.
    // Method: seal the predecessor awake clock past its successor's time before handoff.
    let observed = changed_clocks(|observation| observation.awake_ns = Some(61_000_000_000));

    assert_rates(&observed, true, false);
}

#[test]
fn missing_wall_clock_refuses_both_predecessors() {
    // Goal: neither adapter takes rates from a terminal tick lacking wall time.
    // Method: decode a raw sealed tick without a wall clock, then hand it to the matching ring.
    let observed = changed_clocks(|observation| observation.wall_ns = None);

    assert_rates(&observed, false, false);
}

#[test]
fn missing_continuous_clock_refuses_both_predecessors() {
    // Goal: elapsed time cannot be invented at the handoff.
    // Method: seal a real missing continuous clock and observe its ring successor.
    let observed = changed_clocks(|observation| observation.continuous_ns = None);

    assert_rates(&observed, false, false);
}

#[test]
fn equal_continuous_clock_refuses_both_predecessors() {
    // Goal: a zero duration does not produce rates.
    // Method: seal the predecessor continuous clock at its successor's time before handoff.
    let observed = changed_clocks(|observation| observation.continuous_ns = Some(60_000_000_000));

    assert_rates(&observed, false, false);
}

#[test]
fn tick_gap_refuses_both_predecessors() {
    // Goal: an adjacent publication does not hide a missed collection tick.
    // Method: hand off a real sealed tick at 58, then publish its successor at tick 60.
    let (mut fixture, terminal) = sealed_predecessor_at_tick(58);
    fixture.skip_run_ticks(1);
    let observed = observe_handoff(fixture, terminal);

    assert_rates(&observed, false, false);
}

#[test]
fn an_overwritten_successor_does_not_resurrect_the_terminal_baseline() {
    // Goal: a publication gap after handoff cannot resurrect either terminal predecessor.
    // Method: overwrite publications one and two, then read from the sealed handoff.
    let (mut fixture, terminal) = sealed_predecessor();
    for offset in 1..=9 {
        fixture.publish_with_usage(100 + offset, &usage(offset));
    }
    let mut ring = Ring::new(fixture.root.clone());
    ring.accept(Some(terminal));
    ring.follow().unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut observed = Vec::new();

    ring.read(|_, machine, apps| {
        let mut captured = AppsSecond::with_room();
        captured.take(apps, None, Some(&budget));
        observed.push((*machine, captured));
        true
    })
    .unwrap();

    assert_eq!(observed.len(), 8);
    assert_eq!(observed[0].0.publication, 3);
    assert_rates(&observed[0], false, false);
    assert_rates(&observed[1], true, true);
}

fn assert_foreign_terminal(change: impl FnOnce(&mut collector::ring::RunIdentity)) {
    let (fixture, mut terminal) = sealed_predecessor();
    change(&mut terminal.run);
    let mut ring = Ring::new(fixture.root.clone());
    ring.accept(Some(terminal));
    ring.follow().unwrap();
    let source = ring.source.as_ref().unwrap();
    assert!(source.previous.ticks.is_empty());
    assert!(source.previous_frame.is_none());
    assert!(source.previous_endpoint.is_none());
    assert!(ring.terminal.is_none());
}

#[test]
fn another_recordings_terminal_is_rejected() {
    // Goal: recording identity protects both predecessors.
    // Method: alter only the terminal recording identity before opening the actual ring.
    assert_foreign_terminal(|run| run.recording.0[0] ^= 1);
}

#[test]
fn another_runs_terminal_is_rejected() {
    // Goal: collector restart cannot inherit rates from the old run.
    // Method: alter only the terminal run identity before opening the actual ring.
    assert_foreign_terminal(|run| run.run.0[0] ^= 1);
}

#[test]
fn another_boots_terminal_is_rejected() {
    // Goal: boot identity protects both predecessors.
    // Method: alter only the terminal boot identity before opening the actual ring.
    assert_foreign_terminal(|run| run.boot.0[0] ^= 1);
}

#[test]
fn an_exhausted_terminal_publication_is_refused_without_wrapping() {
    // Goal: seeking past the terminal publication cannot wrap to an unrelated ring prefix.
    // Method: give the handoff the largest representable publication and follow the matching ring.
    let (fixture, mut terminal) = sealed_predecessor();
    terminal.machine.ticks[0].publication = u64::MAX;
    let mut ring = Ring::new(fixture.root.clone());
    ring.accept(Some(terminal));

    let result = ring.follow();

    assert_eq!(result, Err(crate::store::Failure::TickShape));
    assert!(ring.source.is_none());
}

#[test]
fn a_refused_terminal_sample_preserves_only_the_machine_predecessor() {
    // Goal: a refused process sample neither erases machine state nor revives an old frame.
    // Method: seal a normal/refused pair and read its adjacent successful successor from the ring.
    let mut fixture = Fixture::new_mode(true, 2_000, None);
    crate::store::tests::expand_program_dictionary(&mut fixture);
    crate::store::tests::publish_named_processes(&mut fixture, 58, &[b"small"], 1);
    crate::store::tests::publish_named_processes(&mut fixture, 59, &[&vec![b'x'; 1_000]], 2_000);
    crate::live::app_process_groups_history_tests::named::seal_selected(&fixture, &[1, 2]);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut captured = Vec::new();
    let outcome = recording::load_payload(&fixture.root, &budget, &operation, |_, _, apps| {
        let mut second = AppsSecond::with_room();
        second.take(apps, None, Some(&budget));
        captured.push(second);
    })
    .unwrap();
    let terminal = outcome.terminal.unwrap();
    assert!(terminal.process.is_none());
    assert_eq!(terminal.machine.ticks.len(), 1);
    assert_eq!(terminal.machine.ticks[0].publication, 2);
    assert_eq!(
        captured[1].processes.as_ref().unwrap_err(),
        &crate::store::Missing::Refused(
            persistence::Error::Bound("live App sample bytes").to_string()
        )
    );
    fixture.start_at_run_tick(60);
    fixture.publish_with_usage(101, &usage(1));
    let mut ring = Ring::new(fixture.root.clone());
    ring.accept(Some(terminal));
    ring.follow().unwrap();
    let mut observed = Vec::new();

    ring.read(|_, machine, apps| {
        let mut second = AppsSecond::with_room();
        second.take(apps, None, Some(&budget));
        observed.push((*machine, second));
        true
    })
    .unwrap();

    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].0.publication, 3);
    assert!(observed[0].0.follows);
    assert_eq!(observed[0].1.processes.as_ref().unwrap().previous, None);
    assert_eq!(observed[0].1.elapsed_ns, None);
}

#[test]
fn cancellation_rejects_a_finished_report_before_installing_its_handoff() {
    // Goal: cancellation between loading and startup acceptance leaves no handoff or View.
    // Method: cancel a successful report, then use the Store's acceptance and publication gates.
    let (fixture, terminal) = sealed_predecessor();
    let cancellation = Arc::new(crate::cancellation::Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let outcome = recording::Outcome {
        left_out: None,
        terminal: Some(terminal),
    };
    let mut held = crate::store::Held::new();
    let mut ring = Ring::new(fixture.root.clone());
    let mailbox = crate::store::Mailbox::new();
    cancellation.cancel().unwrap();

    let result = crate::store::startup_accept(Ok(outcome), &mut held, &mut ring, &operation);
    let published = crate::store::step(
        &mut held,
        &mut ring,
        Ok(()),
        &mailbox,
        Some(&operation),
        crate::store::Publication::Complete,
    );

    assert!(result.is_err());
    assert!(published.is_err());
    assert!(mailbox.take().is_none());
    assert!(ring.terminal.is_none());
    assert!(ring.source.as_ref().unwrap().previous.ticks.is_empty());
    assert!(ring.source.as_ref().unwrap().previous_frame.is_none());
}
