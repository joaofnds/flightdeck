use std::time::Duration;

pub(super) const DISCOVERY_LIMIT: Duration = Duration::from_secs(20);
pub(super) const STARTUP_LIMIT: Duration = Duration::from_secs(20);
pub(super) const SHUTDOWN_LIMIT: Duration = Duration::from_secs(125);
pub(super) const KILL_LIMIT: Duration = Duration::from_secs(2);
pub(super) const UNANNOUNCED_FAILURE: &str = "A collector holds this recording without naming \
     its build, since it predates this Flightdeck or could not register its run. Quit the \
     Flightdeck that started it, then retry.";
pub(super) const UNRECOGNIZED_FAILURE: &str = "The running collector announces itself in a \
     format another version of Flightdeck writes. Quit the Flightdeck that started it, then retry.";

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Mode {
    Attach,
    Auto,
    Explicit,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum State {
    Discovering,
    Starting,
    Attached,
    Running,
    Stopping,
    Killing,
    Stopped,
    Failed,
}

#[cfg(test)]
#[path = "collector_lifecycle_policy_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Action {
    None,
    Spawn,
    Terminate,
    Kill,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Observation {
    pub pid: u32,
    pub run: String,
    pub boot: String,
    pub publication: u64,
}

#[derive(Default)]
pub(super) struct Source {
    pub recording_route: bool,
    pub recording: RecordingStatus,
    pub producer_alive: bool,
    pub build: Build,
    pub observation: Option<Observation>,
}

/// What the collector holding the recording says built it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum Build {
    /// No collector holds the recording, or the collector this app follows is built from its
    /// code.
    #[default]
    Expected,
    /// A collector holds the recording without announcing the newest registered run.
    Unannounced,
    /// A collector holds the recording and announces it in another version's format.
    Unrecognized,
    Other(u64),
}

pub(super) fn other_build_failure(build: u64) -> String {
    format!(
        "The running collector is built from other code than this Flightdeck (build {build:016x}, \
         expected {:016x}). Quit the Flightdeck that started it, then retry.",
        collector::recording::BUILD
    )
}

/// The collector this app started is a stale executable, which only a rebuild replaces.
pub(super) fn owned_build_failure(build: u64) -> String {
    format!(
        "The collector beside this Flightdeck is built from other code (build {build:016x}, \
         expected {:016x}). Rebuild it together with this Flightdeck, then retry.",
        collector::recording::BUILD
    )
}

#[derive(Clone, Copy, Default, PartialEq)]
pub(super) enum RecordingStatus {
    #[default]
    Pending,
    Missing,
    Ready,
}

pub(super) struct Policy {
    pub mode: Mode,
    pub state: State,
    pub owned_pid: Option<u32>,
    pub failure: Option<String>,
    since: Option<Duration>,
    previous: Option<Observation>,
    stopping: bool,
}

impl Policy {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            state: if mode == Mode::Attach {
                State::Attached
            } else {
                State::Discovering
            },
            owned_pid: None,
            failure: None,
            since: None,
            previous: None,
            stopping: false,
        }
    }

    pub fn step(&mut self, now: Duration, source: &Source) -> Action {
        match self.state {
            State::Discovering => self.discover(now, source),
            State::Starting => self.starting(now, source),
            State::Attached => self.attached(now, source),
            State::Stopping => {
                if self.expired(now, SHUTDOWN_LIMIT) {
                    self.failure = Some("Collector exceeded its 125-second saving deadline".into());
                    self.state = State::Killing;
                    self.since = Some(now);
                    return Action::Kill;
                }

                Action::None
            }
            State::Killing => {
                if self.expired(now, KILL_LIMIT) {
                    self.failure = Some("Owned collector did not exit after SIGKILL".into());
                    self.state = State::Stopped;
                }
                Action::None
            }
            _ => Action::None,
        }
    }

    fn discover(&mut self, now: Duration, source: &Source) -> Action {
        // An explicit start collects whatever page is open, where Auto waits for a readings
        // route.
        if self.since.is_none() {
            if self.mode == Mode::Auto {
                if !source.recording_route {
                    return Action::None;
                }
            }
        }

        if self.since.is_none() {
            self.since = Some(now);
        }
        if let Build::Other(build) = source.build {
            self.fail(&other_build_failure(build));
            return Action::None;
        }

        if source.build == Build::Unrecognized {
            self.fail(UNRECOGNIZED_FAILURE);
            return Action::None;
        }

        if self.expired(now, DISCOVERY_LIMIT) {
            if source.build == Build::Unannounced {
                self.fail(UNANNOUNCED_FAILURE);
            } else {
                if source.producer_alive {
                    self.fail(
                        "Existing collector is alive but readings did not advance within \
                         20 seconds",
                    );
                } else {
                    self.fail(
                        "Recording discovery did not finish within 20 seconds; retry to reconnect",
                    );
                }
            }
            return Action::None;
        }

        if source.producer_alive {
            if self.advanced(source.observation.as_ref()) {
                self.state = State::Attached;
                self.since = Some(now);
            }

            return Action::None;
        }

        if source.recording == RecordingStatus::Missing {
            self.state = State::Starting;
            return Action::Spawn;
        }

        if source.recording == RecordingStatus::Ready {
            self.state = State::Starting;
            return Action::Spawn;
        }

        Action::None
    }

    fn attached(&mut self, now: Duration, source: &Source) -> Action {
        if self.mode == Mode::Attach {
            return Action::None;
        }

        if !source.producer_alive {
            self.fail("Shared collector stopped; retry to resume recording");
            return Action::None;
        }

        if self.advanced(source.observation.as_ref()) {
            self.since = Some(now);
        } else {
            if self.expired(now, DISCOVERY_LIMIT) {
                self.fail(
                    "Shared collector readings did not advance within 20 seconds; \
                     retry to reconnect",
                );
            }
        }

        Action::None
    }

    fn starting(&mut self, now: Duration, source: &Source) -> Action {
        assert!(self.since.is_some());
        if let Build::Other(build) = source.build {
            self.failure = Some(owned_build_failure(build));
            return self.stop(now);
        }

        if source.build == Build::Unrecognized {
            self.failure = Some(UNRECOGNIZED_FAILURE.into());
            return self.stop(now);
        }

        if self.expired(now, STARTUP_LIMIT) {
            self.failure =
                Some("Owned collector readings did not advance within 20 seconds".into());
            return self.stop(now);
        }

        if let Some(observation) = &source.observation {
            if Some(observation.pid) == self.owned_pid {
                if self.advanced(source.observation.as_ref()) {
                    self.state = State::Running;
                    return Action::None;
                }
            }
        }

        Action::None
    }

    fn advanced(&mut self, observation: Option<&Observation>) -> bool {
        let previous = self.previous.take();
        self.previous = observation.cloned();
        match (previous, observation) {
            (Some(previous), Some(current)) => {
                if previous.pid != current.pid {
                    return false;
                }

                if previous.run != current.run {
                    return false;
                }

                if previous.boot != current.boot {
                    return false;
                }

                previous.publication < current.publication
            }
            _ => false,
        }
    }

    pub fn launched(&mut self, pid: u32, now: Duration) {
        assert!(pid > 0);
        assert!(self.owned_pid.is_none());
        self.owned_pid = Some(pid);
        self.state = State::Starting;
        self.since = Some(now);
        self.previous = None;
    }

    pub fn exited(&mut self, message: &str, successful: bool) {
        self.owned_pid = None;
        if self.stopping {
            if !successful {
                if self.failure.is_none() {
                    self.failure = Some(message.chars().take(4096).collect());
                }
            }
            self.state = if self.failure.is_some() {
                State::Failed
            } else {
                State::Stopped
            };
        } else {
            self.failure = Some(message.chars().take(4096).collect());
            self.state = State::Failed;
        }
    }

    pub fn stop(&mut self, now: Duration) -> Action {
        self.stopping = true;
        if self.owned_pid.is_none() {
            self.state = State::Stopped;
            return Action::None;
        }

        if matches!(
            self.state,
            State::Stopping | State::Killing | State::Stopped
        ) {
            return Action::None;
        }

        self.state = State::Stopping;
        self.since = Some(now);
        Action::Terminate
    }

    pub fn fail(&mut self, message: &str) {
        self.failure = Some(message.chars().take(4096).collect());
        self.state = State::Failed;
    }

    pub fn retry(&mut self) {
        if self.can_retry() {
            self.state = State::Discovering;
            self.failure = None;
            self.since = None;
            self.previous = None;
            self.stopping = false;
        }
    }

    pub fn can_retry(&self) -> bool {
        if self.state != State::Failed {
            return false;
        }

        self.owned_pid.is_none()
    }

    pub fn needs_health(&self, recording_route: bool) -> bool {
        if self.state == State::Starting {
            return true;
        }

        if self.state == State::Attached {
            return self.mode != Mode::Attach;
        }

        if self.state != State::Discovering {
            return false;
        }

        if self.mode == Mode::Explicit {
            return true;
        }

        if self.mode != Mode::Auto {
            return false;
        }

        recording_route || self.since.is_some()
    }

    pub fn observed_publication(&self) -> Option<u64> {
        self.previous.as_ref().map(|observed| observed.publication)
    }

    fn expired(&self, now: Duration, limit: Duration) -> bool {
        self.since
            .is_some_and(|since| now.saturating_sub(since) >= limit)
    }
}
