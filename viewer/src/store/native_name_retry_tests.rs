use std::sync::Arc;

use super::super::{Admission, Input, OwnedFrame, PrepareError, Prepared, Stage};
use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::live_apps::{Publication, PublicationEvents};
use crate::recording::MachineMinute;
use crate::store::payload::{NameTrace, ProcessNames};
use crate::store::{AppDetails, AppsSecond, ByteBudget, Payload, ProcessName, ProcessRow};

#[test]
fn a_native_partial_name_refusal_keeps_its_stage_predecessor_and_accepted_names() {
    // Goal: partial native name admission cannot advance accepted names or lose prepared input.
    // Method: stage real ring publications, deny the second new name, then resume the same input.
    let mut fixture = Fixture::new();
    super::publish_many(
        &mut fixture,
        1,
        &super::Many::new(1, 50, b"prior", b"/Prior.app"),
    );
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let observer = Arc::new(ByteBudget::new(64 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let first = Captured::read(&fixture, 1);
    let mut names = ProcessNames::new(16, &budget).unwrap();
    let previous = MachineMinute::default();
    let input = first.input(&names, &previous, None, None);
    let mut prepared = Prepared::new(&input.source).unwrap();
    prepared.resume(&input, &budget, &operation).unwrap();
    prepared.commit_names(&mut names);
    let prior = std::mem::take(&mut prepared.apps);
    let frame = prepared.into_frame().unwrap();
    assert_eq!(names.observation().0, 1);
    fixture.skip_run_ticks(2);
    fixture.publish_with_system_rows(101);
    let second = Captured::read(&fixture, 2);
    let input = second.input(&names, &first.machine, Some(&frame), Some(&prior));
    let mut prepared = Prepared::new(&input.source).unwrap();
    prepared.capture_frame(&input, &budget).unwrap();
    prepared.capture_sample(&input, &budget).unwrap();
    prepared.capture_raw(&input, &budget, &operation).unwrap();
    prepared.capture_machine(&input, &budget).unwrap();
    prepared
        .capture_metadata(input.previous_apps, &budget)
        .unwrap();
    let trace = NameTrace::new(2, &observer);

    refuse_then_retry(&mut prepared, &input, &budget);

    assert_eq!(names.observation().0, 1);
    assert_eq!(frame.get().observation().stamp.publication, 1);
    assert_eq!(
        prior.processes.as_ref().unwrap().selected.stamp.publication,
        1
    );
    assert_eq!(
        prior.processes.as_ref().unwrap()[0]
            .executable_name()
            .unwrap()
            .bytes,
        b"prior"
    );
    prepared.commit_names(&mut names);
    assert_eq!(names.observation().0, 3);
    let records = trace.finish();
    assert_eq!(records.values().len(), 2);
    assert!(!records.values()[0].committed);
    assert_eq!(records.values()[0].work.allocation_requests, 5);
    assert!(records.values()[1].committed);
    println!("native_name_retry={:?}", records.values());
    drop((
        prepared, prior, frame, names, records, first, second, fixture,
    ));
    assert_eq!(budget.used(), 0);
    assert_eq!(observer.used(), 0);
}

fn refuse_then_retry(prepared: &mut Prepared, input: &Input<'_>, budget: &Arc<ByteBudget>) {
    let sample = prepared.sample.as_ref().unwrap().get();
    assert_eq!(sample.processes().len(), 2);
    let name = sample
        .processes()
        .iter()
        .filter_map(|row| row.executable_name.as_ref())
        .min_by(|left, right| left.bytes.cmp(&right.bytes))
        .unwrap();
    let name_bytes =
        2 * size_of::<usize>() + size_of::<ProcessName>() + name.bytes.len() + name.display.len();
    let payload_bytes = 2 * size_of::<usize>()
        + size_of::<Payload>()
        + sample.processes().len() * size_of::<ProcessRow>()
        + sample.apps().len() * size_of::<AppDetails>();
    let scratch = sample.processes().len()
        * (size_of::<&crate::live_app_response::Bytes>() + size_of::<Arc<ProcessName>>());
    let pressure = budget
        .reserve(budget.maximum() - budget.used() - payload_bytes - scratch - name_bytes)
        .unwrap();
    let held = budget.used();

    let refusal = prepared.capture_payload(input, budget);

    assert!(matches!(
        refusal,
        Err(PrepareError::Admission(Admission::Payload))
    ));
    assert!(prepared.stage == Stage::Payload);
    assert!(prepared.name_additions.is_none());
    assert_eq!(input.names.observation().0, 1);
    assert_eq!(budget.used(), held);
    drop(pressure);
    prepared.capture_payload(input, budget).unwrap();
    assert!(prepared.ready());
    let expected = prepared.sample.as_ref().unwrap().get();
    let payload = prepared.apps.processes.as_ref().unwrap();
    assert_eq!(payload.previous, expected.previous);
    for (actual, expected) in payload.iter().zip(expected.processes()) {
        assert_eq!(actual.pid, expected.pid);
        assert_eq!(actual.creation, expected.creation);
        assert_eq!(
            actual.executable_name().unwrap().bytes,
            expected.executable_name.as_ref().unwrap().bytes
        );
    }
}

struct Captured {
    reader: collector::ring::RingReader,
    tick: collector::ring::Tick,
    identities: Vec<collector::ring::IdentityObservation>,
    publication: u64,
    machine: MachineMinute,
}

impl Captured {
    fn read(fixture: &Fixture, publication: u64) -> Self {
        let mut reader =
            collector::ring::RingReader::open_generation(fixture.writer.name()).unwrap();
        reader.seek_publication(publication).unwrap();
        let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
        let mut identities = Vec::with_capacity(2);
        assert!(
            matches!(reader.read_identity_into(&mut tick, &mut identities).unwrap(),
            collector::ring::Found::Publication { publication: found, .. } if found == publication)
        );
        let mut machine = MachineMinute::default();
        machine.push_tick(publication, &tick);

        Self {
            reader,
            tick,
            identities,
            publication,
            machine,
        }
    }

    fn input<'a>(
        &'a self,
        names: &'a ProcessNames,
        previous: &'a MachineMinute,
        frame: Option<&'a OwnedFrame>,
        apps: Option<&'a AppsSecond>,
    ) -> Input<'a> {
        Input {
            names,
            source: Publication {
                reader: &self.reader,
                tick: &self.tick,
                identities: &self.identities,
                publication: self.publication,
            },
            events: PublicationEvents {
                events: &[],
                identities: &[],
            },
            current: &self.machine,
            previous,
            previous_frame: frame.map(OwnedFrame::get),
            previous_apps: apps,
        }
    }
}
