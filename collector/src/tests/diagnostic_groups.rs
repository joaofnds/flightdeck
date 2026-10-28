use super::*;

const TEST: &str = "tests::diagnostic_groups::delivers_a_co_ending_minute_through_group_admission";
const OVERSHOOT_TEST: &str =
    "tests::diagnostic_groups::finishes_an_overdue_run_at_its_last_publication";
const ROOT: &str = "FLIGHTDECK_GROUP_TEST_ROOT";
const RING: &str = "FLIGHTDECK_GROUP_TEST_RING";

#[test]
fn delivers_a_co_ending_minute_through_group_admission() {
    // Goal: a minute's co-ending tick and recording reports survive actual group admission.
    // Method: run the collector orchestration on its existing accelerated test schedule.
    minute_case(false);
}

#[test]
fn finishes_an_overdue_run_at_its_last_publication() {
    // Goal: finite overshoot ends after one acquisition and reports the actual final ordinal.
    // Method: give the real run a schedule already overdue at startup, without delaying a wait.
    minute_case(true);
}

fn minute_case(overdue: bool) {
    if let Some(root) = std::env::var_os(ROOT) {
        minute_child(&PathBuf::from(root), overdue);
    }
    let name = if overdue {
        "diagnosticovershoot"
    } else {
        "diagnosticminute"
    };
    let test = if overdue { OVERSHOOT_TEST } else { TEST };
    let ring = TestRing::new(name);
    let root = std::env::temp_dir().join(format!("flightdeck-{name}-{}", std::process::id()));
    recording::private::create_private_dir(&root).unwrap();
    eprintln!(
        "minute group fixture retained on failure: {}",
        root.display()
    );
    let mut child = process::OwnedChild::spawn(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(ROOT, &root)
            .env(RING, &ring.place.lock_path)
            .stdout(std::fs::File::create(root.join("stdout")).unwrap())
            .stderr(std::fs::File::create(root.join("stderr")).unwrap()),
    );

    let status = child.wait(Duration::from_secs(5));

    assert_eq!(Some(29), status.code());
    let output = std::fs::read_to_string(root.join("stdout")).unwrap();
    let mut reader = Reader::open(&ring.place.lock_path).unwrap();
    let mut last_tick = 0;
    let mut publications = 0;
    for _publication in 0..RING_SLOTS {
        let found = reader.read().unwrap();
        if let Found::Publication { tick, .. } = found {
            last_tick = tick;
            publications += 1;
        } else {
            assert_eq!(found, Found::Nothing);
            break;
        }
    }
    assert!(last_tick > 0);
    assert_minute_pair(&output, last_tick);
    let whole = output
        .lines()
        .find(|line| line.starts_with("whole run, ticks "))
        .unwrap();
    assert_eq!(report_range(whole, "whole run, ticks ").1, last_tick);
    assert!(
        collector::ring::RingReader::open(&ring.place.lock_path)
            .unwrap()
            .completed()
    );
    if overdue {
        assert_eq!(publications, 2);
        assert!(last_tick > TICKS_PER_MINUTE);
    }
    if std::env::var_os("FLIGHTDECK_DIAGNOSTIC_RETAIN").is_none() {
        std::fs::remove_dir_all(&root).unwrap();
    }
}

fn minute_child(root: &Path, overdue: bool) -> ! {
    let overdue_schedule = Schedule::starting_now(1).unwrap();
    diagnostics::install(Role::Collector);
    let lock = PathBuf::from(std::env::var_os(RING).unwrap());
    let name = lock.file_stem().unwrap().to_str().unwrap();
    let options = Options {
        seconds: NonZeroU64::new(TICKS_PER_MINUTE),
        rescan_interval: DEFAULT_RESCAN_INTERVAL,
        ring_name: name.to_owned(),
        recording: Some(root.join("recording")),
        parent_pid: None,
    };
    let owner = owner::Owner::bind(None).unwrap();
    let mut out = diagnostics::stdout_group();
    {
        let place = Place::for_user(name).unwrap();
        let mut run = Run::start_with(
            Timebase::read().unwrap(),
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &place,
            options.recording.as_ref().unwrap(),
            &mut *out,
            |_root, timebase| {
                Persister::spawn_command(&mut std::process::Command::new("/usr/bin/true"), timebase)
            },
        )
        .unwrap();
        out.submit();
        if overdue {
            assert!(overdue_schedule.first_ahead_of(clock::now()) > TICKS_PER_MINUTE + 1);
            run.schedule = overdue_schedule;
        }

        run_collect(
            &options,
            owner,
            &mut run,
            Err(io::ErrorKind::NotFound.into()),
            &mut out,
        )
        .unwrap();
        out.submit();
    }
    drop(out);
    drop(options);
    diagnostics::exit(29);
}

fn assert_minute_pair(output: &str, last_tick: u64) {
    let lines: Vec<_> = output.lines().collect();
    let minute = lines
        .iter()
        .position(|line| line.starts_with("minute, ticks "))
        .unwrap_or_else(|| panic!("no admitted minute report: {output}"));
    let tick = lines[..minute]
        .iter()
        .rposition(|line| line.starts_with("ticks "))
        .unwrap();
    let minute_range = report_range(lines[minute], "minute, ticks ");
    let tick_range = report_range(lines[tick], "ticks ");

    assert_eq!(minute_range.1, tick_range.1);
    assert!(minute_range.0 <= tick_range.0);
    assert_eq!(minute_range.1, last_tick);
    assert_eq!(minute, tick + 3);
    let recording = lines[minute + 1..]
        .iter()
        .position(|line| line.starts_with("recording minute elapsed_ns="))
        .map(|index| minute + 1 + index)
        .unwrap();
    assert_eq!(minute + 3, recording);
    assert!(lines[recording + 1].starts_with("recording cumulative collector_cpu_ns="));
    assert!(lines[recording + 2].starts_with("identity minute "));
    assert!(!output.contains("diagnostic loss"), "{output}");
    eprintln!("admitted minute range={minute_range:?} co_ending_ticks={tick_range:?}");
}

fn report_range(line: &str, prefix: &str) -> (u64, u64) {
    let range = line
        .strip_prefix(prefix)
        .unwrap()
        .split(':')
        .next()
        .unwrap();
    let (first, last) = range.split_once('-').unwrap();
    (first.parse().unwrap(), last.parse().unwrap())
}
