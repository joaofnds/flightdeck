use std::collections::BTreeMap;
use std::path::PathBuf;

use collector::recording::{BUILD, Registration, identity_text, random_identity};
use collector::ring::{Identity, Place, Readings, Shape, Writer};
use serde_json::json;

use super::*;
use crate::live_source::Snapshot;

#[test]
fn paused_selection_does_not_freeze_owned_collector_health() {
    // Goal: inspecting publication one cannot terminate a healthy owned producer.
    // Method: publish twice to a real registered ring while presenting the first snapshot.
    let mut fixture = Recording::new();
    fixture.publish();
    let paused = fixture.snapshot();
    let mut session = fixture.session(Mode::Explicit);
    session
        .0
        .policy
        .launched(std::process::id(), Duration::ZERO);
    session.tick();
    fixture.publish();

    session.tick();

    assert!(session.ready());
    assert_eq!(paused.catalog["latest_observation"]["publication"], 1);
    session.0.started = Instant::now()
        .checked_sub(STARTUP_LIMIT + Duration::from_secs(1))
        .unwrap();
    session.tick();
    assert!(session.ready());
}

#[test]
fn opening_another_recording_does_not_change_auto_discovery_root() {
    // Goal: presented recordings cannot replace the configured collector's control plane.
    // Method: advance the configured ring while presenting another root's snapshot.
    let mut configured = Recording::new();
    let mut presented = Recording::new();
    configured.publish();
    presented.publish();
    let mut session = configured.session(Mode::Auto);
    session.tick();
    configured.publish();

    session.tick();

    assert_eq!(session.0.policy.state, State::Attached);
    assert_eq!(session.owned_pid(), None);
    session.begin_stop();
    assert!(session.finished());
}

#[test]
fn registry_rollover_requires_advancement_from_the_new_run() {
    // Goal: cached health mappings cannot mistake a new run's first publication for progress.
    // Method: replace a real registered writer between observations, then advance its new ring.
    let mut fixture = Recording::new();
    fixture.publish();
    let mut session = fixture.session(Mode::Auto);
    session.tick();
    fixture.rollover();
    fixture.publish();
    session.tick();
    assert_eq!(session.0.policy.state, State::Discovering);

    fixture.publish();
    session.tick();

    assert_eq!(session.0.policy.state, State::Attached);
    assert_eq!(session.owned_pid(), None);
}

#[test]
fn a_foreign_registered_identity_fails_without_spawning() {
    // Goal: another recording's registry metadata never becomes ownership or readiness evidence.
    // Method: change only the persisted recording identity and use the production health path.
    let mut fixture = Recording::new();
    fixture.publish();
    let mut registry = collector::recording::Registry::read(&fixture.root).unwrap();
    registry.runs[0].identity.recording = random_identity().unwrap();
    registry.write(&fixture.root).unwrap();
    let mut session = fixture.session(Mode::Auto);

    session.tick();

    assert!(session.failure().is_some());
    assert_eq!(session.0.policy.state, State::Failed);
    assert_eq!(session.owned_pid(), None);
}

#[test]
fn a_missing_ring_with_a_live_pid_stays_unknown_until_its_fixed_deadline() {
    // Goal: unavailable metadata from a live registration cannot launch a competing owner.
    // Method: unlink only this fixture's generation and cross discovery's absolute bound.
    let mut fixture = Recording::new();
    fixture.publish();
    collector::ring::unlink_generation(fixture.writer.as_ref().unwrap().name()).unwrap();
    let mut session = fixture.session(Mode::Auto);
    session.tick();
    session.0.started = Instant::now()
        .checked_sub(STARTUP_LIMIT + Duration::from_secs(1))
        .unwrap();

    session.tick();

    assert!(session.failure().is_some());
    assert_eq!(session.0.policy.state, State::Failed);
    assert_eq!(session.owned_pid(), None);
}

#[test]
fn a_completed_shared_collector_offers_recovery_without_taking_ownership() {
    // Goal: a shared producer stopping after attachment exposes an explicit recovery action.
    // Method: complete its real registered ring after two advancing observations.
    let mut recording = Recording::new();
    let mut session = recording.session(Mode::Auto);
    recording.publish();
    session.tick();
    recording.publish();
    session.tick();
    recording.writer.as_mut().unwrap().finish();

    session.tick();

    assert!(session.status().unwrap().1);
    assert_eq!(session.owned_pid(), None);
    session.begin_stop();
    assert!(session.finished());
}

#[test]
fn a_stalled_shared_collector_offers_recovery() {
    // Goal: shared health is independent of the selected observation.
    // Method: keep a real registered producer alive without publishing through the deadline.
    let mut recording = Recording::new();
    let mut session = recording.session(Mode::Auto);
    recording.publish();
    session.tick();
    recording.publish();
    session.tick();
    session.0.started = Instant::now()
        .checked_sub(policy::DISCOVERY_LIMIT + Duration::from_secs(1))
        .unwrap();

    session.tick();

    assert!(session.status().unwrap().1);
    assert_eq!(session.owned_pid(), None);
}

#[test]
fn a_registered_pid_is_not_alive_once_its_collector_lets_go_of_the_recording() {
    // Goal: a pid macOS gave another process cannot pass for the collector it registered.
    // Method: drop the writer unfinished, so the registered pid, this test's own, stays alive.
    let mut fixture = Recording::new();
    fixture.publish();
    drop(fixture.writer.take());

    let source = health::Health::new().read(&fixture.root, None).unwrap();

    assert!(!source.producer_alive);
    assert!(source.recording == RecordingStatus::Ready);
}

/// A build no code of this repository fingerprints to, standing for another checkout's.
const OTHER_BUILD: u64 = 7;

#[test]
fn a_collector_announcing_another_build_is_reported_and_not_read() {
    // Goal: the app learns that the running collector's code differs before reading its ring.
    // Method: replace the fixture's announcement with one naming its run and another build.
    let fixture = Recording::new();
    fixture.announce(fixture.run(), OTHER_BUILD);

    let source = health::Health::new().read(&fixture.root, None).unwrap();

    assert_eq!(policy::Build::Other(OTHER_BUILD), source.build);
    assert!(!source.producer_alive);
}

#[test]
fn the_owned_collector_announcing_another_build_is_reported() {
    // Goal: the app learns at once that the collector it started is a stale executable.
    // Method: announce another build while owning the pid that registered the run.
    let fixture = Recording::new();
    fixture.announce(fixture.run(), OTHER_BUILD);

    let source = health::Health::new()
        .read(&fixture.root, Some(std::process::id()))
        .unwrap();

    assert_eq!(policy::Build::Other(OTHER_BUILD), source.build);
}

#[test]
fn another_builds_announcement_is_ignored_while_this_app_starts_its_own_collector() {
    // Goal: an announcement left by an earlier holder cannot fail this app's own startup.
    // Method: read another build's announcement while owning a child of another pid.
    let fixture = Recording::new();
    fixture.announce(fixture.run(), OTHER_BUILD);

    let source = health::Health::new()
        .read(&fixture.root, Some(std::process::id() + 1))
        .unwrap();

    assert_eq!(policy::Build::Expected, source.build);
}

#[test]
fn an_announcement_of_an_older_run_reads_as_a_holder_yet_to_announce() {
    // Goal: an announcement the next holder has yet to withdraw cannot pass for that holder.
    // Method: roll over to a second run, then announce the first run under the second's lock.
    let mut fixture = Recording::new();
    let older = fixture.run();
    fixture.rollover();
    fixture.announce(older, BUILD);

    let source = health::Health::new().read(&fixture.root, None).unwrap();

    assert_eq!(policy::Build::Unannounced, source.build);
    assert!(source.recording == RecordingStatus::Pending);
    assert!(!source.producer_alive);
}

#[test]
fn a_completed_run_announced_under_a_held_recording_is_finishing() {
    // Goal: a run that ended reads as a holder still finishing, never as a recording to
    // restart.
    // Method: finish the announced run's ring while its writer keeps holding the recording.
    let mut fixture = Recording::new();
    fixture.writer.as_mut().unwrap().finish();

    let source = health::Health::new().read(&fixture.root, None).unwrap();

    assert!(source.recording == RecordingStatus::Finishing);
    assert!(!source.producer_alive);
}

#[test]
fn a_holder_announcing_in_another_format_is_unrecognized() {
    // Goal: a collector of a version that changed the announcement is refused, never misread.
    // Method: replace the fixture's announcement with a later version's header.
    let fixture = Recording::new();
    fixture.announce_text("flightdeck-collector-2\n");

    let source = health::Health::new().read(&fixture.root, None).unwrap();

    assert_eq!(policy::Build::Unrecognized, source.build);
    assert!(source.recording == RecordingStatus::Pending);
}

#[test]
fn a_held_recording_without_an_announcement_is_pending() {
    // Goal: a starting or older collector holding the recording reads as one yet to announce.
    // Method: remove the fixture's announcement while its writer holds the recording.
    let fixture = Recording::new();
    std::fs::remove_file(fixture.root.join("collector.announcement")).unwrap();

    let source = health::Health::new().read(&fixture.root, None).unwrap();

    assert_eq!(policy::Build::Unannounced, source.build);
    assert!(source.recording == RecordingStatus::Pending);
}

struct Recording {
    root: PathBuf,
    lock_paths: Vec<PathBuf>,
    writer: Option<Writer>,
    publication: u64,
}

impl Recording {
    fn new() -> Self {
        let token = identity_text(random_identity().unwrap());
        let root = std::env::temp_dir().join(format!("flightdeck-lifecycle-{token}"));
        let place = Place::for_user(&format!("gpui-health-{token}")).unwrap();
        let (writer, registered) = Registration::start(&root, &place, Self::shape(), 1)
            .unwrap()
            .into_parts();
        registered.unwrap();
        Self {
            root,
            lock_paths: vec![place.lock_path],
            writer: Some(writer),
            publication: 0,
        }
    }

    fn shape() -> Shape {
        Shape {
            slot_count: 4,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 1,
        }
    }

    fn publish(&mut self) {
        self.writer.as_mut().unwrap().publish(&Readings {
            run_tick: self.publication,
            cpus: &[collector::machine::CPUTicks::default()],
            memory: &collector::machine::Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: collector::ring::Dropped::default(),
        });
        self.publication += 1;
    }

    fn snapshot(&self) -> Snapshot {
        let identity = self.writer.as_ref().unwrap().identity().unwrap();
        Snapshot {
            generation: 1,
            catalog: json!({"path":self.root, "collector_completed":false,
                "latest_collector_pid":std::process::id(), "latest_observation":{
                "run":identity_text(identity.run).to_string(),
                "boot":identity_text(identity.boot).to_string(),
                "publication":self.publication}}),
            selection: json!({"second":1000,"selected":{
                "run":identity_text(identity.run).to_string(),
                "boot":identity_text(identity.boot).to_string(),
                "publication":self.publication}}),
            pages: BTreeMap::new(),
            apps: None,
            detail: None,
            history: None,
            cpu_detail: None,
            cpu_history: None,
            resident_history: None,
            app_attribution_history: None,
            overview_app_histories: None,
            histories_pending: false,
            process_history: None,
            rankings: None,
            app_ranks: None,
            app_parts: None,
            read_ms: 0.0,
        }
    }

    fn session(&self, mode: Mode) -> CollectorSession {
        CollectorSession::new(Configuration {
            root: self.root.clone(),
            binary: std::env::current_exe().unwrap(),
            isolated: true,
            mode,
        })
    }

    fn run(&self) -> Identity {
        self.writer.as_ref().unwrap().identity().unwrap().run
    }

    fn announce(&self, run: Identity, build: u64) {
        self.announce_text(&format!(
            "flightdeck-collector-1\n{}\n{build:016x}\n",
            identity_text(run)
        ));
    }

    fn announce_text(&self, text: &str) {
        collector::recording::private::atomic_write(
            &self.root,
            "collector.announcement",
            text.as_bytes(),
        )
        .unwrap();
    }

    fn rollover(&mut self) {
        let mut previous = self.writer.take().unwrap();
        previous.finish();
        drop(previous);
        let token = identity_text(random_identity().unwrap());
        let place = Place::for_user(&format!("gpui-health-{token}")).unwrap();
        let (writer, registered) = Registration::start(&self.root, &place, Self::shape(), 1)
            .unwrap()
            .into_parts();
        registered.unwrap();
        self.lock_paths.push(place.lock_path);
        self.writer = Some(writer);
        self.publication = 0;
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        if let Some(writer) = self.writer.as_mut() {
            writer.finish();
        }
        let registry = collector::recording::Registry::read(&self.root).unwrap();
        for registered in registry.runs.iter().take(2) {
            match collector::ring::unlink_generation(&registered.ring_name) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("Fixture ring cleanup failed: {error}"),
            }
        }
        std::fs::remove_dir_all(&self.root).unwrap();
        // Each ring is new to the test, so its lock file in Application Support would stay.
        for lock_path in &self.lock_paths {
            std::fs::remove_file(lock_path).unwrap();
        }
    }
}
