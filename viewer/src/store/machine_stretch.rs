//! What the seconds of one stretch read of the whole Mac, each series reduced on its own, so a
//! chart that draws one reading of a stretch still finds a reading that lasted one second.

use collector::machine::Pressure;
use collector::ring::RunIdentity;
use model::rates::number;
pub use model::summary::MachinePlace;
use model::summary::{CategorySummary, GaugeSummary, RateSummary};

use super::{MachineSecond, STRETCHES_MAX};
use crate::EARLIER_STRIDE_SECONDS;

/// The changes of category a stretch keeps. One run's stretch holds a reading a second.
const CATEGORY_CHANGES_KEPT: usize = 10;

const _: () = {
    // Ten readings change at most nine times, which `CATEGORY_CHANGES_KEPT` holds.
    assert!(EARLIER_STRIDE_SECONDS == 10);
    // The Mac's readings cost a store about 2 MiB at the most stretches it holds.
    assert!(size_of::<MachineStretch>() <= 6 * 1024);
    assert!(STRETCHES_MAX * size_of::<MachineStretch>() <= 2_300 * 1024);
};

pub type Gauge = GaugeSummary<MachinePlace>;
pub type Rate = RateSummary<MachinePlace>;
pub type Category = CategorySummary<u8, MachinePlace, CATEGORY_CHANGES_KEPT>;

/// Each series of the Mac's charts over one stretch. The rates are what a counter grew by over
/// the time between readings, and the gauges what a reading saw at its instant.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MachineStretch {
    pub cpu_percent: Rate,
    pub cpu_level_percent: [Rate; 2],
    pub gpu_active_percent: Rate,
    pub disk_read_bps: Rate,
    pub disk_written_bps: Rate,
    pub network_received_bps: Rate,
    pub network_sent_bps: Rate,
    pub memory_app_bytes: Gauge,
    pub memory_compressed_bytes: Gauge,
    pub memory_total_bytes: Gauge,
    pub memory_used_bytes: Gauge,
    pub memory_wired_bytes: Gauge,
    pub power_watts: Gauge,
    pub soc_celsius: Gauge,
    pub fan_mean_rpm: Gauge,
    pub pressure: Category,
    pub thermal_pressure: Category,
}

impl MachineStretch {
    /// Takes in `second` of run `run`, the reading after the last one it took in.
    pub fn add(&mut self, run: &RunIdentity, second: &MachineSecond) {
        let place = MachinePlace {
            boot: run.boot.0,
            run: run.run.0,
            publication: second.publication,
            wall_ns: second.wall_ns,
        };
        let follows = second.follows();

        self.add_rates(second, follows, &place);
        self.add_memory(second.memory.as_ref(), follows, &place);
        self.power_watts
            .push(second.power_watts.map(f64::from), follows, place);
        self.soc_celsius
            .push(second.soc_celsius.map(f64::from), follows, place);
        self.fan_mean_rpm
            .push(second.fan_mean_rpm.map(f64::from), follows, place);
        self.thermal_pressure
            .push(second.thermal_pressure, follows, place);
    }

    fn add_rates(&mut self, second: &MachineSecond, follows: bool, place: &MachinePlace) {
        let place = *place;
        let rate = |value: Option<f64>| value.zip(second.elapsed_ns);
        let network = second.network.as_ref();

        self.cpu_percent
            .push(rate(second.cpu_percent), follows, place);
        for (level, percent) in self
            .cpu_level_percent
            .iter_mut()
            .zip(second.cpu_level_percent)
        {
            level.push(rate(percent), follows, place);
        }
        self.gpu_active_percent
            .push(rate(second.gpu_active_percent), follows, place);
        self.disk_read_bps
            .push(rate(second.disk_read_bps), follows, place);
        self.disk_written_bps
            .push(rate(second.disk_written_bps), follows, place);
        self.network_received_bps.push(
            rate(network.map(|traffic| traffic.received_bps)),
            follows,
            place,
        );
        self.network_sent_bps.push(
            rate(network.map(|traffic| traffic.sent_bps)),
            follows,
            place,
        );
    }

    fn add_memory(
        &mut self,
        memory: Option<&super::MemorySecond>,
        follows: bool,
        place: &MachinePlace,
    ) {
        let place = *place;
        let bytes = |value: Option<u64>| value.map(number);

        self.memory_app_bytes.push(
            bytes(memory.and_then(|memory| memory.app_bytes)),
            follows,
            place,
        );
        self.memory_compressed_bytes.push(
            bytes(memory.map(|memory| memory.compressed_bytes)),
            follows,
            place,
        );
        self.memory_total_bytes.push(
            bytes(memory.map(|memory| memory.total_bytes)),
            follows,
            place,
        );
        self.memory_wired_bytes.push(
            bytes(memory.map(|memory| memory.wired_bytes)),
            follows,
            place,
        );
        self.memory_used_bytes.push(
            bytes(memory.and_then(MachineStretch::add_memory_used)),
            follows,
            place,
        );
        self.pressure.push(
            memory.map(|memory| pressure_level(memory.pressure)),
            follows,
            place,
        );
    }

    /// What the memory chart stacks at one reading, added up at that reading, since the most of
    /// each part at different readings adds up to more than the Mac ever used.
    fn add_memory_used(memory: &super::MemorySecond) -> Option<u64> {
        memory
            .app_bytes?
            .checked_add(memory.wired_bytes)?
            .checked_add(memory.compressed_bytes)
    }
}

/// The level the recording writes for `pressure`, which the memory pressure chart and readout
/// read.
#[must_use]
pub fn pressure_level(pressure: Pressure) -> u8 {
    match pressure {
        Pressure::Normal => 0,
        Pressure::Warning => 1,
        Pressure::Critical => 2,
    }
}
