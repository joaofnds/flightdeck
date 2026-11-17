//! The launch's history over several minutes, which it reads and decodes at once.
use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::ThreadId;

use super::*;

#[test]
fn a_history_of_several_minutes_reads_them_beside_its_thread() {
    // Goal: a history of several minutes reads its minutes at once, each on a reader thread of
    // its own, rather than one a step on the store's thread or one after another on one thread.
    // Method: seal six minutes the live span reaches into, so the history holds five, start the
    // launch in groups of six minutes, so the five form one group, name the thread that reads
    // each file while the history installs its minutes, and count the threads.
    let mut run = TurnStage::grouping(history_fixture());
    run.stage.startup.history_group_minutes_max = HISTORY_GROUP_MINUTES_BOUND;
    let reading = Reading::leaked();
    run.stage.startup.capture = reading.capture();
    run.reach_phase(Phase::History);

    reading.on.store(true, Ordering::Release);
    run.install_history();
    reading.on.store(false, Ordering::Release);

    let threads = reading.threads.lock().unwrap();
    assert!(threads.iter().all(|(name, _)| name == "flightdeck-reader"));
    let distinct: HashSet<ThreadId> = threads.iter().map(|(_, id)| *id).collect();
    assert_eq!(distinct.len(), HISTORY_MINUTES_READ);
}

#[test]
fn a_grouped_history_installs_what_a_history_of_one_minute_a_step_installs() {
    // Goal: the history that decodes its minutes at once on threads installs the same machine
    // seconds and Apps as the history that decodes one minute a step, decodes each second once
    // and throws no decoding away, since each guess of a minute's seed holds.
    // Method: launch `history_fixture` grouped under a trace and install its history, then
    // launch the same recording again a minute a step, and compare the seconds each span holds
    // and the work traced.
    let mut grouped = TurnStage::grouping(history_fixture());
    let trace = Trace::new(grouped.fixture.writer.identity().unwrap().recording);
    grouped.reach_phase(Phase::History);
    let scope = trace.enter();
    grouped.install_history();
    drop(scope);
    grouped.assert_pairing();
    assert_history_work(&trace, &grouped, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    assert!(discarded_work(&trace).is_empty());
    let machine = |run: &TurnStage| run.stage.held.span.held().cloned().collect::<Vec<_>>();
    let apps = |run: &TurnStage| run.stage.held.apps.held().cloned().collect::<Vec<_>>();
    let installed = (machine(&grouped), apps(&grouped));

    let mut stepped = TurnStage::grouping(grouped.fixture);
    stepped.stage.startup.history_group_minutes_max = 1;
    stepped.reach_phase(Phase::History);
    stepped.install_history();

    assert_eq!(installed.0, machine(&stepped));
    assert_eq!(installed.1, apps(&stepped));
}

#[test]
fn a_group_the_budget_refuses_goes_on_a_minute_a_step_without_waiting() {
    // Goal: where the budget cannot hold the readers of a group at once, the history goes on at
    // once a minute a step, which the budget holds, rather than wait for memory to come back.
    // Method: launch `history_fixture` in groups of six minutes with a capture that refuses for
    // want of memory each file a reader thread of a group reads, install the history, and check
    // that the launch waited on no capacity and completes.
    let mut run = TurnStage::grouping(history_fixture());
    run.stage.startup.history_group_minutes_max = HISTORY_GROUP_MINUTES_BOUND;
    run.stage.startup.capture = Box::leak(Box::new(|file, budget: &_| {
        if std::thread::current().name() == Some("flightdeck-reader") {
            return Err(AdmissionRefusal::Budget);
        }
        AdmittedTable::from_file_budgeted(file, budget)
    }));
    run.reach_phase(Phase::History);

    run.install_history();

    assert!(run.blocked.is_none());
    assert_eq!(run.stage.startup.history_group_minutes_max, 1);
    run.complete();
}

#[test]
fn a_seal_that_changes_inside_a_group_is_refused_at_its_minute() {
    // Goal: a minute of a group whose seal changed after the launch found it is refused through
    // the launch's witness, as a step of one minute refuses it, and the minutes before it in the
    // group stay installed.
    // Method: launch `history_fixture` in groups of six minutes, so the fourth minute falls in
    // the first group, change its seal once the history begins, turn until the launch fails,
    // and check the failure and the minutes installed.
    let mut run = TurnStage::grouping(history_fixture());
    run.stage.startup.history_group_minutes_max = HISTORY_GROUP_MINUTES_BOUND;
    run.reach_phase(Phase::History);
    let marker = run.fixture.marker(3);
    crate::store::tests::change_saved_manifest(&run.fixture.root, &marker);

    let result = run.try_turn();

    assert!(
        matches!(result, Err(Failure::Read(Refused::Commitment(error)))
        if error.kind() == std::io::ErrorKind::InvalidData)
    );
    assert_eq!(run.stage.startup.history_index, 3);
    assert_eq!(installed_publications(&run), installed_before_minute_3());
    run.assert_pairing();
}

#[test]
fn a_seal_that_changes_after_its_group_installs_fails_the_launch() {
    // Goal: a minute of a group whose seal changes once the group installed it fails the launch
    // before the launch completes, as a minute a step whose seal changes after its install does.
    // Method: launch `history_fixture` in groups of six minutes and install its history, change
    // the seal of its fourth minute, turn once more, and check the failure.
    let mut run = TurnStage::grouping(history_fixture());
    run.stage.startup.history_group_minutes_max = HISTORY_GROUP_MINUTES_BOUND;
    run.reach_phase(Phase::History);
    run.install_history();
    let marker = run.fixture.marker(3);
    crate::store::tests::change_saved_manifest(&run.fixture.root, &marker);

    let result = run.try_turn();

    assert!(
        matches!(result, Err(Failure::Read(Refused::Commitment(error)))
        if error.kind() == std::io::ErrorKind::InvalidData)
    );
    assert!(run.stage.startup.phase == Phase::History);
}

#[test]
fn a_minute_whose_files_are_refused_inside_a_group_fails_as_a_step_of_one_minute_fails() {
    // Goal: a minute of a group whose files cannot be read fails the launch with the refusal a
    // step of one minute gives, and the minutes before it in the group stay installed.
    // Method: launch `history_fixture` in groups of six minutes, so the fourth minute falls in
    // the first group, with a capture that refuses each file of that minute, turn until the
    // launch fails, and check the failure and the minutes installed.
    let mut run = TurnStage::grouping(history_fixture());
    let refused = minute_files(&run.fixture.marker(3));
    run.stage.startup.capture = Box::leak(Box::new(move |file: std::fs::File, budget: &_| {
        let inode = std::os::unix::fs::MetadataExt::ino(&file.metadata().unwrap());
        if refused.contains(&inode) {
            return Err(AdmissionRefusal::Io {
                kind: std::io::ErrorKind::PermissionDenied,
                raw_os_error: None,
            });
        }
        AdmittedTable::from_file_budgeted(file, budget)
    }));
    run.stage.startup.history_group_minutes_max = HISTORY_GROUP_MINUTES_BOUND;
    run.reach_phase(Phase::History);

    let result = run.try_turn();

    assert!(matches!(
        result,
        Err(Failure::Read(Refused::Input(AdmissionRefusal::Io {
            kind: std::io::ErrorKind::PermissionDenied,
            raw_os_error: None,
        })))
    ));
    assert_eq!(run.stage.startup.history_index, 3);
    assert_eq!(installed_publications(&run), installed_before_minute_3());
    run.assert_pairing();
}

#[test]
fn a_ring_the_collector_overruns_during_the_history_misses_its_lost_tick_and_completes() {
    // Goal: where the collector publishes more ticks than the ring holds while the history has
    // minutes left, the launch counts the tick the ring lost as missed and goes on to complete,
    // as a launch that installs one minute a step does, since only a return reloads its hour.
    // Method: launch `history_fixture` to its history, publish nine ticks into the eight-slot
    // ring, complete the launch, and check which of those ticks the span holds.
    let mut run = TurnStage::under(
        history_fixture(),
        OVERRUN_TICKS.len(),
        OVERRUN_TICKS,
        crate::store::BYTES_MAX,
    );
    run.reach_phase(Phase::History);
    for [_, run_tick, wall_second] in &OVERRUN_TICKS[HISTORY_TICKS.len()..] {
        run.fixture
            .publish_with_usage(*wall_second, &usage(*run_tick));
    }

    run.complete();

    let held: Vec<_> = run
        .stage
        .held
        .apps
        .held()
        .map(|apps| apps.observation.unwrap().stamp.publication)
        .filter(|publication| *publication > 12)
        .collect();
    assert_eq!(held, (14..=21).collect::<Vec<_>>());
}

#[test]
fn a_hide_during_the_history_installs_no_minute_and_a_show_goes_on() {
    // Goal: a window hidden while the history has minutes left pauses the launch before it reads
    // a minute, and the launch goes on from the same minute once the window shows again.
    // Method: launch `history_fixture` to its history, hide, turn once, show, then install the
    // history and complete the launch.
    let mut run = TurnStage::grouping(history_fixture());
    run.reach_phase(Phase::History);
    let history_index = run.stage.startup.history_index;
    run.shared.active.store(false, Ordering::Release);

    assert!(!run.turn());
    assert_eq!(run.stage.startup.history_index, history_index);
    assert!(!run.shared.paused.load(Ordering::Acquire));

    run.shared.active.store(true, Ordering::Release);
    run.install_history();
    run.complete();
}

#[test]
fn a_grouped_history_charges_what_a_history_of_one_minute_a_step_charges() {
    // Goal: the history that decodes its minutes at once holds its seconds in the bytes the
    // history of one minute a step holds them in, so a minute's first second shares each
    // reading that repeats the last second of the minute before it, whichever thread decoded it.
    // Method: launch a recording whose every second reads the same in groups of six minutes and
    // install its history, then launch it again a minute a step, and compare the bytes each
    // launch's budget holds and the Apps each span holds.
    let mut grouped = TurnStage::grouping(history_fixture_reading(|_| usage(0)));
    grouped.stage.startup.history_group_minutes_max = HISTORY_GROUP_MINUTES_BOUND;
    grouped.reach_phase(Phase::History);
    grouped.install_history();
    let charged = grouped.budget.used();
    let apps = |run: &TurnStage| run.stage.held.apps.held().cloned().collect::<Vec<_>>();
    let installed = apps(&grouped);

    let mut stepped = TurnStage::grouping(grouped.fixture);
    stepped.stage.startup.history_group_minutes_max = 1;
    stepped.reach_phase(Phase::History);
    stepped.install_history();

    assert_eq!(installed, apps(&stepped));
    assert_eq!(charged, stepped.budget.used());
}

#[test]
fn a_seal_that_changes_while_its_thread_decodes_is_refused_at_its_minute() {
    // Goal: a minute of a group whose seal changes after its reader read it, while its thread
    // decodes it, fails the launch at that minute, before the launch installs a later one, as
    // a step of one minute fails whose seal changes after its read.
    // Method: launch `history_fixture` to its history twice, in groups of six minutes and a
    // minute a step. The grouped launch's threads change the fourth minute's seal as they begin
    // to decode it, once every reader has read its minute, and the stepped launch's capture
    // changes it once its fourth step has read that minute's files. Turn each until it fails,
    // and compare the failure, the minute it stopped at and the seconds it installed.
    let mut grouped = TurnStage::grouping(history_fixture());
    grouped.stage.startup.history_group_minutes_max = HISTORY_GROUP_MINUTES_BOUND;
    grouped.stage.startup.guessing = seal_changing_guess;
    grouped.reach_phase(Phase::History);
    let grouped_result = grouped.try_turn();

    let mut stepped = TurnStage::grouping(history_fixture());
    stepped.stage.startup.history_group_minutes_max = 1;
    stepped.reach_phase(Phase::History);
    for _ in 0..3 {
        assert!(!stepped.turn());
    }
    stepped.stage.startup.capture = seal_changing_capture(&stepped.fixture, 3);
    let stepped_result = stepped.try_turn();

    for result in [grouped_result, stepped_result] {
        assert!(
            matches!(result, Err(Failure::Read(Refused::Commitment(error)))
            if error.kind() == std::io::ErrorKind::InvalidData)
        );
    }
    assert_eq!(grouped.stage.startup.history_index, 3);
    assert_eq!(stepped.stage.startup.history_index, 3);
    assert_eq!(
        installed_publications(&grouped),
        installed_publications(&stepped)
    );
}

#[test]
fn a_group_whose_decoding_the_budget_refuses_goes_on_a_minute_a_step() {
    // Goal: where the budget refuses what a decoder thread of a group holds, the store's thread
    // decodes that minute itself, the launch waits on no capacity, and every later step reads
    // and decodes one minute on the store's thread.
    // Method: launch `history_fixture` in groups of two minutes with decoder threads whose work
    // the budget refuses, turn once, then name the thread that reads each file while the rest of
    // the history installs, and complete the launch.
    let mut run = TurnStage::grouping(history_fixture());
    run.stage.startup.history_group_minutes_max = 2;
    run.stage.startup.guessing = refused_guess;
    let reading = Reading::leaked();
    run.stage.startup.capture = reading.capture();
    run.reach_phase(Phase::History);

    assert!(!run.turn());
    reading.on.store(true, Ordering::Release);
    run.install_history();
    reading.on.store(false, Ordering::Release);

    assert!(run.blocked.is_none());
    let threads = reading.threads.lock().unwrap();
    assert!(!threads.is_empty());
    assert!(threads.iter().all(|(name, _)| name != "flightdeck-reader"));
    run.complete();
}

#[test]
fn a_decoding_begun_from_another_seed_is_thrown_away_and_its_minute_decoded_again() {
    // Goal: a thread's decoding of a minute that began from another seed than the one the
    // minute before it left is thrown away, counted as discarded work, and the minute decoded
    // again on the store's thread, which installs what a history of one minute a step installs.
    // A launch of a real recording seeds each thread from the frame the decoder holds, so the
    // test seeds each thread from the last frame of its own minute.
    // Method: launch `history_fixture` in groups of six minutes under a trace with each thread
    // seeded from its own minute, install the history, then launch the same recording again a
    // minute a step, and compare the seconds each holds and the work traced as discarded.
    let mut grouped = TurnStage::grouping(history_fixture());
    grouped.stage.startup.history_group_minutes_max = HISTORY_GROUP_MINUTES_BOUND;
    grouped.stage.startup.guessing = self_seeded_guess;
    let trace = Trace::new(grouped.fixture.writer.identity().unwrap().recording);
    grouped.reach_phase(Phase::History);
    let scope = trace.enter();
    grouped.install_history();
    drop(scope);
    let apps = |run: &TurnStage| run.stage.held.apps.held().cloned().collect::<Vec<_>>();
    let installed = apps(&grouped);

    let mut stepped = TurnStage::grouping(grouped.fixture);
    stepped.stage.startup.history_group_minutes_max = 1;
    stepped.reach_phase(Phase::History);
    stepped.install_history();

    assert_eq!(installed, apps(&stepped));
    let discarded: HashSet<u64> = discarded_work(&trace)
        .iter()
        .filter(|event| event.kind == Kind::AppDerivation)
        .map(|event| match event.key {
            Key::Publication { publication, .. } => publication,
            _ => panic!("an App derivation is keyed by its publication"),
        })
        .collect();
    assert_eq!(discarded, (3..=10).collect());
}

#[test]
fn a_native_tick_between_two_groups_leaves_what_it_leaves_between_two_minutes() {
    // Goal: a native tick that moves the live span by a second between two steps of a grouped
    // history, the step after a lone last minute included, leaves the launch holding what a
    // launch of one minute a step holds given the same ticks at the same minutes.
    // Method: launch `history_fixture` in groups of two minutes, so its five minutes install in
    // steps of two, two and one, publish a tick before the second and the third step, complete,
    // then launch the same recording made again a minute a step with the ticks before the same
    // minutes, and compare the seconds each span holds, apart from the run that made each.
    let launched = |group_minutes_max: usize| {
        let mut run = TurnStage::under(
            history_fixture(),
            HISTORY_TICKS.len(),
            MOVED_TICKS,
            crate::store::BYTES_MAX,
        );
        run.stage.startup.history_group_minutes_max = group_minutes_max;
        run.reach_phase(Phase::History);
        run.install_history_publishing(&MOVED_TICKS[HISTORY_TICKS.len()..]);
        run.complete();
        run
    };

    let grouped = launched(2);
    let stepped = launched(1);

    assert_eq!(machine_of_any_run(&grouped), machine_of_any_run(&stepped));
    assert_eq!(apps_of_any_run(&grouped), apps_of_any_run(&stepped));
}

#[test]
fn a_grouped_minute_installs_once_the_packing_thread_brought_the_ready_blocks_back() {
    // Goal: a group installs each minute its threads decoded only once the packing thread has
    // brought the ready plain blocks back within `BLOCKS_OUT_MAX`, as a minute a step does, so
    // the launch's plain rows stay inside their bound.
    // Method: seal the minutes of `history_fixture` with each second in thirty wall seconds of
    // its own, so installing a minute readies the blocks of its two seconds and of the second
    // before them, beside a packing thread that answers only after a pause, so a ready block
    // stays plain unless the launch waits for it. Install the history in one group and count
    // the ready plain blocks after each turn, to which the minute the turn installed last adds
    // at most those three.
    let mut run = TurnStage::under(
        history_fixture_of(usage, SPREAD_TICKS),
        SPREAD_TICKS.len(),
        SPREAD_TICKS,
        crate::store::BYTES_MAX,
    );
    run.stage.startup.history_group_minutes_max = HISTORY_GROUP_MINUTES_BOUND;
    let packing = PacedPacking::start(&mut run);
    run.reach_phase(Phase::History);

    let end = run.stage.startup.selected.unwrap();
    for _ in 0..MINUTES_MAX * 2 {
        if run.stage.startup.history_index == end {
            break;
        }
        assert!(!run.turn());
        let ready = run.stage.held.apps.ready_plain_count();
        assert!(ready <= crate::store::packing::BLOCKS_OUT_MAX + 3);
    }
    assert_eq!(run.stage.startup.history_index, end);
    run.complete();
    packing.stop();
}

/// The publication, run tick and wall second of each tick of `history_fixture`.
const HISTORY_TICKS: &[[u64; 3]] = &[
    [1, 0, 1000],
    [2, 59, 1001],
    [3, 60, 1002],
    [4, 119, 1003],
    [5, 120, 1004],
    [6, 179, 1005],
    [7, 180, 1006],
    [8, 239, 1007],
    [9, 240, 1008],
    [10, 299, 1009],
    [11, 300, 1010],
    [12, 359, 1011],
];

/// The ticks of `history_fixture` and the nine the collector publishes after them, one more
/// than the ring holds.
const OVERRUN_TICKS: &[[u64; 3]] = &[
    [1, 0, 1000],
    [2, 59, 1001],
    [3, 60, 1002],
    [4, 119, 1003],
    [5, 120, 1004],
    [6, 179, 1005],
    [7, 180, 1006],
    [8, 239, 1007],
    [9, 240, 1008],
    [10, 299, 1009],
    [11, 300, 1010],
    [12, 359, 1011],
    [13, 360, 1012],
    [14, 361, 1013],
    [15, 362, 1014],
    [16, 363, 1015],
    [17, 364, 1016],
    [18, 365, 1017],
    [19, 366, 1018],
    [20, 367, 1019],
    [21, 368, 1020],
];

/// The ticks of `history_fixture` and the two the collector publishes during its history, which
/// move the live span by a second each.
const MOVED_TICKS: &[[u64; 3]] = &[
    [1, 0, 1000],
    [2, 59, 1001],
    [3, 60, 1002],
    [4, 119, 1003],
    [5, 120, 1004],
    [6, 179, 1005],
    [7, 180, 1006],
    [8, 239, 1007],
    [9, 240, 1008],
    [10, 299, 1009],
    [11, 300, 1010],
    [12, 359, 1011],
    [13, 360, 1012],
    [14, 361, 1013],
];

/// The minutes `history_fixture` seals.
const HISTORY_MINUTES: u64 = 6;

/// The minutes of `history_fixture` its history reads, every one but the minute the launch
/// selects to show first.
const HISTORY_MINUTES_READ: usize = 5;

/// The publications of `history_fixture` the launch installs before its history, those of the
/// minute it selects to show first.
const SELECTED_PUBLICATIONS: [u64; 2] = [11, 12];

/// Six minutes of one run, each holding its first and its last second, so each minute's first
/// second follows the last second of the minute before it and takes its rates from it. A live
/// span of every second reaches into the oldest minute, so the history holds five minutes. The
/// wall seconds follow one another, so the span closes no block the launch waits to see packed,
/// since no packing thread runs beside the test.
fn history_fixture() -> Fixture {
    history_fixture_reading(usage)
}

/// The minutes of `history_fixture`, each second of which reads `reading` of its run tick.
fn history_fixture_reading(reading: fn(u64) -> collector::processes::Usage) -> Fixture {
    history_fixture_of(reading, HISTORY_TICKS)
}

/// The minutes of `history_fixture`, each second of which reads `reading` of its run tick and
/// stands at the wall second `ticks` gives it.
fn history_fixture_of(
    reading: fn(u64) -> collector::processes::Usage,
    ticks: &[[u64; 3]],
) -> Fixture {
    let mut fixture = Fixture::new();
    for (minute, [first, last]) in (0..HISTORY_MINUTES).zip(ticks.as_chunks::<2>().0) {
        assert_eq!(first[1], minute * 60);
        assert_eq!(last[1], minute * 60 + 59);
        fixture.publish_with_usage(first[2], &reading(first[1]));
        fixture.skip_run_ticks(58);
        fixture.publish_with_usage(last[2], &reading(last[1]));
        fixture.seal_minute(minute);
    }
    fixture
}

/// The ticks of `history_fixture` at the wall seconds their run ticks give, so each second
/// falls in thirty wall seconds of its own and closes the block before it.
const SPREAD_TICKS: &[[u64; 3]] = &[
    [1, 0, 990],
    [2, 59, 1049],
    [3, 60, 1050],
    [4, 119, 1109],
    [5, 120, 1110],
    [6, 179, 1169],
    [7, 180, 1170],
    [8, 239, 1229],
    [9, 240, 1230],
    [10, 299, 1289],
    [11, 300, 1290],
    [12, 359, 1349],
];

/// How long the packing thread of `PacedPacking` waits before it answers what the store's
/// thread handed it, far longer than a group takes to install its minutes, so the blocks a
/// group readies wait to be packed unless the launch waits for them.
const PACKING_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

/// A packing thread that answers what the store's thread handed it only after a pause, until
/// the test stops it.
struct PacedPacking {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl PacedPacking {
    /// Gives `run` a span of Apps whose blocks this thread packs.
    fn start(run: &mut TurnStage) -> Self {
        let capacity = run.ticks.len();
        let (store_end, mut packing_end) = crate::store::packing::pair(
            Arc::clone(&run.budget),
            crate::store::payload::packed::APP_ROWS_PACKED_ALWAYS,
        );
        let buffers = Buffers::new(
            2 * (capacity + crate::store::block::BLOCK_DEPARTED_MAX),
            AppsSecond::default,
        );
        run.stage.held.apps = crate::store::block::BlockSpan::new(capacity, buffers, store_end);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            // A launch of `history_fixture` takes far fewer pauses than this.
            for _ in 0..10_000 {
                if stopped.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(PACKING_PAUSE);
                packing_end.answer_handed();
            }
            panic!("the test did not stop its packing thread");
        });

        Self { stop, thread }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Release);
        self.thread.join().unwrap();
    }
}

impl TurnStage {
    /// A launch of `fixture`, which `history_fixture` makes, under the store's own budget, since
    /// the readers of a group reserve their scratch memory together.
    fn grouping(fixture: Fixture) -> Self {
        Self::under(
            fixture,
            HISTORY_TICKS.len(),
            HISTORY_TICKS,
            crate::store::BYTES_MAX,
        )
    }

    /// Turns the history until it installed every minute before the selected one, which leaves
    /// it the step that certifies the launch.
    fn install_history(&mut self) {
        assert!(self.stage.startup.phase == Phase::History);
        let end = self.stage.startup.selected.unwrap();
        for _ in 0..MINUTES_MAX * 2 {
            if self.stage.startup.history_index == end {
                return;
            }
            assert!(!self.turn());
        }
        panic!("the history did not install its minutes");
    }

    /// Turns the history as `install_history` does, publishing each of `ticks` before the step
    /// that begins at the second minute, then the fourth, which a group of two minutes begins.
    fn install_history_publishing(&mut self, ticks: &[[u64; 3]]) {
        let mut ticks = [2, 4].into_iter().zip(ticks).peekable();
        assert!(self.stage.startup.phase == Phase::History);
        let end = self.stage.startup.selected.unwrap();
        for _ in 0..MINUTES_MAX * 2 {
            if self.stage.startup.history_index == end {
                assert!(ticks.peek().is_none());
                return;
            }
            if let Some((_, [_, run_tick, wall_second])) =
                ticks.next_if(|(index, _)| *index == self.stage.startup.history_index)
            {
                self.fixture
                    .publish_with_usage(*wall_second, &usage(*run_tick));
            }
            assert!(!self.turn());
        }
        panic!("the history did not install its minutes");
    }
}

/// The machine seconds `run`'s span holds, apart from the run that read them.
fn machine_of_any_run(run: &TurnStage) -> Vec<MachineSecond> {
    let mut machine: Vec<MachineSecond> = run
        .stage
        .held
        .span
        .held()
        .map(|second| (**second).clone())
        .collect();
    for second in &mut machine {
        second.run = [0; 2];
    }
    machine
}

/// What each second's Apps of `run`'s span hold that does not name the run that read them.
fn apps_of_any_run(run: &TurnStage) -> Vec<AppsOfAnyRun<'_>> {
    run.stage
        .held
        .apps
        .held()
        .map(|apps| {
            (
                apps.observation.unwrap().stamp.publication,
                apps.consecutive,
                apps.elapsed_ns,
                apps.missing.as_ref(),
                &apps.apps,
                apps.processes.as_ref().map(|payload| {
                    (
                        payload.previous.map(|stamp| stamp.publication),
                        payload.processes().collect(),
                        payload.apps().collect(),
                    )
                }),
            )
        })
        .collect()
}

/// A second's publication, whether it follows the second before it, its elapsed time, why it
/// holds no Apps, its Apps, and of its processes the publication of the second they follow,
/// each process and each App's additional measures.
type AppsOfAnyRun<'a> = (
    u64,
    bool,
    Option<u64>,
    Option<&'a Missing>,
    &'a crate::store::AppRows,
    Result<PayloadOfAnyRun<'a>, &'a Missing>,
);

/// What a second's processes hold that does not name the run that read them.
type PayloadOfAnyRun<'a> = (
    Option<u64>,
    Vec<crate::store::ProcessAt<'a>>,
    Vec<crate::store::AppDetails>,
);

/// The publications of the seconds of the live span whose Apps the launch installed.
fn installed_publications(run: &TurnStage) -> Vec<u64> {
    run.stage
        .held
        .apps
        .held()
        .filter(|apps| apps.missing != Some(Missing::Unread))
        .map(|apps| apps.observation.unwrap().stamp.publication)
        .collect()
}

/// The publications `installed_publications` gives once the history of `history_fixture`
/// installed its first three minutes: those minutes' and the selected minute's.
fn installed_before_minute_3() -> Vec<u64> {
    (1..=6).chain(SELECTED_PUBLICATIONS).collect()
}

/// The files a step reads of a minute of `history_fixture`, the tables of its processes.
const MINUTE_FILES_READ: usize = 2;

/// A capture that changes the seal of `fixture`'s minute at `minute` once it has read every
/// file a step reads of that minute, so after the minute's read and before its check.
fn seal_changing_capture(fixture: &Fixture, minute: u64) -> &'static InputCapture<'static> {
    let root = fixture.root.clone();
    let marker = fixture.marker(minute);
    let files = minute_files(&marker);
    let read = Mutex::new(HashSet::with_capacity(MINUTE_FILES_READ));
    Box::leak(Box::new(move |file: std::fs::File, budget: &_| {
        let inode = std::os::unix::fs::MetadataExt::ino(&file.metadata().unwrap());
        let input = AdmittedTable::from_file_budgeted(file, budget)?;
        let mut read = read.lock().unwrap();
        if files.contains(&inode) && read.insert(inode) && read.len() == MINUTE_FILES_READ {
            crate::store::tests::change_saved_manifest(&root, &marker);
        }
        Ok(input)
    }))
}

/// A decoder thread's work, which first changes the seal of the fourth minute of
/// `history_fixture` where that is the minute `read` read, after every reader has read its
/// minute.
fn seal_changing_guess(
    input: &Input<'_>,
    before: &Result<HistoryRead<'_>, Failure>,
    read: &Result<HistoryRead<'_>, Failure>,
    recent: &[Option<Observation>],
) -> Option<HistoryGuess> {
    let sealed = &read.as_ref().unwrap().sealed;
    let first = sealed.endpoints().unwrap()[0].observation();
    if first.stamp.publication == 7 {
        let marker =
            crate::live::identity_fixture::saved_commitment(input.root, sealed.identity(), 3);
        crate::store::tests::change_saved_manifest(input.root, &marker);
    }
    history_group_guess(input, before, read, recent)
}

/// A decoder thread's work the budget refused, where both minutes were read, as
/// `history_group_guess` decodes only then.
fn refused_guess(
    _: &Input<'_>,
    before: &Result<HistoryRead<'_>, Failure>,
    read: &Result<HistoryRead<'_>, Failure>,
    _: &[Option<Observation>],
) -> Option<HistoryGuess> {
    let (Ok(_), Ok(_)) = (before, read) else {
        return None;
    };
    let (scope, tentative) = persistence::work::ingestion::tentative();
    drop(scope);
    Some((tentative, Err(Failure::Blocked)))
}

/// A decoder thread's work seeded from the last frame of the minute it decodes rather than of
/// the minute before it.
fn self_seeded_guess(
    input: &Input<'_>,
    _: &Result<HistoryRead<'_>, Failure>,
    read: &Result<HistoryRead<'_>, Failure>,
    recent: &[Option<Observation>],
) -> Option<HistoryGuess> {
    history_group_guess(input, read, read, recent)
}

/// The name and the id of the thread that read each file of the launch while `on` held.
struct Reading {
    on: AtomicBool,
    threads: Mutex<Vec<(String, ThreadId)>>,
}

impl Reading {
    /// A `Reading` for as long as the test runs, since the launch keeps its capture for as long
    /// as it runs.
    fn leaked() -> &'static Self {
        Box::leak(Box::new(Self {
            on: AtomicBool::new(false),
            threads: Mutex::new(Vec::new()),
        }))
    }

    fn capture(&'static self) -> &'static InputCapture<'static> {
        Box::leak(Box::new(move |file, budget: &Arc<ByteBudget>| {
            if self.on.load(Ordering::Acquire) {
                let thread = std::thread::current();
                let name = thread.name().unwrap_or_default().to_owned();
                self.threads.lock().unwrap().push((name, thread.id()));
            }
            AdmittedTable::from_file_budgeted(file, budget)
        }))
    }
}

/// The work `trace` counts as thrown away.
fn discarded_work(trace: &Trace) -> Vec<Event> {
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    events.retain(|event| event.purpose == Purpose::Discarded);
    events
}

/// The inodes of the files under the directory that holds `marker`, the commitment of a minute.
fn minute_files(marker: &Path) -> Vec<u64> {
    const ENTRIES_MAX: usize = 256;
    let mut directories = vec![marker.parent().unwrap().to_owned()];
    let mut inodes = Vec::new();
    for _ in 0..ENTRIES_MAX {
        let Some(directory) = directories.pop() else {
            return inodes;
        };
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                directories.push(entry.path());
            } else {
                inodes.push(std::os::unix::fs::MetadataExt::ino(&metadata));
            }
        }
    }
    panic!("a minute holds more than {ENTRIES_MAX} directories");
}
