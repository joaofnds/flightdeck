use super::*;

#[path = "../../tests/support/process.rs"]
mod process;

const TEST: &str = "tests::diagnostic_groups::delivers_a_co_ending_minute_through_group_admission";
const ROOT: &str = "FLIGHTDECK_GROUP_TEST_ROOT";
const RING: &str = "FLIGHTDECK_GROUP_TEST_RING";

#[test]
fn delivers_a_co_ending_minute_through_group_admission() {
    // Goal: a minute's co-ending tick and recording reports survive actual group admission.
    // Method: run the collector orchestration on its existing accelerated test schedule.
    if let Some(root) = std::env::var_os(ROOT) {
        minute_child(&PathBuf::from(root));
    }
    let ring = TestRing::new("diagnosticminute");
    let root = std::env::temp_dir().join(format!("flightdeck-minute-group-{}", std::process::id()));
    recording::private::create_private_dir(&root).unwrap();
    eprintln!(
        "minute group fixture retained on failure: {}",
        root.display()
    );
    let mut child = process::OwnedChild::spawn(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(ROOT, &root)
            .env(RING, &ring.place.lock_path)
            .stdout(std::fs::File::create(root.join("stdout")).unwrap())
            .stderr(std::fs::File::create(root.join("stderr")).unwrap()),
    );

    let status = child.wait(Duration::from_secs(5));

    assert_eq!(Some(29), status.code());
    let output = std::fs::read_to_string(root.join("stdout")).unwrap();
    assert_minute_pair(&output);
    if std::env::var_os("FLIGHTDECK_DIAGNOSTIC_RETAIN").is_none() {
        std::fs::remove_dir_all(&root).unwrap();
    }
}

fn minute_child(root: &Path) -> ! {
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

fn assert_minute_pair(output: &str) {
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
    assert!(minute_range.1 <= TICKS_PER_MINUTE);
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
