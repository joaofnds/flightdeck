use super::*;

#[test]
fn places_gap_before_resumed_observation() {
    // Goal: a delayed wait puts the ordinal gap before the observation, preserving its clocks.
    // Method: drive real Run acquisition and decode its ring with fixed boundary clocks.
    let mut fixture = Fixture::new("delayedwait");
    let baseline = fixture.run.first_tick;
    let first = fixture.observation(10_000_000_000);
    let resumed = fixture.observation(20_200_000_000);
    let successor = fixture.observation(21_000_000_000);
    let started = fixture.deadline(baseline);
    let delayed = fixture.deadline(baseline + 10) + fixture.period() / 5;
    let recovered = fixture.deadline(baseline + 11);

    let requested = fixture.tick(baseline, &Clocks::at(started), Some(first));
    let following = fixture.tick(requested.following, &Clocks::at(delayed), Some(resumed));
    fixture.tick(following.following, &Clocks::at(recovered), Some(successor));

    fixture.assert_publication(
        &Found::Publication {
            publication: 2,
            tick: baseline,
        },
        Some(&first),
    );
    fixture.assert_publication(
        &Found::Publication {
            publication: 3,
            tick: baseline + 10,
        },
        Some(&resumed),
    );
    fixture.assert_publication(
        &Found::Publication {
            publication: 4,
            tick: baseline + 11,
        },
        Some(&successor),
    );
    assert_eq!(fixture.reader.read().unwrap(), Found::Nothing);
    assert_eq!(fixture.run.whole.tally.ticks, 3);
    assert_eq!(fixture.run.whole.tally.slips, 9);
}

macro_rules! timing_cases {
    ($($name:ident: $start:expr, $finish:expr => $observed:expr, $following:expr;)*) => {$ (
        #[test]
        fn $name() {
            // Goal: start selects the ordinal, completion skips deadlines, and lateness survives.
            // Method: decode one real acquisition with boundary readings in tenths of a period.
            let mut fixture = Fixture::new(stringify!($name));
            let requested = fixture.run.first_tick;
            let deadline = fixture.deadline(requested);
            let clocks = Clocks {
                started_at: deadline + fixture.period() * $start / 10,
                finished_at: deadline + fixture.period() * $finish / 10,
            };
            let times = fixture.observation(10_000_000_000);

            let step = fixture.tick(requested, &clocks, Some(times));

            assert_eq!(step.requested, requested);
            assert_eq!(step.observed, requested + $observed);
            assert_eq!(step.following, requested + $following);
            fixture.assert_publication(
                &Found::Publication { publication: 2, tick: requested + $observed }, Some(&times),
            );
            assert_eq!(fixture.reader.read().unwrap(), Found::Nothing);
            assert_eq!(fixture.run.whole.tally.ticks, 1);
            assert_eq!(fixture.run.whole.tally.slips, $following - 1);
            assert_eq!(fixture.run.whole.following(), requested + $following);
            assert_eq!(fixture.run.whole.tally.late_total, Duration::from_nanos(
                fixture.run.timebase.nanoseconds(clocks.started_at - deadline).unwrap(),
            ));
        }
    )*};
}

timing_cases! {
    preserves_an_on_time_observation: 0, 0 => 0, 1;
    preserves_a_subperiod_late_observation: 9, 9 => 0, 1;
    assigns_the_exact_start_deadline: 90, 90 => 9, 10;
    skips_the_exact_completion_deadline: 0, 100 => 0, 11;
    keeps_acquisition_delay_after_the_observation: 0, 92 => 0, 10;
    counts_wait_and_acquisition_slips_once: 92, 124 => 9, 13;
}

#[test]
fn reports_a_boundary_crossed_before_acquisition() {
    // Goal: requested and following close the report even when observed is past its boundary.
    // Method: acquire immediately before a minute, then resume past it and inspect reports.
    let mut fixture = Fixture::new("crossedboundary");
    let boundary = (fixture.run.first_tick + 2).div_ceil(TICKS_PER_MINUTE) * TICKS_PER_MINUTE;
    let before = fixture.deadline(boundary - 2);
    let times = fixture.observation(10_000_000_000);
    let step = fixture.tick(fixture.run.first_tick, &Clocks::at(before), Some(times));
    let window_first_tick = fixture.run.window.first_tick;
    fixture.output.set_position(0);
    let resumed = fixture.deadline(boundary + 1);

    let step = fixture.tick(step.following, &Clocks::at(resumed), Some(times));

    assert_eq!(step.observed, boundary + 1);
    assert_eq!(fixture.run.window.first_tick, boundary + 2);
    assert_eq!(fixture.run.minute.first_tick, boundary + 2);
    assert_eq!(fixture.run.minute.tally.ticks, 0);
    let report = fixture.report();
    assert_eq!(
        report
            .lines()
            .filter(|line| line.starts_with("ticks "))
            .count(),
        1
    );
    assert_eq!(
        report
            .lines()
            .filter(|line| line.starts_with("minute, ticks "))
            .count(),
        1
    );
    assert!(report.contains(&format!("ticks {window_first_tick}-{}:", boundary + 1)));
    assert!(report.contains(&format!(
        "minute, ticks {}-{}:",
        fixture.run.first_tick,
        boundary + 1
    )));
}

#[test]
fn reports_many_crossed_minutes_once_without_allocating() {
    // Goal: even a long delayed wait publishes once and closes at most one report per span.
    // Method: count allocator calls while a real acquisition crosses three reporting minutes.
    let mut fixture = Fixture::new("manyminutes");
    let requested = fixture.run.first_tick;
    let observed = requested + 3 * TICKS_PER_MINUTE;
    let started = fixture.deadline(observed);
    let times = fixture.observation(190_000_000_000);
    fixture.output.set_position(0);

    let calls = allocator_calls_in(|| {
        fixture.tick(requested, &Clocks::at(started), Some(times));
    });

    assert_eq!(calls, 0);
    fixture.assert_publication(
        &Found::Publication {
            publication: 2,
            tick: observed,
        },
        Some(&times),
    );
    assert_eq!(fixture.reader.read().unwrap(), Found::Nothing);
    assert_eq!(fixture.run.whole.tally.ticks, 1);
    assert_eq!(fixture.run.whole.tally.slips, 3 * TICKS_PER_MINUTE);
    let report = fixture.report();
    assert_eq!(
        report
            .lines()
            .filter(|line| line.starts_with("ticks "))
            .count(),
        1
    );
    assert_eq!(
        report
            .lines()
            .filter(|line| line.starts_with("minute, ticks "))
            .count(),
        1
    );
}

#[test]
fn preserves_the_gap_without_observation_clocks_or_allocations() {
    // Goal: unavailable observation clocks retain scheduling identity without allocating.
    // Method: refuse the snapshot at a delayed real acquisition and decode its ring.
    let mut fixture = Fixture::new("delayedclockrefusal");
    let requested = fixture.run.first_tick;
    let observed = requested + 9;
    let started = fixture.deadline(observed);

    let calls = allocator_calls_in(|| {
        fixture.tick(requested, &Clocks::at(started), None);
    });

    fixture.assert_publication(
        &Found::Publication {
            publication: 2,
            tick: observed,
        },
        None,
    );
    assert!(!fixture.reader.tick().processes.is_empty());
    assert_eq!(fixture.run.whole.tally.slips, 9);
    assert_eq!(calls, 0);
    assert!(
        fixture
            .report()
            .contains("observation clocks unavailable kind=InvalidData")
    );
}

#[test]
fn preserves_warm_up_and_gaps_before_the_first_regular_observation() {
    // Goal: tick zero remains warm-up while the first regular delayed observation leaves a gap.
    // Method: the fixture decodes warm-up, then reads a delayed first tick and its successor.
    let mut fixture = Fixture::new("firstdelayed");
    assert_eq!(fixture.run.whole.tally.ticks, 0);
    assert_eq!(fixture.run.whole.tally.slips, 0);
    let requested = fixture.run.first_tick;
    let observed = requested + 9;
    let times = fixture.observation(20_200_000_000);
    let recovered = fixture.observation(21_000_000_000);
    let started = fixture.deadline(observed) + fixture.period() / 5;

    let step = fixture.tick(requested, &Clocks::at(started), Some(times));
    fixture.tick(
        step.following,
        &Clocks::at(fixture.deadline(observed + 1)),
        Some(recovered),
    );

    fixture.assert_publication(
        &Found::Publication {
            publication: 2,
            tick: observed,
        },
        Some(&times),
    );
    fixture.assert_publication(
        &Found::Publication {
            publication: 3,
            tick: observed + 1,
        },
        Some(&recovered),
    );
    assert_eq!(fixture.reader.read().unwrap(), Found::Nothing);
    assert_eq!(fixture.run.whole.tally.ticks, 2);
    assert_eq!(fixture.run.whole.tally.slips, 9);
}

#[test]
fn associates_delayed_events_with_their_publication() {
    // Goal: a corrected ordinal tags both the publication and every event it drained.
    // Method: keep a native child alive through a delayed acquisition and decode its Start.
    let mut fixture = Fixture::new("delayedevents");
    let mut child = process::OwnedChild::spawn(
        std::process::Command::new("/bin/cat").stdin(std::process::Stdio::piped()),
    );
    let pid = child.child.id().cast_signed();
    let requested = fixture.run.first_tick;
    let observed = requested + 9;
    let times = fixture.observation(20_200_000_000);
    let started = fixture.deadline(observed);

    fixture.tick(requested, &Clocks::at(started), Some(times));

    fixture.assert_publication(
        &Found::Publication {
            publication: 2,
            tick: observed,
        },
        Some(&times),
    );
    let range = fixture.reader.tick().events.clone();
    assert!(!range.is_empty());
    let mut found_child = false;
    for expected in fixture.reader.next_event()..range.end {
        let FoundEvent::Event {
            number,
            tick,
            event,
        } = fixture.reader.read_event().unwrap()
        else {
            panic!("missing retained event");
        };
        assert_eq!(number, expected);
        if range.contains(&number) {
            assert_eq!(tick, observed);
            if matches!(event, Event::Start { pid: found, .. } if found == pid) {
                found_child = true;
            }
        }
    }
    assert!(found_child);
    drop(child.child.stdin.take());
    assert!(child.wait(Duration::from_secs(1)).success());
}

struct Fixture {
    run: Run,
    reader: Reader,
    output: io::Cursor<Box<[u8]>>,
    origin: ObservationTimes,
    _recording: TestRecording,
}

struct Clocks {
    started_at: u64,
    finished_at: u64,
}

impl Clocks {
    fn at(now: u64) -> Self {
        Self {
            started_at: now,
            finished_at: now,
        }
    }
}

impl Fixture {
    fn new(name: &str) -> Self {
        let timebase = Timebase::read().unwrap();
        let recording = TestRecording::new(name);
        let mut output = io::Cursor::new(vec![0; 65_536].into_boxed_slice());
        let run = Run::start_with(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &recording.ring.place,
            &recording.root,
            &mut output,
            |_root, timebase| slow_persister(timebase, 3),
        )
        .unwrap();
        let mut reader = Reader::open(&recording.ring.place.lock_path).unwrap();
        assert_eq!(
            reader.read().unwrap(),
            Found::Publication {
                publication: 1,
                tick: 0
            }
        );
        let origin = recording::observation_times(timebase).unwrap();
        Self {
            run,
            reader,
            output,
            origin,
            _recording: recording,
        }
    }

    fn deadline(&self, tick: u64) -> u64 {
        self.run.schedule.deadline(tick)
    }

    fn period(&self) -> u64 {
        self.run.timebase.ticks(TEST_TICK_PERIOD_NS).unwrap()
    }

    fn observation(&self, elapsed_ns: u64) -> ObservationTimes {
        ObservationTimes {
            wall_ns: self.origin.wall_ns + elapsed_ns,
            continuous_ns: self.origin.continuous_ns + elapsed_ns,
            awake_ns: self.origin.awake_ns + elapsed_ns,
        }
    }

    fn tick(&mut self, requested: u64, clocks: &Clocks, times: Option<ObservationTimes>) -> Step {
        let now = Cell::new(clocks.started_at);
        self.run
            .tick_with_observation(
                requested,
                &mut self.output,
                || now.replace(clocks.finished_at),
                |_| times.ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData)),
            )
            .unwrap()
    }

    fn assert_publication(&mut self, found: &Found, times: Option<&ObservationTimes>) {
        assert_eq!(&self.reader.read().unwrap(), found);
        assert_eq!(self.reader.tick().times.as_ref(), times);
    }

    fn report(&self) -> &str {
        let count = usize::try_from(self.output.position()).unwrap();
        std::str::from_utf8(&self.output.get_ref()[..count]).unwrap()
    }
}
