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
    pub fn read(root: &Path) -> io::Result<Option<Self>> {
        let control = match Control::open(root) {
            Ok(control) => control,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let now_ns = control::now()?;
        Ok(Some(Self {
            controller: Worker::read(&control, Role::Controller, now_ns),
            collector: Worker::read(&control, Role::Collector, now_ns),
            persister: Worker::read(&control, Role::Persister, now_ns),
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
    fn read(control: &Control, role: Role, now_ns: u64) -> io::Result<Self> {
        let health = control.read(role)?;
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
