//! Opt-in measurement of what the live store costs on a recording: each read of its span, taking a
//! second, loading the hour, a launch's load by stage, the load of a return after a gap, decoding
//! the Apps a launch owes its stretches and the bytes its rows take. A change to how the store
//! holds its seconds runs it before and after, and every digest must come out the same. It builds
//! only with the `read-benchmark` feature, and `just read-benchmark <a recording>` runs each test
//! in a process of its own, since several of them read the process's footprint.
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use persistence::engine::{THREAD_DEFAULTS, ThreadCounts};
use persistence::resources::Footprint;
use serde_json::Value;

use super::query::{CheckedCoverage, Queries};
use super::recording::staged::{Advance, Input, Startup};
use super::recording::{self, Continuity};
use super::{
    AppDetails, AppRow, AppsCursor, AppsSecond, AppsStretch, ByteBudget, COHORT_MAX, ChartPoints,
    Cohorts, CompleteMachineState, ContextRanks, Held, KeptPlain, Leaders, MachineSecond, Mailbox,
    Newest, Payload, ProcessAt, ProcessRow, QueryContext, QueryOutcome, QueryRequest,
    QuerySelection, Readings, Response, RunIdentity, SECONDS_HELD_MAX, SECONDS_MAX, SampledSecond,
    SourceGeneration, TickApps, View, WallSeconds, coverage,
};
use crate::app_process_groups_history::{
    AppProcessGroupsHistory, ProcessGroup, ProcessGroupsPoint,
};
use crate::cancellation::Cancellation;
use crate::live_app_response::{
    AppIdentity, METRICS, Page, Rank, Rankable, RankedApp, Ranking, Ranks,
};
use crate::session::ContextRange;
use crate::{AppAttributionMetric, WindowRanks};

/// The times each read runs, of which the report gives the median and the highest.
const RUNS: usize = 100;
/// The engine threads the app runs with, which the loads are timed on.
const APP_THREAD_COUNTS: ThreadCounts = ThreadCounts {
    maximum: 6,
    ..THREAD_DEFAULTS
};
/// How far back the earlier second that is ranked lies, in a span twice as long or longer.
const SECONDS_BACK: usize = 600;
/// How far back the second a fixed context selects lies, near enough that the span still
/// holds every second the context charts.
const FIXED_SECONDS_BACK: u64 = 60;
/// The rows a page asks for.
const PAGE_ROWS: u32 = 100;
/// The seconds a continued load takes, those of the minutes one load may group, where the
/// recording holds three times as many.
const CONTINUED_SECONDS: u64 = 360;
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
const SECONDS_PER_MINUTE: u64 = 60;
/// The minutes a window stays hidden before a return, up to the hour a store follows, each the
/// gap between the newest second the store holds and the recording's newest one.
const RETURN_GAP_MINUTES: [u64; 3] = [20, 40, 59];
/// The steps a launch takes at most: a few for each sealed minute it may read.
const LAUNCH_STEPS_MAX: usize = recording::MINUTES_MAX * 4;

#[test]
fn reads_of_the_live_span_cost() {
    // Goal: say what each read of a View costs on a real recording's newest hour.
    // Method: load the hour's sealed minutes as the store does, publish a View and time each
    // read the charts and the rankings make of it.
    let mut loaded = load_hour_settled(&prepared_root());
    let view = publish(&mut loaded.held);
    let cohort = leading_apps(&view);
    println!(
        "span of {} seconds and {} stretches, {} Apps at the newest, a cohort of {}",
        view.apps.len(),
        view.stretches.len(),
        view.apps.newest().unwrap().app_count(),
        cohort.len(),
    );

    time_ranks(&view);
    time_leaders(&view);
    time_charts(&view, &cohort);
}

#[test]
fn reads_of_a_context_that_follows_the_newest_second_cost() {
    // Goal: say what building a query context that follows the newest second costs, and each
    // read the pages make of it.
    // Method: load the hour as the store does, serve the query as the store's thread does and
    // time each read of the context it answers with.
    let mut loaded = load_hour_settled(&prepared_root());

    time_context("followed", &mut loaded, QuerySelection::FollowLatest);
}

#[test]
fn reads_of_a_context_fixed_on_an_earlier_second_cost() {
    // Goal: say what building a query context fixed on an earlier second costs, and each read
    // the pages make of it.
    // Method: load the hour as the store does, serve a query of a second a minute back as the
    // store's thread does and time each read of the context it answers with.
    let mut loaded = load_hour_settled(&prepared_root());
    let fixed = QuerySelection::Second {
        second: newest_wall_second(&loaded.held) - FIXED_SECONDS_BACK,
        alternative: 0,
    };

    time_context("fixed", &mut loaded, fixed);
}

#[test]
fn taking_a_second_costs() {
    // Goal: say what taking one second into the span and publishing a View of it costs, with
    // every earlier View let go and with one View held through every take reported, as a hold
    // keeps one.
    // Method: load the hour as the store does, with the packing thread beside it, wait untimed
    // before each take until the packing thread has answered what it was handed, publish after
    // every take and time the two together, over the takes that fill the span the store ends
    // with, and say apart what packing a block took the packing thread.
    let root = prepared_root();

    let takes = timed_takes(&root, None);
    let reported_from = takes_reported_from(takes.elapsed_ns.len());
    assert!(
        takes.elapsed_ns.len() - reported_from > SECONDS_HELD_MAX,
        "the recording holds too few seconds for a View to stay held past the store's spare \
         buffers"
    );
    report_takes("take a second and publish", &takes, reported_from);
    drop(takes);
    let takes_held = timed_takes(&root, Some(reported_from));
    report_takes(
        "take a second and publish, an earlier View held",
        &takes_held,
        reported_from,
    );
}

#[test]
fn loading_the_hour_costs() {
    // Goal: say what loading the newest hour costs in time and in memory.
    // Method: load the hour's sealed minutes as the store does, once in this process, with the
    // packing thread beside the load as in the app, and read the process's footprint and the
    // store's budget once the packing thread has packed what the load left.
    let root = prepared_root();

    let started = Instant::now();
    let mut loaded = load_hour(&root, take_every_second);
    let elapsed = started.elapsed();
    loaded.held.apps.settle();
    let memory = Memory::read(&loaded.budget);

    println!("load of the hour: {} ns", elapsed.as_nanos());
    report_memory("once the hour is loaded", &memory);
    report_span("the loaded span", &publish(&mut loaded.held));
}

#[test]
fn launching_costs() {
    // Goal: say what a launch costs in time and in memory before the store has its first View to
    // hand over, and from there until it holds the Apps of every second it holds whole.
    // Method: step the launch's staged load over the hour's sealed minutes as the store's thread
    // steps it, with the packing thread beside it and a ring that follows no collector, time the
    // steps up to the first View and the steps after it, and read the process's footprint and
    // the store's budget at both ends, the second once the packing thread has packed what the
    // launch gave it. The Apps the launch then owes its stretches are what
    // `decoding_the_owed_apps_costs` times.
    let root = prepared_root();
    let budget = Arc::new(ByteBudget::new(super::BYTES_MAX));
    let (store_end, packing_end) = super::packing::pair(
        Arc::clone(&budget),
        super::payload::packed::APP_ROWS_PACKED_SHARE,
    );
    drop(packing_end.start(std::thread::current()).unwrap());
    let mut held = Held::new(root.clone(), Arc::clone(&budget), store_end);
    held.startup = super::StartupState::Loading;
    let mut ring = super::Ring::new(root.clone());
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let input = Input {
        root: &root,
        budget: &budget,
        operation: &operation,
    };
    let mut startup = Startup::new();

    let started = Instant::now();
    let first_steps = launch_steps(&mut startup, &input, &mut held, &mut ring);
    startup.primary(&mut held).unwrap();
    let first_view = publish(&mut held);
    let first_elapsed = started.elapsed();
    report_span("the span of the first View", &first_view);
    drop(first_view);
    report_memory("at the first View", &Memory::read(&budget));
    let resumed = Instant::now();
    let later_steps = launch_steps(&mut startup, &input, &mut held, &mut ring);
    let later_elapsed = resumed.elapsed();
    held.apps.settle();
    report_memory("once the launch is loaded", &Memory::read(&budget));

    println!(
        "launch to the first View: {} ns in {} steps",
        first_elapsed.as_nanos(),
        first_steps.len(),
    );
    println!(
        "launch from the first View until the Apps of the seconds held whole are loaded: {} ns \
         in {} steps",
        later_elapsed.as_nanos(),
        later_steps.len(),
    );
    println!("steps to the first View, in nanoseconds: {first_steps:?}");
    println!("steps after the first View, in nanoseconds: {later_steps:?}");
    report_span("the span once the launch is loaded", &publish(&mut held));
    launching_costs_owed(&root, &mut held);
}

/// Adds the Apps the launch owes the stretches of `held`, as the store does once the window has
/// taken the launch's first View, and reports what that took and what the charts then cost.
fn launching_costs_owed(root: &Path, held: &mut Held) {
    let owed = held.owed.take().unwrap();
    let resumed = Instant::now();
    super::run_owed_add(root, held, &owed);
    println!(
        "adding the Apps the launch owes its stretches: {} ns",
        resumed.elapsed().as_nanos(),
    );
    let view = publish(held);
    report_span("the span once the owed Apps are added", &view);
    println!(
        "stretches once the owed Apps are added: {}",
        view.stretches.len()
    );
    time_leaders(&view);
    time_charts(&view, &leading_apps(&view));
}

#[test]
fn continuing_a_load_costs() {
    // Goal: say what a load that continues from the store's newest second costs in time and in
    // memory, when the recording holds six minutes the store has not taken, or a third of a
    // recording shorter than eighteen.
    // Method: load the hour but for those newest minutes, then load from the newest second
    // held as the store's refresh does, and read the footprint before and after, each once
    // the packing thread has packed what the load before it left.
    let root = prepared_root();
    let cutoff_ns = continued_cutoff_ns(&root);
    let mut loaded = load_hour(&root, |held, run, second, apps| {
        take_through(cutoff_ns, held, run, second, apps);
    });
    loaded.held.apps.settle();
    let before = Memory::read(&loaded.budget);

    let started = Instant::now();
    let taken = continue_load(&root, &mut loaded);
    let elapsed = started.elapsed();
    loaded.held.apps.settle();
    let after = Memory::read(&loaded.budget);

    println!(
        "continued load: {} ns, {taken} seconds handed over",
        elapsed.as_nanos(),
    );
    report_memory("before the continued load", &before);
    report_memory("after the continued load", &after);
    report_span("the loaded span", &publish(&mut loaded.held));
}

#[test]
fn returning_after_a_gap_costs() {
    // Goal: say what the load a store runs when its window comes back costs in time, after the
    // window was hidden for 20, 40 or 59 minutes, or for as long as the recording allows, and
    // what the store holds once that load is done, so a change to the return can be compared
    // with it figure for figure.
    // Method: for each gap, load the hour but for its seconds, then load from the newest second
    // held as the store's refresh does, with no ring, and report the time, the seconds handed
    // over and the digests of the span the store then holds.
    let root = prepared_root();
    let (oldest_ns, newest_ns) = recorded_wall_ns(&root);
    // The seconds floor, which a gap of a second less does not show.
    let recorded_seconds = (newest_ns - oldest_ns) / NANOSECONDS_PER_SECOND;
    assert!(
        recorded_seconds > SECONDS_PER_MINUTE,
        "the recording holds no minute before a gap"
    );

    for gap_minutes in RETURN_GAP_MINUTES {
        // The load before the gap keeps a minute, so the store holds a second to continue from.
        let gap_seconds =
            (gap_minutes * SECONDS_PER_MINUTE).min(recorded_seconds - SECONDS_PER_MINUTE);
        let cutoff_ns = newest_ns - gap_seconds * NANOSECONDS_PER_SECOND;
        let mut loaded = load_hour(&root, |held, run, second, apps| {
            take_through(cutoff_ns, held, run, second, apps);
        });
        loaded.held.apps.settle();

        let started = Instant::now();
        let since = load_since(&root, &mut loaded);
        let elapsed = started.elapsed();
        loaded.held.apps.settle();

        report_return(
            &format!("after a gap of {gap_seconds} seconds"),
            elapsed,
            &since,
            loaded,
        );
    }
}

#[test]
fn returning_after_an_hour_costs() {
    // Goal: say what the load a store runs costs in time and in memory when its window comes back
    // after an hour or more, so that the loaded hour no longer holds the store's newest second
    // and the load replaces what the store holds, and what the store holds once it is done.
    // Method: load the hour, which stands for the hour the store held when its window hid, and
    // name as its newest second one from before the run's first sealed minute. Then load from
    // that second as the store's refresh does, with no ring, once in this process, and report
    // the time, the memory, the seconds handed over and the digests of the span the store holds.
    let root = prepared_root();
    let mut loaded = load_hour_settled(&root);
    let run = loaded.held.newest.unwrap().run;
    loaded.held.newest = Some(Newest {
        run,
        publication: 0,
        segment: 0,
    });

    let started = Instant::now();
    let since = load_since(&root, &mut loaded);
    let elapsed = started.elapsed();
    loaded.held.apps.settle();
    let memory = Memory::read(&loaded.budget);

    assert!(
        since.replaced,
        "the load continued the seconds held, which times no return after an hour"
    );
    report_memory("once the return after an hour is loaded", &memory);
    report_return("after an hour", elapsed, &since, loaded);
}

/// Says what a return's load, which took `elapsed` and handed over `since`, cost, and the digests
/// of the span `loaded` then holds, naming the return `label`.
fn report_return(label: &str, elapsed: std::time::Duration, since: &Since, mut loaded: Loaded) {
    println!(
        "return {label}: {} ns, {} seconds handed over, replaced the seconds held: {}, left out: \
         {:?}",
        elapsed.as_nanos(),
        since.taken,
        since.replaced,
        loaded.held.left_out,
    );
    let view = publish(&mut loaded.held);
    report_span(&format!("the span {label}"), &view);
    println!("stretches {label}: {}", view.stretches.len());
}

#[test]
fn decoding_the_owed_apps_costs() {
    // Goal: say what the loaders cost in time and in memory in the pass the store runs once the
    // window has its first View, which adds up the Apps a launch owes the store's stretches.
    // Method: decode the Apps of the hour's sealed minutes on the pass's loaders, in a process
    // that holds nothing else, and read the footprint before and after, so the peak's rise is
    // what the loaders held at once. No store takes what they hand over, so the time and the
    // memory of adding it to the store's stretches are left out. The parts counted are no
    // digest: a stretch two loaders decode comes in two parts, so the count follows the number
    // of loaders, and a recording whose minutes keep no App parts hands over none.
    let root = prepared_root();
    let owed = recording::Owed::of_hour(&root);
    let before = Footprint::read().unwrap();
    let mut part_count = 0_usize;

    let started = Instant::now();
    let left_out = recording::load_owed(&root, &owed, |_, _, apps| {
        if let TickApps::Folded(_) = apps {
            part_count += 1;
        }
    });
    let elapsed = started.elapsed();
    let after = Footprint::read().unwrap();

    assert!(left_out.is_none());
    println!(
        "decoding the owed Apps: {} ns, {part_count} parts of stretches handed over",
        elapsed.as_nanos(),
    );
    println!(
        "before the owed Apps are decoded: footprint {} bytes, footprint peak {} bytes",
        before.current_bytes, before.lifetime_bytes_max,
    );
    println!(
        "once the owed Apps are decoded: footprint {} bytes, footprint peak {} bytes",
        after.current_bytes, after.lifetime_bytes_max,
    );
}

#[test]
fn rows_of_the_live_span_repeat() {
    // Goal: say how many bytes the span's rows take, how many of its process rows and App
    // details the store shares between seconds, and how many process rows the store holds apart
    // from the row of the second before although they read the same and their App index moved.
    // Method: load the hour as the store does and compare each process's row with its row of
    // the second before, by address and then field by field.
    let mut loaded = load_hour_settled(&prepared_root());
    let view = publish(&mut loaded.held);

    let app_bytes = report_apps(&view);
    let details_bytes = report_details(&view);
    let process_bytes = report_processes(&view);
    println!(
        "rows of the span as held: {} bytes",
        app_bytes + details_bytes + process_bytes,
    );
}

/// What a load left behind: the seconds held and the budget their rows are charged to.
struct Loaded {
    held: Held,
    budget: Arc<ByteBudget>,
}

/// The recording to read, with the engine that reads it ready as the app readies it.
fn prepared_root() -> PathBuf {
    let optimized = !cfg!(debug_assertions);
    assert!(
        optimized,
        "the benchmark times the build people use: cargo test --release"
    );
    persistence::engine::initialize_process_with(APP_THREAD_COUNTS).unwrap();
    let root = std::env::var_os("FLIGHTDECK_REFERENCE_RECORDING")
        .expect("FLIGHTDECK_REFERENCE_RECORDING names the recording to read");

    PathBuf::from(root)
}

/// Loads the newest hour of `root` as the store does, handing each second to `take`, with the
/// packing thread started as the store's thread starts it. The load waits for nothing, and
/// the packing thread ends once the store's end goes with what the store holds.
fn load_hour(
    root: &Path,
    mut take: impl FnMut(&mut Held, &RunIdentity, &MachineSecond, TickApps<'_>),
) -> Loaded {
    let budget = Arc::new(ByteBudget::new(super::BYTES_MAX));
    let (store_end, packing_end) = super::packing::pair(
        Arc::clone(&budget),
        super::payload::packed::APP_ROWS_PACKED_SHARE,
    );
    drop(packing_end.start(std::thread::current()).unwrap());
    let mut held = Held::new(root.to_path_buf(), Arc::clone(&budget), store_end);
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut handed_over = 0_usize;
    let outcome =
        recording::load_payload_hour(root, &budget, &operation, None, |run, second, apps| {
            handed_over += 1;
            take(&mut held, run, second, apps);
        })
        .unwrap();
    assert!(
        handed_over > 0,
        "the load handed over no second, as when the recording's retention period has run out"
    );
    held.left_out = outcome.left_out;
    assert!(held.left_out.is_none());
    coverage::accept_catalog(
        &mut held,
        outcome.checked_catalog.or(outcome.continued_catalog),
    )
    .unwrap();
    held.ring_complete = true;

    Loaded { held, budget }
}

/// Steps `startup` as the store's thread steps a launch, until a step says the first View is due
/// or the launch is loaded, and returns what each step took, in nanoseconds.
fn launch_steps(
    startup: &mut Startup,
    input: &Input<'_>,
    held: &mut Held,
    ring: &mut super::Ring,
) -> Vec<u128> {
    let mut steps = Vec::with_capacity(LAUNCH_STEPS_MAX);
    for _ in 0..LAUNCH_STEPS_MAX {
        let started = Instant::now();
        let advance = startup.advance(input, held, ring).unwrap();
        held.apps.exchange();
        steps.push(started.elapsed().as_nanos());
        match advance {
            Advance::Progress => {}
            // The packing thread wakes this one, as it wakes the store's.
            Advance::Blocked => std::thread::park_timeout(super::WAIT),
            Advance::Primary | Advance::Complete => return steps,
        }
    }
    panic!("the launch took more steps than its minutes allow");
}

/// Loads the newest hour of `root` taking every second, once the packing thread has packed
/// what the load left, as the running app holds it a few seconds after a load.
fn load_hour_settled(root: &Path) -> Loaded {
    let mut loaded = load_hour(root, take_every_second);
    loaded.held.apps.settle();

    loaded
}

fn take_every_second(
    held: &mut Held,
    run: &RunIdentity,
    second: &MachineSecond,
    apps: TickApps<'_>,
) {
    assert!(held.take(run, second, apps));
}

/// Takes `second` where it is no later than `cutoff_ns`, which leaves the seconds past it for
/// a continued load.
fn take_through(
    cutoff_ns: u64,
    held: &mut Held,
    run: &RunIdentity,
    second: &MachineSecond,
    apps: TickApps<'_>,
) {
    if second.wall_ns <= cutoff_ns {
        assert!(held.take(run, second, apps));
    }
}

/// The wall time of the last second a load takes before the continued one, which leaves that
/// load `CONTINUED_SECONDS` of the hour of `root`, or a third of an hour shorter than three
/// times that.
fn continued_cutoff_ns(root: &Path) -> u64 {
    let (oldest_ns, newest_ns) = recorded_wall_ns(root);
    // The seconds and the third of them floor, which a continued load of a second less does
    // not show.
    let recorded_seconds = (newest_ns - oldest_ns) / NANOSECONDS_PER_SECOND;
    let continued_seconds = CONTINUED_SECONDS.min(recorded_seconds / 3);

    newest_ns - continued_seconds * NANOSECONDS_PER_SECOND
}

/// The wall times of the oldest and the newest second of the hour of `root`.
fn recorded_wall_ns(root: &Path) -> (u64, u64) {
    let (mut oldest_ns, mut newest_ns) = (u64::MAX, 0);
    drop(load_hour(root, |_, _, second, _| {
        oldest_ns = oldest_ns.min(second.wall_ns);
        newest_ns = newest_ns.max(second.wall_ns);
    }));
    assert!(oldest_ns <= newest_ns);

    (oldest_ns, newest_ns)
}

/// Loads what `root` holds past the newest second of `loaded`, as the store's refresh does,
/// and says how many seconds the load handed over.
fn continue_load(root: &Path, loaded: &mut Loaded) -> usize {
    let since = load_since(root, loaded);
    assert!(
        !since.replaced,
        "the load replaced the seconds held, which times no continued load"
    );

    since.taken
}

/// What a load from the store's newest second handed over.
struct Since {
    taken: usize,
    /// Whether the recording no longer held the newest second, so the seconds the load handed
    /// over replaced those the store held.
    replaced: bool,
}

/// Loads what `root` holds past the newest second of `loaded` as the store's refresh does where
/// it holds every minute of the hour, which resets the store where the load replaces its seconds.
fn load_since(root: &Path, loaded: &mut Loaded) -> Since {
    let since = loaded.held.newest;
    let held = &mut loaded.held;
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let (mut taken, mut replaced) = (0_usize, false);
    let outcome = recording::load_payload_since(
        root,
        &loaded.budget,
        &operation,
        since.as_ref(),
        |continuity, run, second, apps| {
            if taken == 0 && continuity == Continuity::Replace {
                held.reset();
                replaced = true;
            }
            assert!(held.take(run, second, apps));
            taken += 1;
        },
    )
    .unwrap();
    held.left_out = outcome.left_out;
    coverage::accept_catalog(held, outcome.checked_catalog.or(outcome.continued_catalog)).unwrap();

    Since { taken, replaced }
}

fn publish(held: &mut Held) -> Arc<View> {
    let mailbox = Mailbox::new();
    held.publish(&mailbox);

    mailbox.take().unwrap()
}

/// The second of the wall clock the newest second held lies in.
fn newest_wall_second(held: &Held) -> u64 {
    let newest_ns = held.span.held().map(|second| second.wall_ns).max().unwrap();

    // The second floors.
    newest_ns / NANOSECONDS_PER_SECOND
}

/// The Apps that lead the newest second's CPU ranking, which a chart by App draws.
fn leading_apps(view: &View) -> Vec<Arc<RankedApp>> {
    let ranks = view.apps.newest().unwrap().ranks().unwrap();
    let apps: Vec<_> = ranks.metrics[0]
        .entries
        .iter()
        .take(COHORT_MAX)
        .map(|rank| Arc::clone(&rank.app))
        .collect();
    assert!(!apps.is_empty(), "the newest second ranks no App by CPU");

    apps
}

fn time_ranks(view: &View) {
    let newest = view.apps.newest().unwrap();
    // The half floors, so a short span ranks its middle second and never its first.
    let back = SECONDS_BACK.min((view.apps.len() - 1) / 2);
    let earlier = view.apps.second(view.apps.len() - 1 - back);

    time("rank the newest second", || newest.ranks(), ranks_digest);
    time(
        &format!("rank the second {back} seconds back"),
        || earlier.ranks(),
        ranks_digest,
    );
    time(
        "rank every second of the span",
        || {
            view.apps
                .seconds()
                .map(super::Second::ranks)
                .collect::<Vec<_>>()
        },
        |seconds| {
            let mut hasher = DefaultHasher::new();
            for ranks in seconds {
                hash_ranked(ranks, &mut hasher);
            }

            hasher.finish()
        },
    );
}

fn time_leaders(view: &View) {
    let window = WallSeconds {
        first: view.seconds.first().unwrap().wall_ns / NANOSECONDS_PER_SECOND,
        last: view.seconds.last().unwrap().wall_ns / NANOSECONDS_PER_SECOND,
    };
    let mut leaders = Leaders::new();

    time_leaders_over(
        "with its stretches",
        view,
        &view.stretches,
        &mut leaders,
        window,
    );
    time_leaders_over("without its stretches", view, &[], &mut leaders, window);
    time_leaders_again(view, &mut leaders, window);
}

/// Times the choice of the leaders over `window` of `view` with `stretches` by a room that
/// forgets before each choice, so each adds up every stretch, as a first choice does. The
/// forgetting is timed with the choice.
fn time_leaders_over(
    label: &str,
    view: &View,
    stretches: &[Arc<AppsStretch>],
    leaders: &mut Leaders,
    window: WallSeconds,
) {
    let readings = Readings {
        stretches,
        ..view.readings()
    };

    time(
        &format!("choose the leaders of the span {label}"),
        || {
            let mut cohorts = Cohorts::with_room();
            leaders.forget();
            leaders.choose(&readings, window, &mut cohorts);
            cohorts
        },
        cohorts_digest,
    );
}

/// Times the choice of the leaders over `window` of `view` by a room that chose over the same
/// stretches already, so it adds none of them up again, as the page's room does in nine seconds
/// of ten. In the tenth the oldest stretch leaves the window and the room adds every stretch up
/// again, as `time_leaders_over` times. Its last choice must give the cohorts a fresh room gives.
fn time_leaders_again(view: &View, leaders: &mut Leaders, window: WallSeconds) {
    let readings = view.readings();
    let mut fresh = Cohorts::with_room();
    Leaders::new().choose(&readings, window, &mut fresh);
    let fresh_digest = cohorts_digest(&fresh);
    leaders.choose(&readings, window, &mut Cohorts::with_room());

    time(
        "choose the leaders of the span again over the stretches it kept the sums of",
        || {
            let mut cohorts = Cohorts::with_room();
            leaders.choose(&readings, window, &mut cohorts);
            cohorts
        },
        |cohorts| {
            let digest = cohorts_digest(cohorts);
            assert_eq!(digest, fresh_digest);
            digest
        },
    );
}

fn time_charts(view: &View, cohort: &[Arc<RankedApp>]) {
    time(
        "attribution of the cohort at every second of the span",
        || {
            view.apps
                .seconds()
                .map(|second| second.attribution(AppAttributionMetric::Cpu, cohort))
                .collect::<Vec<_>>()
        },
        digest,
    );
    time(
        "resident memory of the cohort at every second of the span",
        || {
            view.apps
                .seconds()
                .map(|second| second.resident(cohort))
                .collect::<Vec<_>>()
        },
        digest,
    );
}

/// Times building the context a query of `selection` is answered with, and each read of it.
fn time_context(label: &str, loaded: &mut Loaded, selection: QuerySelection) {
    let app = Arc::clone(&leading_apps(&publish(&mut loaded.held))[0]);
    let queries = Queries::new();

    time(
        &format!("build a {label} context"),
        || context(loaded, &queries, selection),
        context_digest,
    );
    let context = context(loaded, &queries, selection);
    time_context_histories(label, &context, &app);
    time_context_pages(label, &context, &app);
}

/// The coverage the store proves of what `held` holds, built as the store builds it but for
/// the check of its ring, since the benchmark opens no ring.
fn checked_coverage(held: &Held) -> CheckedCoverage {
    let catalog = held.checked_catalog.as_ref().unwrap();
    let source = SourceGeneration {
        recording: catalog.recording,
        generation: held.source_generation,
    };
    let range = ContextRange {
        first_ns: catalog.first_ns.unwrap_or(0),
        end_ns: (newest_wall_second(held) + 1) * NANOSECONDS_PER_SECOND,
    };

    CheckedCoverage::new(source, range)
}

/// The context the store answers a query of `selection` with, served as its thread serves one.
fn context(loaded: &Loaded, queries: &Queries, selection: QuerySelection) -> Arc<QueryContext> {
    let handle = queries.handle(std::thread::current());
    handle
        .submit(QueryRequest {
            epoch: 0,
            source: None,
            selection,
        })
        .unwrap();
    queries.serve(
        &loaded.held,
        Some(&checked_coverage(&loaded.held)),
        &loaded.budget,
    );

    match handle.take().unwrap().outcome {
        QueryOutcome::Ready(context) => context,
        outcome => panic!("expected a context, found {outcome:?}"),
    }
}

fn context_digest(context: &Arc<QueryContext>) -> u64 {
    let seconds = context.seconds();

    digest(&(
        context.selected().observation,
        context.context_last_second(),
        seconds.len(),
        seconds.first().map(|second| second.observation),
        seconds.last().map(|second| second.observation),
    ))
}

fn time_context_histories(label: &str, context: &Arc<QueryContext>, app: &RankedApp) {
    let cancellation = Arc::new(Cancellation::default());
    let groups = context
        .app_process_groups_history(app.kind, &app.bytes, &cancellation, 0)
        .unwrap();
    let executable = groups
        .groups
        .iter()
        .find_map(|group| match group {
            ProcessGroup::Named(name) => Some(name.clone()),
            ProcessGroup::Other => None,
        })
        .unwrap();

    time(
        &format!("{label}: ranks of every second"),
        || {
            context
                .app_ranks(&mut super::RankedSeconds::default())
                .unwrap()
        },
        |ranks| window_digest(context, ranks),
    );
    time(
        &format!("{label}: resident history"),
        || context.app_resident_history(&cancellation, 0).unwrap(),
        history_digest,
    );
    time(
        &format!("{label}: attribution history"),
        || {
            context
                .app_attribution_history(AppAttributionMetric::Cpu, &cancellation, 0)
                .unwrap()
        },
        history_digest,
    );
    time(
        &format!("{label}: trace of one App"),
        || {
            context
                .app_history(app.kind, &app.bytes, &cancellation, 0)
                .unwrap()
        },
        history_digest,
    );
    time(
        &format!("{label}: trace of one executable name"),
        || {
            context
                .app_process_history(app.kind, &app.bytes, &executable, &cancellation, 0)
                .unwrap()
        },
        history_digest,
    );
    time(
        &format!("{label}: process groups of one App"),
        || {
            context
                .app_process_groups_history(app.kind, &app.bytes, &cancellation, 0)
                .unwrap()
        },
        |groups| groups_digest(groups),
    );
}

fn time_context_pages(label: &str, context: &Arc<QueryContext>, app: &RankedApp) {
    let cancellation = Arc::new(Cancellation::default());
    let selected = context.selected().observation;
    let page = Page {
        offset: 0,
        limit: PAGE_ROWS,
    };
    let detail = context
        .app_detail_for(&selected, app.kind, &app.bytes, page, &cancellation, 0)
        .unwrap();
    assert!(!detail.is_null(), "the second selected holds no such App");

    time(
        &format!("{label}: the page of Apps"),
        || {
            context
                .apps_page_for(&selected, page, &cancellation, 0)
                .unwrap()
        },
        response_digest,
    );
    time(
        &format!("{label}: the page of one App's processes"),
        || {
            context
                .app_detail_for(&selected, app.kind, &app.bytes, page, &cancellation, 0)
                .unwrap()
        },
        response_digest,
    );
    time(
        &format!("{label}: the page of processes outside any App"),
        || {
            context
                .ungrouped_page_for(&selected, page, &cancellation, 0)
                .unwrap()
        },
        response_digest,
    );
    time_context_pages_selection(label, context, &selected, &cancellation);
}

/// Times the selection of `selected`, once building every chart point and once sharing every
/// point the reading before built.
fn time_context_pages_selection(
    label: &str,
    context: &Arc<QueryContext>,
    selected: &crate::live_app_response::Observation,
    cancellation: &Arc<Cancellation>,
) {
    time(
        &format!("{label}: the selection, every point built"),
        || {
            context
                .selection_for(
                    selected,
                    false,
                    &mut ChartPoints::default(),
                    cancellation,
                    0,
                )
                .unwrap()
        },
        response_digest,
    );
    // The cache holds what the reading before left, so each timed reading shares every point.
    let mut chart_points = ChartPoints::default();
    drop(
        context
            .selection_for(selected, false, &mut chart_points, cancellation, 0)
            .unwrap(),
    );
    time(
        &format!("{label}: the selection, every point shared"),
        || {
            context
                .selection_for(selected, false, &mut chart_points, cancellation, 0)
                .unwrap()
        },
        response_digest,
    );
}

/// What publishing after every take of a load cost.
struct Takes {
    /// What each take and its publish took, in the order the load handed the seconds over.
    elapsed_ns: Vec<u128>,
    /// What packing each block took the packing thread.
    packing: super::packing::BlockTimes,
    /// What freeing the batches it was handed took the packing thread over the load.
    freeing: super::packing::FreeingTimes,
    /// The span the load ended with.
    view: Arc<View>,
}

/// Loads the hour of `root` publishing a View after every take, and keeps hold of the View
/// published after take `view_held_after`, counted from one, until the load ends, where it
/// names one. Before each take it waits, untimed, until the packing thread has answered what
/// it was handed, since in the running app a second passes between takes, so a take's time
/// holds the exchange and no packing.
fn timed_takes(root: &Path, view_held_after: Option<usize>) -> Takes {
    let mailbox = Mailbox::new();
    let mut elapsed_ns = Vec::new();
    let mut view_held = None;
    let mut loaded = load_hour(root, |held, run, second, apps| {
        timed_takes_wait(held);
        let started = Instant::now();
        assert!(held.take(run, second, apps));
        held.publish(&mailbox);
        elapsed_ns.push(started.elapsed().as_nanos());
        let view = mailbox.take().unwrap();
        if Some(elapsed_ns.len()) == view_held_after {
            view_held = Some(view);
        }
    });
    drop(view_held);
    loaded.held.apps.settle();
    let view = publish(&mut loaded.held);
    let tally = loaded.held.apps.packing_tally();
    let packing = tally.block_times();
    let freeing = tally.freeing_times();

    Takes {
        elapsed_ns,
        packing,
        freeing,
        view,
    }
}

/// Waits until the packing thread has answered every message `held` handed it, woken by each
/// answer, within a bound of ten seconds.
fn timed_takes_wait(held: &Held) {
    let tally = held.apps.packing_tally();
    for _ in 0..10_000 {
        if tally.finished() {
            return;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(1));
    }
    panic!("the packing thread did not answer within ten seconds");
}

/// How many of a load's first takes a report leaves out, so that it covers the takes that fill
/// the span the store ends with. It leaves out the first at least, after which a View can be
/// held through every take reported.
fn takes_reported_from(take_count: usize) -> usize {
    take_count.saturating_sub(SECONDS_MAX).max(1)
}

fn report_takes(label: &str, takes: &Takes, reported_from: usize) {
    let reported = &takes.elapsed_ns[reported_from..];
    let mut sorted = reported.to_vec();
    sorted.sort_unstable();
    // The mean floors, which loses under a nanosecond, and the median of an even count is the
    // higher of its two middle takes.
    let mean = reported.iter().sum::<u128>() / u128::try_from(reported.len()).unwrap();

    println!(
        "{label}: mean {mean} ns, median {} ns, slowest {} ns over the newest {} of {} takes",
        sorted[sorted.len() / 2],
        sorted[sorted.len() - 1],
        reported.len(),
        takes.elapsed_ns.len(),
    );
    // No line below is a figure `gate.py` reads, since today's code has no packing thread. The
    // share, the thirtieth and the freeing a second floor.
    let packing = &takes.packing;
    println!(
        "{label}, packing a block on its own thread took a mean of {} ns and at most {} ns over \
         {} blocks, {} millionths of one core at one block every thirty seconds",
        packing.mean_ns,
        packing.highest_ns,
        packing.block_count,
        packing.mean_ns / 30_000,
    );
    let freeing = &takes.freeing;
    let take_count = u64::try_from(takes.elapsed_ns.len()).unwrap();
    let freeing_ns = freeing.ns_sum / take_count;
    println!(
        "{label}, freeing what it was handed took the packing thread {freeing_ns} ns a second, \
         {} ns over {} batches and {take_count} takes",
        freeing.ns_sum, freeing.batch_count,
    );
    println!(
        "{label}, a second on both threads costs {} ns, the take's mean, a thirtieth of the \
         packing's and the freeing a second",
        mean + u128::from(packing.mean_ns / 30) + u128::from(freeing_ns),
    );
    report_span("the span those takes left", &takes.view);
}

/// The memory of this process and of the store's budget at one moment.
struct Memory {
    footprint: Footprint,
    budget_bytes: usize,
    budget_bytes_peak: usize,
}

impl Memory {
    fn read(budget: &ByteBudget) -> Self {
        Self {
            footprint: Footprint::read().unwrap(),
            budget_bytes: budget.used(),
            budget_bytes_peak: budget.used_peak(),
        }
    }
}

fn report_memory(label: &str, memory: &Memory) {
    println!(
        "{label}: footprint {} bytes, footprint peak {} bytes, budget {} bytes, budget peak {} \
         bytes",
        memory.footprint.current_bytes,
        memory.footprint.lifetime_bytes_max,
        memory.budget_bytes,
        memory.budget_bytes_peak,
    );
}

/// Says which seconds the span of `view` holds and a digest of each part of their readings, so
/// that two spans that differ say where.
fn report_span(label: &str, view: &View) {
    println!(
        "{label}: {} seconds from wall second {} to {}, digests: machine seconds {:016x}, Apps \
         {:016x}, earlier seconds {:016x}, sampled seconds {:016x}, {} of {} sampled seconds \
         ranked",
        view.seconds.len(),
        view.seconds.first().unwrap().wall_ns / NANOSECONDS_PER_SECOND,
        view.seconds.last().unwrap().wall_ns / NANOSECONDS_PER_SECOND,
        machine_digest(&view.seconds),
        apps_digest(view),
        machine_digest(&view.earlier),
        sampled_digest(view),
        view.sampled
            .iter()
            .filter(|sample| sample.ranks().is_ok())
            .count(),
        view.sampled.len(),
    );
}

/// Says how many App rows the span's seconds hold of their own and how often the Apps they show
/// change from one second to the next, and gives the bytes those rows take. A packed second's
/// block holds its rows in its table, which the process rows' figure counts, but the second
/// still shows its Apps.
fn report_apps(view: &View) -> usize {
    let mut cursor = AppsCursor::default();
    let mut apps_before: Vec<Arc<RankedApp>> = Vec::new();
    let (mut rows, mut bytes) = (0_usize, 0_usize);
    let (mut sets_changed, mut apps_changed) = (0_usize, 0_usize);
    for (held, second) in view.apps.iter().zip(view.apps.seconds()) {
        assert_eq!(held.observation, second.observation());
        let rows_held = held.apps.as_slice();
        rows += rows_held.len();
        bytes += size_of_val(rows_held);
        let apps: Vec<Arc<RankedApp>> = cursor
            .apps(second)
            .iter()
            .map(|row| Arc::clone(&row.name))
            .collect();
        let (now, before) = (shown(&apps), shown(&apps_before));
        if !before.is_empty() && now != before {
            sets_changed += 1;
            apps_changed += now.iter().filter(|app| !before.contains(app)).count();
            apps_changed += before.iter().filter(|app| !now.contains(app)).count();
        }
        apps_before = apps;
    }

    println!("App rows: {rows} in {bytes} bytes, the Apps held changed at {sets_changed} seconds");
    println!("Apps that came or went at those seconds: {apps_changed}");
    bytes
}

/// The identities of `apps`, in their order.
fn shown(apps: &[Arc<RankedApp>]) -> Vec<&AppIdentity> {
    apps.iter().map(|app| app_identity(app)).collect()
}

/// Says how many App details the span's seconds name and how many the store holds once, and
/// gives the bytes they take as held.
fn report_details(view: &View) -> usize {
    let mut distinct: HashSet<*const AppDetails> = HashSet::new();
    let mut references = 0_usize;
    let payloads = view
        .apps
        .iter()
        .filter_map(|second| second.processes.as_ref().ok());
    // A packed second holds its Apps' details in its block's table, which the process rows'
    // figure counts.
    for payload in payloads {
        for details in payload.own_apps() {
            references += 1;
            distinct.insert(std::ptr::from_ref(details));
        }
    }
    let bytes = references * Payload::APP_DETAILS_REFERENCE_BYTES
        + distinct.len() * Payload::APP_DETAILS_BYTES;

    println!(
        "App details as held: {bytes} bytes, {references} named, {} held once",
        distinct.len(),
    );
    bytes
}

/// Says how the span's process rows repeat from one second to the next, and gives the bytes
/// they take as held.
fn report_processes(view: &View) -> usize {
    let census = ProcessCensus::of(view);
    let bytes = census.rows * Payload::ROW_REFERENCE_BYTES
        + census.distinct * Payload::ROW_BYTES
        + census.table_bytes;

    println!(
        "process rows of the plain seconds: {}, of them {} shared with the second before",
        census.rows, census.shared,
    );
    println!(
        "  {} of a process the second before does not hold",
        census.arrived
    );
    println!(
        "  {} that differ in a reading, {} of all, shared or not, in the App index",
        census.readings_differ, census.index_differs,
    );
    println!(
        "  {} held apart though equal in every reading, whose App index moved",
        census.index_only
    );
    println!(
        "process rows as held: {bytes} bytes, {} rows held once",
        census.distinct,
    );
    // No line below names a figure the gate reads, so a build before packing prints none of
    // them and the gate refuses nothing.
    println!(
        "  {} more read from {} packed blocks, whose block tables take {} bytes",
        census.packed_rows, census.tables, census.table_bytes,
    );
    println!(
        "  {} of those blocks pack their App rows and {} keep them plain",
        census.app_rows_packed,
        census.tables - census.app_rows_packed,
    );
    report_app_rows_shares(view);
    println!(
        "Blocks of the span packed {} of {}, kept plain for a bound {} and for the budget {}",
        view.apps.packed_count(),
        view.apps.block_count(),
        view.apps.kept_plain_count(KeptPlain::Bound),
        view.apps.kept_plain_count(KeptPlain::Budget),
    );
    bytes
}

/// What comparing each process row of a span with the row of the same process a second before
/// counts.
#[derive(Default)]
struct ProcessCensus {
    /// The rows of the plain seconds, which the counts below the first rows' count compare.
    rows: usize,
    /// The rows the store holds once, however many seconds name them.
    distinct: usize,
    /// The rows that are the row of the second before, at the same address.
    shared: usize,
    /// The rows of a process the second before does not hold.
    arrived: usize,
    readings_differ: usize,
    /// The rows whose App index is not the one of the second before, shared or not.
    index_differs: usize,
    /// The rows held apart from the row of the second before although equal to it in every
    /// reading, whose App index moved.
    index_only: usize,
    /// The processes read at the seconds of packed blocks, which hold no row of their own.
    packed_rows: usize,
    /// The packed blocks, each counted once.
    tables: usize,
    /// The bytes the tables of the packed blocks, which hold their processes and their Apps,
    /// are charged.
    table_bytes: usize,
    /// How many of the packed blocks hold their App rows in their table.
    app_rows_packed: usize,
}

/// A process row of a second and the App index the second gives it.
type Listed<'view> = (Option<u32>, &'view ProcessRow);

impl ProcessCensus {
    fn of(view: &View) -> Self {
        let mut census = Self::default();
        let mut before: HashMap<(i32, String), Listed> = HashMap::new();
        let mut now: HashMap<(i32, String), Listed> = HashMap::new();
        let mut distinct: HashSet<usize> = HashSet::new();
        for second in view.apps.iter() {
            let Ok(payload) = &second.processes else {
                before.clear();
                continue;
            };
            // A packed second's processes read their first row in the block, which the rows
            // held once count, and its changes from the block's table.
            if payload.packed() {
                census.packed_rows += payload.len();
                distinct.extend(payload.processes().map(|process| process.row_address()));
                before.clear();
                continue;
            }
            now.clear();
            for (index, row) in payload.iter().enumerate() {
                distinct.insert(std::ptr::from_ref(row).addr());
                let key = (row.pid, format!("{:?}", row.creation));
                let listed = (payload.app_index(index), row);
                census.count(before.get(&key).copied(), listed);
                now.insert(key, listed);
            }
            std::mem::swap(&mut before, &mut now);
        }
        census.distinct = distinct.len();
        // A packed block's table holds its Apps too, and a block whose every second refused its
        // processes still holds one, so the tables are counted by block.
        let tables = view.apps.blocks().iter().filter_map(|block| block.table());
        for table in tables {
            census.tables += 1;
            census.table_bytes += table.reserved_bytes();
            if table.app_rows_packed() {
                census.app_rows_packed += 1;
            }
        }

        census
    }

    /// Counts `listed` against `before`, the row of its process a second before and its App
    /// index there, where that second holds one.
    fn count(&mut self, before: Option<Listed>, listed: Listed) {
        let (app_index, row) = listed;
        self.rows += 1;
        let Some((old_app_index, old)) = before else {
            self.arrived += 1;
            return;
        };

        let moved = old_app_index != app_index;
        self.index_differs += usize::from(moved);
        if std::ptr::eq(old, row) {
            self.shared += 1;
        } else {
            let same = same_readings(old, row);
            self.index_only += usize::from(same && moved);
            self.readings_differ += usize::from(!same);
        }
    }
}

/// Prints the lowest, the median and the highest share of their plain bytes that the packed
/// blocks' Apps take packed, which decides whether a block packs them.
fn report_app_rows_shares(view: &View) {
    let mut shares: Vec<f64> = view
        .apps
        .app_rows_bytes()
        .map(|bytes| {
            let packed = u32::try_from(bytes.packed).unwrap();
            let plain = u32::try_from(bytes.plain).unwrap();
            f64::from(packed) / f64::from(plain)
        })
        .collect();
    if shares.is_empty() {
        return;
    }
    shares.sort_unstable_by(f64::total_cmp);

    // The median of an even count is the higher of its two middle blocks.
    println!(
        "  their packed App parts take of their plain App bytes: lowest {:.1}%, median {:.1}%, \
         highest {:.1}%",
        100.0 * shares[0],
        100.0 * shares[shares.len() / 2],
        100.0 * shares[shares.len() - 1],
    );
}

/// Whether two rows of one process hold the same readings.
fn same_readings(old: &ProcessRow, row: &ProcessRow) -> bool {
    old.executable_name() == row.executable_name()
        && old.program_unavailable == row.program_unavailable
        && old.cpu_ns() == row.cpu_ns()
        && old.energy_nj() == row.energy_nj()
        && old.gpu_ns() == row.gpu_ns()
        && old.disk_read_bytes() == row.disk_read_bytes()
        && old.disk_written_bytes() == row.disk_written_bytes()
        && old.disk_io_bytes() == row.disk_io_bytes()
        && old.resident_bytes() == row.resident_bytes()
        && old.footprint_bytes() == row.footprint_bytes()
        && old.idle_wakeups() == row.idle_wakeups()
        && old.pageins() == row.pageins()
        && old.threads() == row.threads()
        && old.app_attribution == row.app_attribution
        && old.state == row.state
}

/// A digest of the readings of the Mac at each of `seconds`.
fn machine_digest(seconds: &[Arc<MachineSecond>]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for second in seconds {
        hash_machine(second, &mut hasher);
    }

    hasher.finish()
}

/// Adds every reading of the Mac at `second` to `hasher`, and nothing of the budget its
/// complete readings are charged to, which those readings print with their own.
fn hash_machine(second: &MachineSecond, hasher: &mut DefaultHasher) {
    let summary = MachineSecond {
        complete: CompleteMachineState::Unread,
        cpu_refused: None,
        ..second.clone()
    };
    digest(&summary).hash(hasher);
    let refusal = match &second.complete {
        CompleteMachineState::Refused(reason) => Some(*reason),
        CompleteMachineState::Unread | CompleteMachineState::Available(_) => None,
    };
    let complete = second.complete().map(|complete| complete.summary(second));
    let readings = (
        refusal,
        complete,
        second.cpu_counters(),
        second.cpu_interval(),
    );
    digest(&readings).hash(hasher);
}

/// A digest of the Apps of every second of the span: each second's ranks and facts with each of
/// its App rows and process rows.
fn apps_digest(view: &View) -> u64 {
    let mut hasher = DefaultHasher::new();
    let mut cursor = AppsCursor::default();
    for second in view.apps.seconds() {
        hash_ranked(&second.ranks(), &mut hasher);
        hash_second(second.stored(), cursor.apps(second), &mut hasher);
    }

    hasher.finish()
}

/// A digest of every sampled second a View holds.
fn sampled_digest(view: &View) -> u64 {
    let mut hasher = DefaultHasher::new();
    for second in &view.sampled {
        hash_sampled(second, &mut hasher);
    }

    hasher.finish()
}

/// Hashes `second` with `apps`, the rows of its Apps as a reader reads them.
fn hash_second(second: &AppsSecond, apps: &[AppRow], hasher: &mut DefaultHasher) {
    let AppsSecond {
        metadata,
        processes,
        missing,
        observation,
        consecutive,
        elapsed_ns,
        membership_complete,
        ungrouped_members,
        live_ungrouped_members,
        usage_refused,
        usage_gone,
        apps: _,
        totals,
    } = second;
    let facts = (
        observation,
        consecutive,
        elapsed_ns,
        membership_complete,
        ungrouped_members,
        live_ungrouped_members,
        usage_refused,
        usage_gone,
        metadata.origin,
        missing,
        totals,
    );
    digest(&facts).hash(hasher);
    for row in apps {
        let measures = METRICS.map(|metric| row.measure(metric));
        let facts = (
            app_identity(&row.name),
            row.members,
            row.membership_complete,
        );
        digest(&(facts, measures)).hash(hasher);
    }
    match processes {
        Ok(payload) => {
            digest(&(payload.selected, payload.previous)).hash(hasher);
            for details in payload.apps() {
                digest(&details).hash(hasher);
            }
            for process in payload.processes() {
                hash_process(process, apps, hasher);
            }
        }
        Err(missing) => digest(missing).hash(hasher),
    }
}

/// Adds every reading of a sampled second to `hasher`, naming each App by its identity alone.
fn hash_sampled(second: &SampledSecond, hasher: &mut DefaultHasher) {
    hash_machine(&second.machine, hasher);
    digest(&second.observation).hash(hasher);
    match second.ranks() {
        Ok(ranks) => {
            let facts = (
                ranks.selected,
                ranks.app_count,
                ranks.membership_complete,
                ranks.ungrouped_members,
            );
            digest(&facts).hash(hasher);
            for ranking in &ranks.metrics {
                let facts = (
                    ranking.metric,
                    ranking.unavailable,
                    ranking.overflowed,
                    ranking.partial,
                );
                digest(&facts).hash(hasher);
                for rank in ranking.entries() {
                    let facts = (
                        app_identity(rank.app),
                        rank.amount,
                        rank.rate,
                        rank.measured_members,
                        rank.unavailable_members,
                    );
                    digest(&facts).hash(hasher);
                }
            }
        }
        Err(missing) => digest(missing).hash(hasher),
    }
}

/// Adds every reading of a listed row to `hasher`, with the App of `apps` the row belongs to named
/// by its identity, so the digest stays the same wherever the store keeps that App's index.
fn hash_process(process: ProcessAt<'_>, apps: &[AppRow], hasher: &mut DefaultHasher) {
    let app = process
        .app_index()
        .map(|index| app_identity(&apps[usize::try_from(index).unwrap()].name));
    let facts = (
        process.pid(),
        process.creation(),
        app,
        process.executable_name().map(|name| &name.bytes),
        process.program_unavailable(),
        process.app_attribution(),
        process.state(),
    );
    let amounts = [
        process.cpu_ns(),
        process.energy_nj(),
        process.gpu_ns(),
        process.disk_read_bytes(),
        process.disk_written_bytes(),
        process.disk_io_bytes(),
        process.resident_bytes(),
        process.footprint_bytes(),
        process.idle_wakeups(),
        process.pageins(),
        process.threads(),
    ];
    digest(&(facts, amounts)).hash(hasher);
}

fn app_identity(app: &RankedApp) -> &AppIdentity {
    app
}

/// A digest of the ranks `ranks` holds for each second of `context`.
fn window_digest(context: &QueryContext, ranks: &ContextRanks<WindowRanks>) -> u64 {
    let mut hasher = DefaultHasher::new();
    for second in context.seconds() {
        match ranks.selected_ranks_for(&second.observation) {
            Ok(ranks) => hash_ranks(&ranks, &mut hasher),
            Err(error) => digest(&error).hash(&mut hasher),
        }
    }

    hasher.finish()
}

fn ranks_digest<E: Debug>(ranks: &Result<Ranks, E>) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_ranked(ranks, &mut hasher);

    hasher.finish()
}

fn hash_ranked<E: Debug>(ranks: &Result<Ranks, E>, hasher: &mut DefaultHasher) {
    match ranks {
        Ok(ranks) => hash_ranks(ranks, hasher),
        Err(error) => digest(error).hash(hasher),
    }
}

/// Adds every reading of `ranks` to `hasher`. It names each App by its identity alone, since a
/// ranked App prints the budget its name is charged to, which changes as the store works.
fn hash_ranks(ranks: &Ranks, hasher: &mut DefaultHasher) {
    let Ranks {
        selected,
        app_count,
        membership_complete,
        ungrouped_members,
        metrics,
    } = ranks;
    digest(&(selected, app_count, membership_complete, ungrouped_members)).hash(hasher);
    for ranking in metrics {
        let Ranking {
            metric,
            entries,
            unavailable,
            overflowed,
            partial,
        } = ranking;
        digest(&(metric, unavailable, overflowed, partial)).hash(hasher);
        for rank in entries {
            let Rank {
                app,
                amount,
                rate,
                measured_members,
                unavailable_members,
            } = rank;
            let facts = (
                app_identity(app),
                amount,
                rate,
                measured_members,
                unavailable_members,
            );
            digest(&facts).hash(hasher);
        }
    }
}

fn cohorts_digest(cohorts: &Cohorts) -> u64 {
    let Cohorts {
        seconds,
        cpu,
        power,
        disk,
        gpu,
        resident,
        refused,
    } = cohorts;
    let mut hasher = DefaultHasher::new();
    (seconds, refused).hash(&mut hasher);
    for cohort in [cpu, power, disk, gpu, resident] {
        cohort.len().hash(&mut hasher);
        for app in cohort {
            digest(app_identity(app)).hash(&mut hasher);
        }
    }

    hasher.finish()
}

fn groups_digest(history: &AppProcessGroupsHistory) -> u64 {
    let AppProcessGroupsHistory {
        selected,
        app_kind,
        app_bytes,
        context_seconds,
        named_group_count,
        context_observations_unavailable,
        groups,
        points,
    } = history;
    let facts = (
        selected,
        app_kind,
        app_bytes,
        context_seconds,
        named_group_count,
        context_observations_unavailable,
    );
    let mut hasher = DefaultHasher::new();
    digest(&facts).hash(&mut hasher);
    for group in groups {
        match group {
            ProcessGroup::Named(name) => name.hash(&mut hasher),
            ProcessGroup::Other => 0_u8.hash(&mut hasher),
        }
    }
    for point in points {
        hash_groups_point(point, &mut hasher);
    }

    hasher.finish()
}

fn hash_groups_point(point: &ProcessGroupsPoint, hasher: &mut DefaultHasher) {
    let ProcessGroupsPoint {
        observation,
        elapsed_ns,
        source_available,
        membership_complete,
        unnamed_members,
        readings,
        live,
    } = point;
    let facts = (
        observation,
        elapsed_ns,
        source_available,
        membership_complete,
        unnamed_members,
        live,
    );
    digest(&facts).hash(hasher);
    for reading in readings {
        digest(&(reading.present, reading.members, reading.measures)).hash(hasher);
    }
}

/// A digest of a history or a trace but for the bytes its context takes, which say how the
/// store holds the context's seconds and nothing of what they read.
fn history_digest(response: &Response<Value>) -> u64 {
    let mut history = (**response).clone();
    let context_bytes = history["coverage"]
        .as_object_mut()
        .and_then(|coverage| coverage.remove("context_metadata_bytes"));
    assert!(
        context_bytes.is_some(),
        "the history says the bytes its context takes"
    );

    digest(&history)
}

fn response_digest<T: Debug>(response: &Response<T>) -> u64 {
    digest(&**response)
}

/// A digest of how `value` prints, for values that print the readings they hold and nothing of
/// how the store holds them, which leaves out a ranked App.
fn digest(value: &impl Debug) -> u64 {
    let mut hasher = DefaultHasher::new();
    format!("{value:?}").hash(&mut hasher);

    hasher.finish()
}

/// Runs `read` `RUNS` times and says its median and highest time, and `digest` of what the last
/// run read, which is taken outside the time.
fn time<T>(label: &str, mut read: impl FnMut() -> T, digest: impl Fn(&T) -> u64) {
    let mut elapsed_ns = Vec::with_capacity(RUNS);
    let mut digest_last = 0;
    for run in 0..RUNS {
        let started = Instant::now();
        let value = black_box(read());
        elapsed_ns.push(started.elapsed().as_nanos());
        if run == RUNS - 1 {
            digest_last = digest(&value);
        }
    }
    elapsed_ns.sort_unstable();

    // The median of the even count is the higher of its two middle runs.
    println!(
        "{label}: median {} ns, highest {} ns, digest {digest_last:016x}",
        elapsed_ns[RUNS / 2],
        elapsed_ns[RUNS - 1],
    );
}
