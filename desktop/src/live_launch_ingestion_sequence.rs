use collector::ring::{DictionaryCapacity, Place, RingReader, Shape, Writer};
use serde_json::json;

use super::{Backend, Harness, SourceLayout, Trace, identity_text, output, random_identity};
use super::{request, source};
use crate::live_source_mode::SourceMode;

struct Sequence {
    writer: Option<Writer>,
    programs: Option<source::Programs>,
    harness: Harness,
}

/// `snapshot` without the App parts that keep its context, of which the Store holds two at most,
/// so a test can keep the snapshot past later reads.
fn parts_released(
    mut snapshot: Box<crate::live_source::Snapshot>,
) -> Box<crate::live_source::Snapshot> {
    snapshot.app_parts = None;
    snapshot
}

impl Sequence {
    fn setup(output: &mut Option<Self>) {
        assert!(output.is_none());
        let root = std::env::temp_dir().join(format!(
            "flightdeck-ingestion-sequence-{}",
            identity_text(random_identity().unwrap())
        ));
        let store = persistence::Store::open(&root).unwrap();
        // These clocks count from this Mac's boot, which can lie past a shorter period.
        persistence::retention::Period::from_seconds(10 * 365 * 86_400)
            .unwrap()
            .write(&collector::recording::private::PrivateDirectory::open(&root).unwrap())
            .unwrap();
        let trace = Trace::new(store.recording());
        let scope = Some(trace.enter());
        *output = Some(Self {
            writer: None,
            programs: None,
            harness: Harness {
                backend: None,
                trace,
                scope,
                root,
                input: None,
            },
        });
        let sequence = output.as_mut().unwrap();
        sequence.setup_writer(store.recording());
        sequence.publish(0, |_| {});
        drop(store);
        Backend::start(
            &sequence.harness.root,
            SourceMode::Live,
            &mut sequence.harness.backend,
        )
        .unwrap();
    }

    fn store(&self) -> &viewer::store::Store {
        let backend = self.harness.backend.as_ref().unwrap();

        backend.store.as_ref().unwrap()
    }

    fn store_parked(&self) {
        for _attempt in 0..2_000 {
            if self.store().is_paused() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the Store did not park");
    }

    fn setup_writer(&mut self, recording: collector::ring::Identity) {
        let identity = collector::recording::new_run_identity(recording).unwrap();
        let root = &self.harness.root;
        let place = Place {
            lock_path: root.join("sequence.lock"),
            name_prefix: "/fdseq.".into(),
        };
        let writer = Writer::create_with_identity(
            &place,
            Shape {
                slot_count: 4,
                event_capacity: 6,
                ..source::SHAPE
            },
            DictionaryCapacity {
                entries: 2,
                arena_bytes: 256,
            },
            &identity,
            source::CONTINUOUS_FIRST_NS,
        )
        .unwrap();
        self.writer = Some(writer);
        let writer = self.writer.as_mut().unwrap();
        self.harness.input = Some(source::Source {
            identity,
            population: false,
            layout: SourceLayout::Identity,
            lock: place.lock_path,
            ring: writer.name().to_owned(),
            second_first: source::SECOND_FIRST,
            mapped_bytes: 0,
        });
        let reader = RingReader::open_generation(writer.name()).unwrap();
        let mapped_bytes = u64::try_from(reader.mapped_size_bytes()).unwrap();
        assert!(mapped_bytes <= 65_536);
        self.harness.input.as_mut().unwrap().mapped_bytes = mapped_bytes;
        drop(reader);
        source::create_register(root, &identity, writer.name()).unwrap();
        source::create_programs(writer, &mut self.programs).unwrap();
    }

    fn publish(
        &mut self,
        tick: u64,
        mut customize: impl FnMut(&mut collector::ring::Readings<'_>),
    ) {
        source::create_publish(
            self.writer.as_mut().unwrap(),
            self.programs.as_ref(),
            source::SECOND_FIRST,
            tick,
            &mut customize,
        )
        .unwrap();
    }

    fn teardown(&mut self) {
        let mut writer = self.writer.take().unwrap();
        writer.finish();
        drop(writer);
        self.harness.teardown();
    }

    fn observe(&self, through: u64, output: &mut serde_json::Value) {
        assert!(through <= 3);
        let mut reader = RingReader::open_generation(self.writer.as_ref().unwrap().name()).unwrap();
        assert_eq!(reader.shape().slot_count, 4);
        assert_eq!(reader.publication_newest().unwrap(), through);
        let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
        let mut identities = Vec::with_capacity(3);
        let mut observations = Vec::with_capacity(3);
        for publication in 1..=through {
            reader.seek_publication(publication).unwrap();
            let found = reader
                .read_identity_into(&mut tick, &mut identities)
                .unwrap();
            assert!(matches!(found, collector::ring::Found::Publication {
                publication: actual, ..
            } if actual == publication));
            observations.push(
                json!({"publication": publication, "run_tick": tick.run_tick,
                "wall_ns": tick.times.unwrap().wall_ns.to_string(),
                "continuous_ns": tick.times.unwrap().continuous_ns.to_string(),
                "smc_read": tick.smc_read, "power_nonfinite": tick.power_watts.is_nan(),
                "pid": tick.processes[0].pid,
                "user_ns": tick.processes[0].usage.user_ns.to_string(),
                "system_ns": tick.processes[0].usage.system_ns.to_string(),
                "identity_count": identities.len()}),
            );
        }
        *output = json!(observations);
    }

    fn inspect(
        &mut self,
        snapshot: &crate::live_source::Snapshot,
    ) -> Box<crate::live_source::Snapshot> {
        let mut exact = request("apps");
        exact.second = snapshot.selection["selected"]["wall_ns"]
            .as_u64()
            .map(|wall| wall / 1_000_000_000);
        exact.identity = Some(snapshot.selection["selected"].clone());
        exact.load_resident_history = false;
        exact.load_app_attribution_history = false;
        exact.load_process_history = false;
        exact.earlier = crate::live_source::Earlier::Skip;
        self.harness.inspect_request(exact).unwrap()
    }

    fn select_at(
        &mut self,
        latest: &crate::live_source::Snapshot,
        second: u64,
    ) -> Box<crate::live_source::Snapshot> {
        let identity =
            crate::live_projection::inspection_identity(&latest.selection, second).unwrap();
        let mut selected = request("apps");
        selected.second = Some(second);
        selected.identity = Some(identity);
        self.harness.read_request(selected).unwrap()
    }

    fn overrun_refusal(&mut self) {
        for tick in 3..6 {
            self.publish(tick, |_| {});
        }
    }

    fn observe_overrun(&self, output: &mut serde_json::Value) {
        let mut reader = RingReader::open_generation(self.writer.as_ref().unwrap().name()).unwrap();
        assert_eq!(reader.shape().slot_count, 4);
        assert_eq!(reader.shape().event_capacity, 6);
        assert_eq!(reader.publication_newest().unwrap(), 6);
        let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
        let mut identities = Vec::with_capacity(3);
        reader.seek_publication(2).unwrap();
        let missing = reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap();
        assert!(matches!(
            missing,
            collector::ring::Found::Missed { first: 2, end: 3 }
        ));
        let present = reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap();
        assert!(matches!(
            present,
            collector::ring::Found::Publication { publication: 3, .. }
        ));
        assert_eq!(tick.run_tick, 2);
        let mut events = Vec::with_capacity(6);
        reader.seek_event(1).unwrap();
        for expected in 1..=6 {
            let mut identity = collector::ring::IdentityObservation::EMPTY;
            let found = reader.read_event_identity(&mut identity).unwrap();
            let collector::ring::FoundEvent::Event { number, tick, .. } = found else {
                panic!("the publication-overrun fixture lost an independently retained event");
            };
            assert_eq!(number, expected);
            events.push(json!({"event": number, "run_tick": tick,
                "identity": format!("{identity:?}")}));
        }
        *output = json!({"publication_newest": 6, "slots": 4, "event_capacity": 6,
            "missing_publication": {"first": 2, "end": 3},
            "retained": {"publication": 3, "run_tick": tick.run_tick,
                "wall_ns": tick.times.unwrap().wall_ns.to_string(),
                "continuous_ns": tick.times.unwrap().continuous_ns.to_string(),
                "user_ns": tick.processes[0].usage.user_ns.to_string(),
                "system_ns": tick.processes[0].usage.system_ns.to_string()},
            "events": events});
    }

    fn cleanup(self) {
        let root = self.harness.root.clone();
        let ring = self.harness.input.as_ref().unwrap().ring.clone();
        drop(self);
        assert!(!root.exists());
        let opened = RingReader::open_generation(&ring);
        assert!(matches!(opened, Err(error) if error.kind() == std::io::ErrorKind::NotFound));
    }
}

impl Drop for Sequence {
    fn drop(&mut self) {
        if let Some(mut writer) = self.writer.take() {
            writer.finish();
        }
        if self.harness.input.is_none() {
            assert!(self.harness.backend.is_none());
            std::fs::remove_dir_all(&self.harness.root).unwrap();
        }
    }
}

#[test]
fn a_store_woken_after_a_hide_takes_in_what_it_missed_once() {
    // Goal: a window shown again reconciles its Store once, and no read of the page takes in what
    // it missed as well. Method: park the Store as a hide does, publish, wake it, read a page,
    // and count the work on each missed publication per producer.
    let mut sequence = None;
    Sequence::setup(&mut sequence);
    let mut sequence = sequence.unwrap();
    sequence.publish(1, |_| {});
    sequence.harness.read_publication("cpu", 2);
    sequence.store().set_active(false);
    sequence.store_parked();
    sequence.publish(2, |_| {});
    sequence.publish(3, |_| {});
    let while_parked = sequence.store().take();

    sequence.store().set_active(true);
    sequence.harness.read_publication("cpu", 4);
    sequence.teardown();

    assert_eq!(
        while_parked.map_or(2, |view| view.seconds.last().unwrap().publication),
        2
    );
    let events = sequence.harness.events();
    super::routes::assert_worker_took_in_nothing(&events);
    for publication in [3, 4] {
        for kind in [
            super::Kind::MachineReduction,
            super::Kind::AppDerivation,
            super::Kind::Accepted,
        ] {
            assert_eq!(
                super::count(&events, super::Producer::Store, kind, publication),
                1,
                "{kind:?} {publication}"
            );
        }
    }
}

#[test]
fn a_nonfinite_publication_holds_back_no_successor() {
    // Goal: the page reads B beside its missing power, and C after it, on the one policy the
    // Store keeps, where a second reader used to refuse B and every later reading.
    // Method: publish through a real writer only after each Backend response and Store View.
    let mut sequence = None;
    Sequence::setup(&mut sequence);
    let mut sequence = sequence.unwrap();
    let accepted = parts_released(sequence.harness.read("apps"));
    let first = sequence.harness.view_at(1);
    assert_eq!(accepted.selection["selected"]["publication"], 1);
    let first_identity = first.apps.last().unwrap().observation.unwrap();
    assert_eq!(first_identity.stamp.publication, 1);

    sequence.publish(1, |readings| {
        super::customize(super::Input::NonfinitePower, readings);
    });
    sequence.harness.read_publication("apps", 2);
    let second = sequence.harness.view_at(2);
    assert_eq!(second.apps.last().unwrap().elapsed_ns, Some(1_000_000_000));
    assert_store_app_amounts(second.apps.last().unwrap(), 2);

    sequence.publish(2, |_| {});
    let successor = parts_released(sequence.harness.read_publication("apps", 3));
    let third = sequence.harness.view_at(3);
    let mut input = serde_json::Value::Null;
    sequence.observe(3, &mut input);
    let inspected = sequence.inspect(&accepted);
    sequence.teardown();

    assert_eq!(successor.selection["selected"]["publication"], 3);
    assert_eq!(input[1]["publication"], 2);
    assert_eq!(input[1]["power_nonfinite"], true);
    assert_eq!(input[2]["publication"], 3);
    assert_eq!(input[2]["run_tick"], 2);
    assert_eq!(input[2]["smc_read"], false);
    assert_eq!(
        inspected.selection["selected"],
        accepted.selection["selected"]
    );
    assert_eq!(third.apps.last().unwrap().elapsed_ns, Some(1_000_000_000));
    assert_store_app_amounts(third.apps.last().unwrap(), 1);
    assert_eq!(accepted.apps, inspected.apps);
    assert_eq!(accepted.detail, inspected.detail);
    let events = sequence.harness.events();
    assert_eq!(
        super::count(&events, super::Producer::Worker, super::Kind::Accepted, 1),
        0
    );
    assert_eq!(
        super::count(&events, super::Producer::Store, super::Kind::Accepted, 1),
        1
    );
    assert_eq!(
        super::count(&events, super::Producer::Worker, super::Kind::RingRead, 3),
        0
    );
    assert_eq!(
        super::count(&events, super::Producer::Store, super::Kind::RingRead, 3),
        1
    );
    output::record_sequence(
        &sequence.harness,
        &json!({"accepted": accepted.apps, "accepted_detail": accepted.detail,
            "inspected": inspected.apps, "inspected_detail": inspected.detail, "input": input,
            "selection": accepted.selection, "store_machine": format!("{:?}", third.seconds),
            "store_apps": format!("{:?}", third.apps)}),
    );
    sequence.cleanup();
}

fn assert_store_app_amounts(apps: &viewer::store::AppsSecond, count: usize) {
    use std::sync::Arc;
    use viewer::AppAttributionMetric;
    assert_eq!(apps.apps.len(), count);
    let main = apps
        .apps
        .iter()
        .find(|app| app.name.bytes == source::APP)
        .unwrap();
    assert_eq!(main.members, 3);
    assert!(!main.membership_complete);
    let cohort = [Arc::clone(&main.name)];
    let cpu = apps.attribution(AppAttributionMetric::Cpu, &cohort).apps[0].unwrap();
    assert_eq!(cpu.rate, Some(30.0));
    assert_store_app_amounts_measure(cpu.measure.as_ref(), 300_000_000);
    let power = apps.attribution(AppAttributionMetric::Power, &cohort).apps[0].unwrap();
    assert_eq!(power.rate, Some(1.0));
    assert_store_app_amounts_measure(power.measure.as_ref(), 1_000_000_000);
    let disk = apps.attribution(AppAttributionMetric::Disk, &cohort).apps[0].unwrap();
    assert_eq!(disk.rate, Some(0.0));
    assert_store_app_amounts_measure(disk.measure.as_ref(), 0);
    assert_store_app_amounts_measure(apps.resident(&cohort).apps[0].as_ref(), 104_857_600);
}

fn assert_store_app_amounts_measure(
    actual: Option<&viewer::live_app_response::Measure>,
    known: u64,
) {
    assert_eq!(
        actual,
        Some(&viewer::live_app_response::Measure {
            known,
            measured_members: 1,
            unavailable_members: 2,
            overflowed: false,
        })
    );
}

fn assert_ordinary_c_once(events: &[super::Event]) {
    for kind in [
        super::Kind::RingRead,
        super::Kind::FrameCapture,
        super::Kind::AppDerivation,
        super::Kind::Accepted,
    ] {
        let work: Vec<_> = events
            .iter()
            .filter(|event| event.producer == super::Producer::Store)
            .filter(|event| event.kind == kind)
            .filter(|event| matches!(event.key, super::Key::Publication { publication: 3, .. }))
            .collect();
        assert_eq!(work.len(), 1, "{kind:?}");
        assert_eq!(
            work[0].purpose,
            persistence::work::ingestion::Purpose::Ordinary
        );
    }
}

fn assert_selected_c(
    selected: &crate::live_source::Snapshot,
    inspected: &crate::live_source::Snapshot,
) {
    assert_eq!(selected.selection["selected"]["publication"], 3);
    assert_eq!(selected.selection["selected"]["run_tick"], 2);
    assert_eq!(
        selected.selection["selected"]["reader_losses"]["publications"],
        0
    );
    assert_eq!(selected.selection["selected"]["reader_losses"]["events"], 0);
    assert_eq!(
        selected.selection["selected"]["reader_losses"]["unknown"],
        false
    );
    assert_eq!(
        selected.selection["selected"],
        inspected.selection["selected"]
    );
    let mut selected_summary = selected.selection["summary"].as_object().unwrap().clone();
    let mut inspected_summary = inspected.selection["summary"].as_object().unwrap().clone();

    // A segment belongs to the chart context; all observation readings must agree.
    drop(selected_summary.remove("segment"));
    drop(inspected_summary.remove("segment"));
    assert_eq!(selected_summary, inspected_summary);
    assert_eq!(selected.apps, inspected.apps);
    assert_eq!(selected.detail, inspected.detail);
    let detail = selected.detail.as_ref().unwrap();
    assert_eq!(detail["previous"]["publication"], "2");
    assert_eq!(detail["elapsed_ns"], 1_000_000_000);
    assert_eq!(detail["app"]["cpu_ns"]["measured_members"], 1);
    assert_eq!(detail["app"]["cpu_ns"]["unavailable_members"], 2);
    assert_eq!(detail["app"]["cpu_percent_one_core"], 30.0);
}

#[test]
fn an_exact_successor_of_a_nonfinite_publication_reads_after_it_is_overwritten() {
    // Goal: exact C reads the same after the ring lets go of B, independently of the newest result.
    // Method: acknowledge A/B/C, overwrite only the publication prefix, then select and inspect C.
    let mut sequence = None;
    Sequence::setup(&mut sequence);
    let mut sequence = sequence.unwrap();
    let accepted = parts_released(sequence.harness.read("apps"));
    let first = sequence.harness.view_at(1);
    assert_eq!(accepted.selection["selected"]["publication"], 1);
    assert_eq!(first.seconds.last().unwrap().publication, 1);
    sequence.publish(1, |readings| {
        super::customize(super::Input::NonfinitePower, readings);
    });
    sequence.harness.read_publication("apps", 2);
    let second = sequence.harness.view_at(2);
    assert_store_app_amounts(second.apps.last().unwrap(), 2);
    sequence.publish(2, |_| {});
    sequence.harness.read_publication("apps", 3);
    let third = sequence.harness.view_at(3);
    assert_store_app_amounts(third.apps.last().unwrap(), 1);

    sequence.overrun_refusal();
    let mut input = serde_json::Value::Null;
    sequence.observe_overrun(&mut input);
    let latest = parts_released(sequence.harness.read_publication("apps", 6));
    let sixth = sequence.harness.view_at(6);
    assert_eq!(latest.selection["selected"]["publication"], 6);
    assert!(!latest.histories_pending);
    let mut recovered_events = Vec::new();
    sequence
        .harness
        .trace
        .snapshot(&mut recovered_events)
        .unwrap();
    assert_ordinary_c_once(&recovered_events);
    let selected = sequence.select_at(&latest, source::SECOND_FIRST + 2);
    let inspected = sequence.inspect(&selected);
    sequence.teardown();

    assert_eq!(sixth.seconds.last().unwrap().publication, 6);
    assert_eq!(third.seconds.last().unwrap().publication, 3);
    assert_store_app_amounts(third.apps.last().unwrap(), 1);
    assert_selected_c(&selected, &inspected);
    assert_ordinary_c_once(&sequence.harness.events());
    output::record_sequence_case(
        "refusal-overrun-exact-c",
        &sequence.harness,
        &json!({"input": input, "accepted": accepted.selection,
            "ordinary_recovery_events": recovered_events.iter().map(output::event)
                .collect::<Vec<_>>(),
            "latest": {"selection": latest.selection, "apps": latest.apps, "detail": latest.detail},
            "selected": {"selection": selected.selection, "apps": selected.apps,
                "detail": selected.detail},
            "inspected": {"selection": inspected.selection, "apps": inspected.apps,
                "detail": inspected.detail},
            "store_c_machine": format!("{:?}", third.seconds),
            "store_c_apps": format!("{:?}", third.apps)}),
    );
    sequence.cleanup();
}

#[test]
fn withholds_app_rates_across_a_physically_spaced_run_tick_gap() {
    // Goal: distinguish App predecessor policies with native clocks spaced by two seconds.
    // Method: acknowledge tick0, then publish tick2 as publication2 and retain full detail.
    let mut sequence = None;
    Sequence::setup(&mut sequence);
    let mut sequence = sequence.unwrap();
    let accepted = parts_released(sequence.harness.read("apps"));
    let first = sequence.harness.view_at(1);
    assert_eq!(accepted.selection["selected"]["publication"], 1);
    assert_eq!(first.seconds.last().unwrap().publication, 1);

    sequence.publish(2, |_| {});
    let successor = parts_released(sequence.harness.read_publication("apps", 2));
    let second = sequence.harness.view_at(2);
    let mut input = serde_json::Value::Null;
    sequence.observe(2, &mut input);
    let mut exited_request = request("apps");
    exited_request.app = Some(crate::live_source::AppKey {
        kind: "executable".into(),
        bytes_hex: crate::live_source::app_key_bytes_hex(source::EVENT_EXECUTABLE),
    });
    let exited = sequence.harness.read_request(exited_request).unwrap();
    sequence.teardown();

    let detail = successor.detail.as_ref().unwrap();
    assert_eq!(successor.selection["selected"]["run_tick"], 2);
    assert_eq!(
        successor.selection["selected"]["wall_ns"],
        1_800_000_002_000_000_000_u64
    );
    assert_eq!(detail["previous"]["publication"], "1");
    super::assert_gap_intervals(detail);
    assert_eq!(input[1]["continuous_ns"], "102000000000");
    super::assert_gap_intervals(exited.detail.as_ref().unwrap());
    assert_eq!(
        exited.detail.as_ref().unwrap()["processes"][0]["state"],
        "exited_verified"
    );
    assert_eq!(successor.apps.as_ref().unwrap()["app_count"], 2);
    assert_eq!(second.apps.last().unwrap().apps.len(), 2);
    assert_eq!(second.apps.last().unwrap().elapsed_ns, None);
    output::record_sequence_case(
        "spaced-run-tick-gap",
        &sequence.harness,
        &json!({"input": input, "selection": successor.selection, "apps": successor.apps,
            "detail": successor.detail, "exited_detail": exited.detail,
            "store_machine": format!("{:?}", second.seconds),
            "store_apps": format!("{:?}", second.apps)}),
    );
    sequence.cleanup();
}
