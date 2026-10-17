#[cfg(test)]
#[path = "../src/bin/recording_check/capture.rs"]
mod capture;
#[cfg(test)]
#[path = "../src/bin/recording_check/compare.rs"]
mod compare;
#[cfg(test)]
#[path = "encoder_fixture/mod.rs"]
pub mod fixture;
#[cfg(test)]
#[path = "../src/bin/recording_check/rows.rs"]
mod rows;

#[cfg(test)]
mod tests {
    use super::{capture, compare, fixture, rows};
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use collector::recording::private;
    use collector::ring::{Identity, ObservationTimes, RunIdentity, Shape, Tick};
    use collector::tracker::{Event, Noticed};
    use persistence::{
        Begin, Encoder, Limits, Losses, Manifest, NumberedEvent, Observation, Store, TimeBounds,
        Window, WindowIdentity,
    };

    struct Recording(PathBuf);
    struct Frame {
        publication: u64,
        tick: Tick,
        events: Vec<Event>,
    }

    #[test]
    fn refuses_identity_sources_before_recording_a_numeric_only_reference() {
        // Goal: a successful fidelity reference cannot silently omit a v4 identity section.
        // Method: capture an orderly completed v4 source before creating any reference file.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let place = collector::ring::Place {
            lock_path: recording.0.join("ring.lock"),
            name_prefix: format!("/fdfid{}", std::process::id()),
        };
        let identity = RunIdentity {
            recording: store.recording(),
            run: Identity([31, 32]),
            boot: Identity([41, 42]),
        };
        let mut writer = collector::ring::Writer::create_with_identity(
            &place,
            fixture::profile(false),
            collector::ring::DictionaryCapacity {
                entries: 1,
                arena_bytes: 32,
            },
            &identity,
            1,
        )
        .unwrap();
        writer.finish();
        let reference = recording.0.join("source.reference");

        let result = capture::run(&place.lock_path, &reference, 1);

        collector::ring::unlink_generation(writer.name()).unwrap();
        assert!(matches!(
            result,
            Err(persistence::Error::Bound("identity source preservation"))
        ));
        assert!(!reference.try_exists().unwrap());
    }

    #[test]
    fn every_source_field_survives_encoding_against_the_independent_reference() {
        // Goal: field swaps and invented availability fail independently of encoder mappings.
        // Method: serialize source ticks/events before staging, then compare all ten disk tables.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let shape = fixture::profile(false);
        let identity = WindowIdentity {
            run: RunIdentity {
                recording: store.recording(),
                run: Identity([31, 32]),
                boot: Identity([41, 42]),
            },
            window: 0,
        };
        let frames = [frame(&shape, 0), frame(&shape, 1)];
        let reference = recording.0.join("source.reference");
        write_reference(&reference, &identity.run, &frames);

        let window = stage(&identity, &shape, &frames);
        let Begin::Pending(pending) = store.begin(&identity).unwrap() else {
            panic!("fresh fidelity fixture was committed");
        };
        let tables = Encoder.encode(&window, pending.path()).unwrap();
        store
            .commit(
                pending,
                Manifest {
                    identity,
                    publication_first: 17,
                    publication_end: 19,
                    events_first: 101,
                    events_end: 111,
                    wall_ns: Some(TimeBounds {
                        min: 50_000_000_111,
                        max: 50_000_001_111,
                    }),
                    continuous_ns: Some(TimeBounds {
                        min: 100_000_000_333,
                        max: 100_000_001_333,
                    }),
                    awake_ns: Some(TimeBounds {
                        min: 80_000_000_777,
                        max: 80_000_001_777,
                    }),
                    losses: Losses::default(),
                    data_directory: String::new(),
                    tables,
                },
            )
            .unwrap();

        compare::run(&recording.0, &reference).unwrap();
    }

    impl Recording {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            Self(PathBuf::from(format!(
                "/private/tmp/flightdeck-fidelity-{}-{serial}",
                std::process::id()
            )))
        }
    }

    impl Drop for Recording {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn frame(shape: &Shape, index: u64) -> Frame {
        let mut measured = *shape;
        if index == 1 {
            measured.core_capacity = 0;
            measured.state_capacity = 0;
            measured.fan_capacity = 0;
        }
        let mut tick = fixture::tick(&measured);
        tick.run_tick = index + 4;
        tick.times = Some(ObservationTimes {
            wall_ns: 50_000_000_111 + index * 1000,
            continuous_ns: 100_000_000_333 + index * 1000,
            awake_ns: 80_000_000_777 + index * 1000,
        });
        tick.processes[0].usage.user_ns = 0;
        if index == 1 {
            tick.processes[0].usage.awake_ns_at_start = 80_000_001_000;
            tick.ioreport_read = false;
            tick.smc_read = false;
        }
        let events = vec![
            Event::Start {
                pid: 9001,
                coalition: Some(17),
            },
            Event::Exec { pid: 9001 },
            Event::Exit {
                pid: 9001,
                last: Ok(fixture::usage(101)),
                noticed: Noticed::OnExit,
            },
            Event::Exit {
                pid: 9002,
                last: Err(collector::processes::Unreadable::Refused),
                noticed: Noticed::AlreadyExited,
            },
            Event::Exit {
                pid: 9003,
                last: Err(collector::processes::Unreadable::Gone),
                noticed: Noticed::AlreadyExited,
            },
        ];
        tick.events = 101 + index * 5..106 + index * 5;
        Frame {
            publication: 17 + index,
            tick,
            events,
        }
    }

    fn write_reference(path: &std::path::Path, identity: &RunIdentity, frames: &[Frame]) {
        let mut file = private::open_private_file(path, true).unwrap();
        writeln!(
            file,
            "flightdeck-reference-1 {} {} {} {} {} {}",
            identity.recording.0[0],
            identity.recording.0[1],
            identity.run.0[0],
            identity.run.0[1],
            identity.boot.0[0],
            identity.boot.0[1]
        )
        .unwrap();
        for frame in frames {
            writeln!(
                file,
                "begin {} {}",
                frame.publication,
                frame.tick.times.unwrap().wall_ns
            )
            .unwrap();
            rows::tick(&mut file, frame.publication, &frame.tick).unwrap();
            for (index, event) in frame.events.iter().enumerate() {
                rows::event(
                    &mut file,
                    frame.publication,
                    frame.tick.events.start + u64::try_from(index).unwrap(),
                    frame.tick.run_tick,
                    event,
                )
                .unwrap();
            }
        }
        writeln!(file, "complete {}", frames.len()).unwrap();
    }

    fn stage(identity: &WindowIdentity, shape: &Shape, frames: &[Frame]) -> Window {
        let mut window = Window::new(*identity, Limits::from_shape(shape).unwrap());
        for frame in frames {
            let times = frame.tick.times.unwrap();
            let events: Vec<_> = frame
                .events
                .iter()
                .enumerate()
                .map(|(index, event)| NumberedEvent {
                    number: frame.tick.events.start + u64::try_from(index).unwrap(),
                    tick: frame.tick.run_tick,
                    event,
                })
                .collect();
            window
                .push_tick(
                    &Observation {
                        publication: frame.publication,
                        wall_ns: Some(times.wall_ns),
                        continuous_ns: Some(times.continuous_ns),
                        awake_ns: Some(times.awake_ns),
                    },
                    &frame.tick,
                    &events,
                )
                .unwrap();
        }
        window
    }
}
