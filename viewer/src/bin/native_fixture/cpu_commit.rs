use std::io;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use collector::machine::{CPUTicks, Memory};
use collector::ring::{Dropped, Found, ObservationTimes, Readings, RingReader};
use persistence::{Limits, Losses, Window, WindowIdentity};

pub(super) fn run(checks: &Path) -> io::Result<()> {
    let mut fixture = super::Fixture::new()?;
    fixture.publish_cpu_checks()?;
    let window = captured_window(&fixture)?;
    publish_future(&mut fixture)?;
    let mut check = Check {
        child: Some(
            Command::new(checks)
                .arg("--live-cpu-delayed-fixture")
                .arg(&fixture.root)
                .spawn()?,
        ),
    };
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut sealed = false;
    for _attempt in 0..2500 {
        if !sealed {
            if fixture.root.join("cpu-commit-request").try_exists()? {
                seal(&fixture.root, &window)?;
                std::fs::write(fixture.root.join("cpu-commit-ready"), [])?;
                sealed = true;
            }
        }

        let child = check.child.as_mut().unwrap_or_else(|| unreachable!());
        if let Some(status) = child.try_wait()? {
            drop(check.child.take());
            if !status.success() {
                return Err(io::Error::other(format!(
                    "delayed CPU check failed: {status}"
                )));
            }

            return Ok(());
        }

        if Instant::now() >= deadline {
            break;
        }

        std::thread::sleep(Duration::from_millis(10));
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "delayed CPU check deadline",
    ))
}

fn captured_window(fixture: &super::Fixture) -> io::Result<Window> {
    let writer = fixture
        .writer
        .as_ref()
        .ok_or_else(|| io::Error::other("CPU writer"))?;
    let mut reader = RingReader::open_generation(writer.name())?;
    let mut tick = persistence::tick_buffer(&reader.shape()).map_err(io::Error::other)?;
    let mut window = Window::new(
        WindowIdentity {
            run: writer
                .identity()
                .ok_or_else(|| io::Error::other("CPU run identity"))?,
            window: 0,
        },
        Limits::from_shape(&reader.shape()).map_err(io::Error::other)?,
    );
    for _attempt in 0..4 {
        match reader.read_into(&mut tick)? {
            Found::Nothing => break,
            Found::Missed { .. } => return Err(io::Error::other("CPU fixture overwritten")),
            Found::Publication { publication, .. } => {
                let times = tick
                    .times
                    .ok_or_else(|| io::Error::other("CPU fixture times"))?;
                window
                    .push_tick(
                        &persistence::Observation {
                            publication,
                            wall_ns: Some(times.wall_ns),
                            continuous_ns: Some(times.continuous_ns),
                            awake_ns: Some(times.awake_ns),
                        },
                        &tick,
                        &[],
                    )
                    .map_err(io::Error::other)?;
            }
        }
    }

    Ok(window)
}

fn publish_future(fixture: &mut super::Fixture) -> io::Result<()> {
    let cpus = [CPUTicks {
        user: 50,
        idle: 350,
        ..CPUTicks::default()
    }; 2];
    let writer = fixture
        .writer
        .as_mut()
        .ok_or_else(|| io::Error::other("CPU writer"))?;

    writer.publish_observed(
        &Readings {
            run_tick: 4,
            cpus: &cpus,
            memory: &Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        },
        &ObservationTimes {
            wall_ns: 2902 * 1_000_000_000,
            continuous_ns: 4 * 1_000_000_000,
            awake_ns: 4 * 1_000_000_000,
        },
    );

    Ok(())
}

fn seal(root: &Path, window: &Window) -> io::Result<()> {
    let store = persistence::Store::open(root).map_err(io::Error::other)?;
    let persistence::Begin::Pending(pending) =
        store.begin(&window.identity).map_err(io::Error::other)?
    else {
        return Err(io::Error::other("CPU fixture already sealed"));
    };

    let tables = persistence::Encoder
        .encode(window, pending.path())
        .map_err(io::Error::other)?;
    let publications = window.publication_range();
    let events = window.event_range();

    store
        .commit(
            pending,
            persistence::Manifest {
                identity: window.identity,
                publication_first: publications.start,
                publication_end: publications.end,
                events_first: events.start,
                events_end: events.end,
                wall_ns: None,
                continuous_ns: None,
                awake_ns: None,
                losses: Losses::default(),
                data_directory: String::new(),
                tables,
            },
        )
        .map(|_manifest| ())
        .map_err(io::Error::other)
}

struct Check {
    child: Option<Child>,
}

impl Drop for Check {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Err(error) = child.kill() {
                eprintln!("delayed CPU check termination: {error}");
            }

            let deadline = Instant::now() + Duration::from_secs(2);
            for _attempt in 0..200 {
                match child.try_wait() {
                    Ok(Some(_status)) => return,
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            break;
                        }

                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => {
                        eprintln!("delayed CPU check reap: {error}");
                        return;
                    }
                }
            }
            eprintln!("delayed CPU check did not reap within two seconds");
        }
    }
}
