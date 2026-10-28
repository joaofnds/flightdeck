use super::*;
use crate::cost::{Snapshot, Source};
use crate::ioreport::tests::read_tests as ioreport_fixture;
use crate::recording::discovery::tests::allocator_calls_in;
use crate::smc::tests::read_tests as smc_fixture;

#[test]
fn continues_after_smc_read_failure() {
    // Goal: a returned optional-source error leaves independent observations publishable.
    // Method: read native SMC fixture answers, fail the next read, and decode the real ring.
    let mut collector = Collector::new(Timebase::read().unwrap(), Duration::ZERO).unwrap();
    collector.smc = Some(crate::smc::tests::m5_pro());
    collector.ioreport = None;
    collector.battery = None;
    collector.disks = None;
    let mut harness = Harness::battery(&collector);
    let mut publication = Publication::new(&harness.writer).unwrap();
    let mut reader = RingReader::open_generation(harness.writer.name()).unwrap();
    let mut tick = battery_tick(&reader.shape());
    let mut identities = Vec::with_capacity(collector.capacities().processes);
    let mut meter = Meter::default();
    publication
        .read(&mut collector, &mut meter, &mut harness.writer, None)
        .unwrap();
    publication
        .publish(&mut harness.writer, &collector, 0, None)
        .unwrap();
    reader
        .read_identity_into(&mut tick, &mut identities)
        .unwrap();
    assert!(tick.smc_read);
    crate::smc::tests::fail_read(collector.smc.as_mut().unwrap());

    let acquired = publication.read(&mut collector, &mut meter, &mut harness.writer, None);

    assert!(
        acquired.is_ok(),
        "optional SMC failure terminated acquisition: {acquired:?}"
    );
    publication
        .publish(&mut harness.writer, &collector, 1, None)
        .unwrap();
    assert_eq!(
        reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap(),
        Found::Publication {
            publication: 2,
            tick: 1
        }
    );
    assert!(!tick.smc_read);
    assert!(!tick.processes.is_empty());
    assert!(!identities.is_empty());
}

#[test]
fn continues_after_ioreport_read_failure() {
    // Goal: a returned optional-source error leaves independent observations publishable.
    // Method: read native IOReport fixture answers, fail the next read, and decode the real ring.
    let mut collector = Collector::new(Timebase::read().unwrap(), Duration::ZERO).unwrap();
    collector.smc = None;
    collector.ioreport = Some(crate::ioreport::tests::one_core());
    collector.battery = None;
    collector.disks = None;
    let mut harness = Harness::battery(&collector);
    let mut publication = Publication::new(&harness.writer).unwrap();
    let mut reader = RingReader::open_generation(harness.writer.name()).unwrap();
    let mut tick = battery_tick(&reader.shape());
    let mut identities = Vec::with_capacity(collector.capacities().processes);
    let mut meter = Meter::default();
    publication
        .read(&mut collector, &mut meter, &mut harness.writer, None)
        .unwrap();
    publication
        .publish(&mut harness.writer, &collector, 0, None)
        .unwrap();
    reader
        .read_identity_into(&mut tick, &mut identities)
        .unwrap();
    assert!(tick.ioreport_read);
    crate::ioreport::tests::fail_read(collector.ioreport.as_mut().unwrap());

    let acquired = publication.read(&mut collector, &mut meter, &mut harness.writer, None);

    assert!(
        acquired.is_ok(),
        "optional IOReport failure terminated acquisition: {acquired:?}"
    );
    publication
        .publish(&mut harness.writer, &collector, 1, None)
        .unwrap();
    assert_eq!(
        reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap(),
        Found::Publication {
            publication: 2,
            tick: 1
        }
    );
    assert!(!tick.ioreport_read);
    assert!(!tick.processes.is_empty());
    assert!(!identities.is_empty());
}

#[derive(Clone, Copy)]
struct Failures {
    smc: Option<smc_fixture::Failure>,
    ioreport: Option<ioreport_fixture::Failure>,
}

struct Acquisitions {
    collector: Collector,
    harness: Harness,
    publication: Publication,
    reader: RingReader,
    tick: Tick,
    identities: Vec<IdentityObservation>,
    meter: Meter,
    timebase: Timebase,
    failures: Failures,
    own_pid: i32,
    own_cpu_ns_previous: u64,
}

impl Acquisitions {
    fn new(failures: Failures) -> Self {
        let timebase = Timebase::read().unwrap();
        let mut collector = Collector::new(timebase, Duration::ZERO).unwrap();
        collector.smc = Some(crate::smc::tests::m5_pro());
        collector.ioreport = Some(ioreport_fixture::fixture(failures.ioreport));
        collector.battery = None;
        collector.disks = None;
        let mut harness = Harness::system(&collector);
        let mut publication = Publication::new(&harness.writer).unwrap();
        let reader = RingReader::open_generation(harness.writer.name()).unwrap();
        let tick = battery_tick(&reader.shape());
        let identities = Vec::with_capacity(collector.capacities().processes);
        let mut meter = Meter::default();
        publication
            .read(&mut collector, &mut meter, &mut harness.writer, None)
            .unwrap();
        crate::diagnostics::error(format_args!("optional source diagnostic warm-up"));
        Self {
            collector,
            harness,
            publication,
            reader,
            tick,
            identities,
            meter,
            timebase,
            failures,
            own_pid: crate::processes::own_pid().unwrap(),
            own_cpu_ns_previous: 0,
        }
    }

    fn acquire(&mut self, run_tick: u64) {
        let sequence = u8::try_from(run_tick % 4).unwrap();
        let smc_failure = (sequence == 1).then_some(self.failures.smc).flatten();
        smc_fixture::set_read(self.collector.smc.as_mut().unwrap(), sequence, smc_failure);
        ioreport_fixture::set_read(self.collector.ioreport.as_mut().unwrap(), sequence);
        let times = ObservationTimes {
            wall_ns: 1_000_000_000_000 + run_tick * 1_000_000_000,
            continuous_ns: run_tick * 2_000_000_000,
            awake_ns: run_tick * 1_000_000_000,
        };
        let before = Snapshot::take(&self.meter, self.timebase).unwrap();

        let acquired = self.publication.read(
            &mut self.collector,
            &mut self.meter,
            &mut self.harness.writer,
            Some(&times),
        );

        assert!(
            acquired.is_ok(),
            "optional failure terminated tick {run_tick}: {acquired:?}"
        );
        let cost = Snapshot::take(&self.meter, self.timebase)
            .unwrap()
            .cost_since(&before)
            .unwrap();
        assert!(cost.ms_per_second(Source::Smc) > 0.0);
        assert!(cost.ms_per_second(Source::IOReport) > 0.0);
        self.publication
            .publish(
                &mut self.harness.writer,
                &self.collector,
                run_tick,
                Some(&times),
            )
            .unwrap();
        assert_eq!(
            self.reader
                .read_identity_into(&mut self.tick, &mut self.identities)
                .unwrap(),
            Found::Publication {
                publication: run_tick + 1,
                tick: run_tick
            }
        );
        assert_eq!(self.tick.times, Some(times));
        self.check_independent();
        check_smc(&self.tick, sequence, smc_failure.is_none());
        check_ioreport(
            &self.tick,
            sequence,
            sequence != 1 || self.failures.ioreport.is_none(),
        );
        ioreport_fixture::assert_released(self.collector.ioreport.as_ref().unwrap());
    }

    fn check_independent(&mut self) {
        assert!(!self.tick.cpus.is_empty());
        assert!(self.tick.memory.total_bytes > 0);
        let process = self
            .tick
            .processes
            .iter()
            .find(|process| process.pid == self.own_pid)
            .unwrap();
        let cpu_ns = process.usage.user_ns + process.usage.system_ns;
        assert!(cpu_ns > self.own_cpu_ns_previous);
        self.own_cpu_ns_previous = cpu_ns;
        let own = self
            .identities
            .iter()
            .find(|identity| identity.pid == self.own_pid)
            .unwrap();
        assert_eq!(own.usage, UsageAvailability::Measured);
        assert!(own.creation().is_some());
        assert_eq!(
            self.identities.len(),
            self.collector.identities.observations().len()
        );
        assert_eq!(
            self.collector.events().len(),
            self.collector.event_identities().len()
        );
        assert_eq!(
            self.tick.system.unwrap().reading,
            self.collector.system.reading
        );
        assert_eq!(self.tick.process_gpu_ns().len(), self.identities.len());
    }
}

fn check_smc(tick: &Tick, sequence: u8, available: bool) {
    assert_eq!(tick.smc_read, available);
    if available {
        assert_eq!(
            tick.power_watts.to_bits(),
            (10.0 * f32::from(sequence)).to_bits()
        );
        assert_eq!(tick.soc_celsius.to_bits(), tick.power_watts.to_bits());
        assert_eq!(tick.fan_rpm, [tick.power_watts; 2]);
        let temperature = (sequence > 0).then_some(tick.power_watts.to_bits());
        assert_eq!(tick.cpu_celsius.map(f32::to_bits), temperature);
        assert_eq!(tick.gpu_celsius.map(f32::to_bits), temperature);
    } else {
        assert_eq!(tick.power_watts.to_bits(), 0.0_f32.to_bits());
        assert_eq!(tick.soc_celsius.to_bits(), 0.0_f32.to_bits());
        assert_eq!(tick.cpu_celsius, None);
        assert_eq!(tick.gpu_celsius, None);
        assert!(tick.fan_rpm.is_empty());
    }
}

fn check_ioreport(tick: &Tick, sequence: u8, available: bool) {
    assert_eq!(tick.ioreport_read, available);
    if available {
        assert_eq!(tick.gpu_energy_nj, 100 * u64::from(sequence));
        assert_eq!(tick.gpu_active_ns, [100 * u64::from(sequence); 15]);
        assert_eq!(tick.core_count(), 1);
        assert_eq!(tick.core_active_ns(0), [100 * u64::from(sequence); 15]);
    } else {
        assert_eq!(tick.gpu_energy_nj, 0);
        assert!(tick.gpu_active_ns.is_empty());
        assert_eq!(tick.core_count(), 0);
    }
}

fn exercise_recovery(failures: Failures) {
    let mut acquisitions = Acquisitions::new(failures);
    let capacities = (
        acquisitions.tick.fan_rpm.capacity(),
        acquisitions.tick.gpu_active_ns.capacity(),
        acquisitions.identities.capacity(),
        acquisitions.tick.processes.capacity(),
    );

    let calls = allocator_calls_in(|| {
        for run_tick in 0..8 {
            acquisitions.acquire(run_tick);
        }
    });

    assert_eq!(calls, 0);
    assert_eq!(
        capacities,
        (
            acquisitions.tick.fan_rpm.capacity(),
            acquisitions.tick.gpu_active_ns.capacity(),
            acquisitions.identities.capacity(),
            acquisitions.tick.processes.capacity(),
        )
    );
    println!(
        "optional source recovery: publications=8 reused_slots=2 rust_allocator_calls={calls}"
    );
}

#[test]
fn recovers_smc_without_losing_independent_ioreport_or_identities() {
    // Goal: success/error/success/success stays fresh through reused decoding and ring slots.
    // Method: fail after power and one fan update; count acquisition/publication over two cycles.
    crate::tests::with_diagnostics(
        concat!(
            "publication::tests::optional_sources::",
            "recovers_smc_without_losing_independent_ioreport_or_identities"
        ),
        || {
            exercise_recovery(Failures {
                smc: Some(smc_fixture::Failure::Native),
                ioreport: None,
            });
        },
    );
}

#[test]
fn recovers_ioreport_without_losing_independent_smc_or_identities() {
    // Goal: partially decoded GPU/core state never escapes as a current reading.
    // Method: fail in a core after GPU and earlier states update, then publish two fresh samples.
    crate::tests::with_diagnostics(
        concat!(
            "publication::tests::optional_sources::",
            "recovers_ioreport_without_losing_independent_smc_or_identities"
        ),
        || {
            exercise_recovery(Failures {
                smc: None,
                ioreport: Some(ioreport_fixture::Failure::NegativeResidency),
            });
        },
    );
}

#[test]
fn recovers_both_optional_sources_without_losing_independent_observations() {
    // Goal: simultaneous optional failures do not prevent independent publication or recovery.
    // Method: fail SMC and partially decoded IOReport together through reused slots and buffers.
    crate::tests::with_diagnostics(
        concat!(
            "publication::tests::optional_sources::",
            "recovers_both_optional_sources_without_losing_independent_observations"
        ),
        || {
            exercise_recovery(Failures {
                smc: Some(smc_fixture::Failure::Missing),
                ioreport: Some(ioreport_fixture::Failure::NegativeResidency),
            });
        },
    );
}

#[test]
fn counts_continuing_errors_with_the_installed_diagnostic_transport() {
    // Goal: fixed-size errors remain allocation-free through production diagnostic admission.
    // Method: install the reporter in a supervised child, then run error families and publication.
    crate::tests::with_diagnostics(
        concat!(
            "publication::tests::optional_sources::",
            "counts_continuing_errors_with_the_installed_diagnostic_transport"
        ),
        counts_continuing_errors,
    );
}

fn counts_continuing_errors() {
    for failure in [
        smc_fixture::Failure::Native,
        smc_fixture::Failure::Short,
        smc_fixture::Failure::Missing,
        smc_fixture::Failure::Refused,
    ] {
        smc_fixture::check_failure(failure);
    }
    ioreport_fixture::check_negative_native_channel_count();
    for failure in ioreport_fixture::FAILURES {
        ioreport_fixture::check_failure(failure);
    }
    exercise_recovery(Failures {
        smc: Some(smc_fixture::Failure::Native),
        ioreport: Some(ioreport_fixture::Failure::NegativeResidency),
    });
}
