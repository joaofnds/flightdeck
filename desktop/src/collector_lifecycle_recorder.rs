use std::io;
use std::path::Path;

use collector::recording::control::{self, Control, Health, Role, State};
use collector::recording::identity_text;

use super::policy::{Observation, RecordingStatus, Source};

pub(super) struct Recorder {
    controller: io::Result<Worker>,
    collector: io::Result<Worker>,
    persister: io::Result<Worker>,
    authority: String,
    now_ns: u64,
}

struct Worker {
    health: Health,
    alive: bool,
    fresh: bool,
}

const HEARTBEAT_LIMIT_NS: u64 = 5_000_000_000;

impl Recorder {
    pub fn read(root: &Path, previous: Option<&Self>) -> io::Result<Option<Self>> {
        let control = match Control::open(root) {
            Ok(control) => control,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let now_ns = control::now()?;
        Ok(Some(Self {
            controller: Worker::read(
                &control,
                Role::Controller,
                now_ns,
                previous.map(|recorder| &recorder.controller),
            ),
            collector: Worker::read(
                &control,
                Role::Collector,
                now_ns,
                previous.map(|recorder| &recorder.collector),
            ),
            persister: Worker::read(
                &control,
                Role::Persister,
                now_ns,
                previous.map(|recorder| &recorder.persister),
            ),
            authority: identity_text(control.authority()).to_string(),
            now_ns,
        }))
    }

    pub fn source(&self) -> io::Result<Source> {
        let observation = match &self.collector {
            Ok(collector) if collector.running() && collector.health.publication > 0 => {
                Some(Observation {
                    pid: u32::try_from(collector.health.pid).map_err(io::Error::other)?,
                    run: identity_text(collector.health.run).to_string(),
                    boot: None,
                    authority: Some(self.authority.clone()),
                    publication: collector.health.publication,
                })
            }
            _ => None,
        };
        let controller_missing = matches!(&self.controller, Ok(controller) if !controller.alive);
        let controller_pid = match &self.controller {
            Ok(controller) if controller.alive => {
                Some(u32::try_from(controller.health.pid).map_err(io::Error::other)?)
            }
            _ => None,
        };
        Ok(Source {
            recording: if controller_missing {
                RecordingStatus::Ready
            } else {
                RecordingStatus::Pending
            },
            producer_alive: matches!(&self.collector, Ok(collector) if collector.running()),
            controller_pid,
            controller_missing,
            controller_unavailable: self.controller.is_err(),
            observation,
            ..Source::default()
        })
    }

    pub fn notice(&self) -> String {
        let acquisition = self
            .collector
            .as_ref()
            .map_or_else(|error| format!("unavailable; {error}"), Worker::description);
        let durable = self
            .persister
            .as_ref()
            .map_or_else(|error| format!("unavailable; {error}"), Worker::description);
        let recovery = self.notice_recovery();
        format!("Acquisition: {acquisition} · Durable recording: {durable} · Recovery: {recovery}")
    }

    fn notice_recovery(&self) -> String {
        let controller = match &self.controller {
            Ok(controller) => controller,
            Err(error) => return format!("unavailable; controller health: {error}"),
        };
        if !controller.alive {
            "unavailable; restart the recorder to restore automatic recovery".into()
        } else if !controller.fresh {
            "unavailable; controller heartbeat is stale".into()
        } else if controller.health.state == State::Retrying {
            let next_seconds = controller
                .health
                .next_attempt_ns
                .saturating_sub(self.now_ns)
                .div_ceil(1_000_000_000);
            format!(
                "retry {} in {next_seconds}s ({:?}, error {})",
                controller.health.retries, controller.health.failure, controller.health.error
            )
        } else if controller.health.state == State::Running {
            "available".into()
        } else {
            controller.description()
        }
    }
}

impl Worker {
    fn read(
        control: &Control,
        role: Role,
        now_ns: u64,
        previous: Option<&io::Result<Self>>,
    ) -> io::Result<Self> {
        let health = match (control.read(role), previous) {
            (Ok(health), _) => health,
            // A publication caught between its two sequence stores says nothing about the worker,
            // so its last complete record stands. That record's process and heartbeat are judged
            // afresh below, so a writer that died mid-publication still goes missing or stale.
            (Err(error), Some(Ok(worker))) if error.kind() == io::ErrorKind::WouldBlock => {
                worker.health
            }
            (Err(error), _) => return Err(error),
        };
        let alive = health.alive()?;
        let fresh = now_ns
            .checked_sub(health.heartbeat_ns)
            .is_some_and(|elapsed| elapsed <= HEARTBEAT_LIMIT_NS);
        Ok(Self {
            health,
            alive,
            fresh,
        })
    }

    fn running(&self) -> bool {
        self.alive && self.fresh && self.health.state == State::Running
    }

    fn description(&self) -> String {
        if !self.alive {
            return match self.health.state {
                State::Stopped => "stopped".into(),
                State::Absent => "not started".into(),
                _ => "unavailable; worker exited".into(),
            };
        }
        if !self.fresh {
            return "unavailable; heartbeat is stale".into();
        }
        match self.health.state {
            State::Running if self.health.publication > 0 => {
                format!("running; reading {}", self.health.publication)
            }
            State::Running => "running; awaiting first progress".into(),
            State::Starting => "starting".into(),
            State::Retrying => "retrying".into(),
            State::Failed => format!(
                "failed ({:?}, error {})",
                self.health.failure, self.health.error
            ),
            State::Stopping => "stopping".into(),
            State::Stopped => "stopped".into(),
            State::Absent => "unavailable".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collector::recording::private::create_private_dir;
    use collector::recording::{identity_text, random_identity};
    use std::os::unix::fs::FileExt;
    use std::path::PathBuf;

    struct RefusedControl {
        root: PathBuf,
        control: Control,
    }

    impl RefusedControl {
        /// A control file whose controller publication stands between its two sequence stores.
        fn new() -> Self {
            let token = identity_text(random_identity().unwrap());
            let root = std::env::temp_dir().join(format!("flightdeck-refused-control-{token}"));
            create_private_dir(&root).unwrap();
            drop(Control::create(&root).unwrap());
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(root.join("recorder.control"))
                .unwrap();
            file.write_all_at(&1_u64.to_ne_bytes(), 16 * 8).unwrap();
            let control = Control::open(&root).unwrap();
            assert_eq!(
                control.read(Role::Controller).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            Self { root, control }
        }
    }

    impl Drop for RefusedControl {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    fn healthy(health: Health) -> Worker {
        Worker {
            health,
            alive: true,
            fresh: true,
        }
    }

    #[test]
    fn a_refused_read_finds_the_writer_of_the_kept_record_gone() {
        // Goal: keeping a record across a refused read cannot keep a dead writer looking alive.
        // Method: keep a record marked alive whose process has exited, and refuse the read.
        let fixture = RefusedControl::new();
        let mut exited = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = i32::try_from(exited.id()).unwrap();
        assert!(exited.wait().unwrap().success());
        let own = Health::own().unwrap();
        let previous = Ok(healthy(Health { pid, ..own }));

        let worker = Worker::read(
            &fixture.control,
            Role::Controller,
            own.heartbeat_ns,
            Some(&previous),
        )
        .unwrap();

        assert!(!worker.alive);
    }

    #[test]
    fn a_refused_read_finds_the_heartbeat_of_the_kept_record_stale() {
        // Goal: keeping a record across a refused read cannot keep a silent writer looking fresh.
        // Method: keep a record marked fresh, refuse the read just past the heartbeat's limit.
        let fixture = RefusedControl::new();
        let own = Health::own().unwrap();
        let previous = Ok(healthy(own));

        let worker = Worker::read(
            &fixture.control,
            Role::Controller,
            own.heartbeat_ns + HEARTBEAT_LIMIT_NS + 1,
            Some(&previous),
        )
        .unwrap();

        assert!(worker.alive);
        assert!(!worker.fresh);
    }

    #[test]
    fn a_refused_read_with_no_kept_record_stays_refused() {
        // Goal: with nothing to keep, a refused read is reported rather than invented.
        // Method: refuse the first read of a controller.
        let fixture = RefusedControl::new();

        let result = Worker::read(&fixture.control, Role::Controller, 0, None);

        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::WouldBlock);
    }
}
