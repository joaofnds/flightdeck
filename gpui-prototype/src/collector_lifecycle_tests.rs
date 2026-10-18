use std::collections::BTreeMap;
use std::path::PathBuf;

use collector::recording::{Registration, identity_text, random_identity};
use collector::ring::{Place, Readings, Shape, Writer};
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
    session.tick(true);
    fixture.publish();

    session.tick(true);

    assert!(session.ready());
    assert_eq!(paused.catalog["latest_observation"]["publication"], 1);
    session.0.started = Instant::now()
        .checked_sub(STARTUP_LIMIT + Duration::from_secs(1))
        .unwrap();
    session.tick(false);
    assert!(session.ready());
}

#[test]
fn computer_navigation_does_not_hide_owned_startup_progress() {
    // Goal: startup observes its configured producer after navigation to Computer.
    // Method: advance a real ring after its first observation, then omit presented data.
    let mut fixture = Recording::new();
    fixture.publish();
    let mut session = fixture.session(Mode::Explicit);
    session
        .0
        .policy
        .launched(std::process::id(), Duration::ZERO);
    session.tick(true);
    fixture.publish();

    session.tick(false);

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
    session.tick(true);
    configured.publish();

    session.tick(true);

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
    session.tick(true);
    fixture.rollover();
    fixture.publish();
    session.tick(false);
    assert_eq!(session.0.policy.state, State::Discovering);

    fixture.publish();
    session.tick(false);

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

    session.tick(true);

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
    session.tick(true);
    session.0.started = Instant::now()
        .checked_sub(STARTUP_LIMIT + Duration::from_secs(1))
        .unwrap();

    session.tick(false);

    assert!(session.failure().is_some());
    assert_eq!(session.0.policy.state, State::Failed);
    assert_eq!(session.owned_pid(), None);
}

struct Recording {
    root: PathBuf,
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
        self.writer = Some(writer);
        self.publication = 0;
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.writer.as_mut().unwrap().finish();
        let registry = collector::recording::Registry::read(&self.root).unwrap();
        for registered in registry.runs.iter().take(2) {
            match collector::ring::unlink_generation(&registered.ring_name) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("Fixture ring cleanup failed: {error}"),
            }
        }
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
