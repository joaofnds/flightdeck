//! The highest reading of each series of the whole Mac over the seconds of one stretch, and the
//! reading it came from, so a chart that draws one reading of a stretch still finds a reading that
//! lasted one second. The Data table shows that reading and picking it holds its second, and a
//! state's cell shows the highest state of its ten seconds, which is all a user sees of a
//! stretch, so a stretch keeps nothing else.

use collector::machine::Pressure;
use collector::ring::RunIdentity;
use model::rates::number;
pub use model::summary::MachinePlace;
use model::summary::{CategoryHighest, GaugeHighest, RateHighest};

use super::{
    MachineSecond, MemoryRate, NetworkRate, PointRate, STRETCHES_HELD_MAX, STRETCHES_MAX,
    StorageRate,
};

const _: () = {
    // `Stretches::new` allocates a stretch for each closed stretch the store holds, for each it
    // takes while a View holds their buffers, and for the open one, and each stretch owns one
    // `MachineStretch`, so the Mac's readings cost a store about 1.1 MiB.
    assert!(size_of::<MachineStretch>() <= 3_008);
    let allocated = STRETCHES_MAX + STRETCHES_HELD_MAX + 1;
    assert!(allocated * size_of::<MachineStretch>() <= 1_131_008);
};

pub type Gauge = GaugeHighest<MachinePlace>;
pub type Rate = RateHighest<MachinePlace>;
pub type Category = CategoryHighest<u8, MachinePlace>;

/// Each series of the Mac's charts over one stretch. The rates are what a counter grew by over
/// the time between readings, and the gauges what a reading saw at its instant.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MachineStretch {
    pub cpu_percent: Rate,
    pub cpu_level_percent: [Rate; 2],
    /// The mean clock of each level while its cores were active, level 0 first, which only a
    /// second's complete reading holds (`MachineSecond::cpu_level_ghz`), so each reads none at a
    /// second without one.
    pub cpu_level_ghz: [Rate; 2],
    pub gpu_active_percent: Rate,
    /// The GPU's power, which only a second's complete reading holds (`MachineSecond::gpu_watts`),
    /// so it reads none at a second without one.
    pub gpu_watts: Rate,
    pub disk_read_bps: Rate,
    pub disk_written_bps: Rate,
    pub network_received_bps: Rate,
    pub network_sent_bps: Rate,
    /// The rates of `NetworkRate::ALL`, in its order.
    pub network_rates: [Rate; NetworkRate::ALL.len()],
    /// The rates of `StorageRate::ALL`, in its order.
    pub storage_rates: [Rate; StorageRate::ALL.len()],
    pub memory_app_bytes: Gauge,
    pub memory_compressed_bytes: Gauge,
    pub memory_total_bytes: Gauge,
    pub memory_used_bytes: Gauge,
    pub memory_wired_bytes: Gauge,
    pub swap_used_bytes: Gauge,
    pub swap_total_bytes: Gauge,
    /// The rates of `MemoryRate::ALL`, in its order.
    pub memory_rates: [Rate; MemoryRate::ALL.len()],
    // Only a second's complete reading holds the GPU memory in use and allocated
    // (`MachineSecond::gpu_memory_in_use_bytes`), so each reads none at a second without one.
    pub gpu_memory_in_use_bytes: Gauge,
    pub gpu_memory_allocated_bytes: Gauge,
    pub power_watts: Gauge,
    pub soc_celsius: Gauge,
    pub gpu_celsius: Gauge,
    pub cpu_celsius: Gauge,
    /// The 1, 5 and 15 minute load averages, in that order. Only a second's complete reading
    /// holds the load averages and the open files (`MachineSecond::load_average`), so each of
    /// these and the two open files gauges reads none at a second without one.
    pub load_average: [Gauge; 3],
    pub open_files: Gauge,
    pub open_files_percent: Gauge,
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

        self.add_rates(second, &place);
        self.add_memory(second.memory.as_ref(), &place);
        self.power_watts
            .push(second.power_watts.map(f64::from), place);
        self.soc_celsius
            .push(second.soc_celsius.map(f64::from), place);
        self.gpu_celsius
            .push(second.gpu_celsius.map(f64::from), place);
        self.cpu_celsius
            .push(second.cpu_celsius.map(f64::from), place);
        let load_average = second.load_average();
        for (window, average) in self.load_average.iter_mut().enumerate() {
            let value = load_average.and_then(|load| load.get(window).copied());
            average.push(value, place);
        }
        self.open_files.push(second.open_files().map(number), place);
        self.open_files_percent
            .push(second.open_files_percent(), place);
        self.gpu_memory_in_use_bytes
            .push(second.gpu_memory_in_use_bytes().map(number), place);
        self.gpu_memory_allocated_bytes
            .push(second.gpu_memory_allocated_bytes().map(number), place);
        self.fan_mean_rpm
            .push(second.fan_mean_rpm.map(f64::from), place);
        self.thermal_pressure.push(second.thermal_pressure, place);
    }

    /// The stretch's `rate`.
    #[must_use]
    pub fn memory_rate(&self, rate: MemoryRate) -> &Rate {
        &self.memory_rates[rate.index()]
    }

    /// The stretch's `rate`.
    #[must_use]
    pub fn network_rate(&self, rate: NetworkRate) -> &Rate {
        &self.network_rates[rate.index()]
    }

    /// The stretch's `rate`.
    #[must_use]
    pub fn storage_rate(&self, rate: StorageRate) -> &Rate {
        &self.storage_rates[rate.index()]
    }

    fn add_rates(&mut self, second: &MachineSecond, place: &MachinePlace) {
        let place = *place;
        let rate = |value: Option<f64>| value.zip(second.elapsed_ns);
        let network = second.network.as_ref();

        self.cpu_percent.push(rate(second.cpu_percent), place);
        for (level, percent) in self
            .cpu_level_percent
            .iter_mut()
            .zip(second.cpu_level_percent)
        {
            level.push(rate(percent), place);
        }
        let level_ghz = second.cpu_level_ghz();
        for (level, ghz) in self.cpu_level_ghz.iter_mut().enumerate() {
            let value = level_ghz.and_then(|ghz| ghz.get(level).copied().flatten());
            ghz.push(rate(value), place);
        }
        self.gpu_active_percent
            .push(rate(second.gpu_active_percent), place);
        self.gpu_watts.push(rate(second.gpu_watts()), place);
        self.disk_read_bps.push(rate(second.disk_read_bps), place);
        self.disk_written_bps
            .push(rate(second.disk_written_bps), place);
        self.network_received_bps
            .push(rate(network.map(|traffic| traffic.received_bps)), place);
        self.network_sent_bps
            .push(rate(network.map(|traffic| traffic.sent_bps)), place);
        for (series, memory_rate) in self.memory_rates.iter_mut().zip(MemoryRate::ALL) {
            series.push(rate(second.memory_rate(memory_rate)), place);
        }
        for (series, network_rate) in self.network_rates.iter_mut().zip(NetworkRate::ALL) {
            series.push(rate(second.network_rate(network_rate)), place);
        }
        for (series, storage_rate) in self.storage_rates.iter_mut().zip(StorageRate::ALL) {
            series.push(rate(second.storage_rate(storage_rate)), place);
        }
    }

    fn add_memory(&mut self, memory: Option<&super::MemorySecond>, place: &MachinePlace) {
        let place = *place;
        let bytes = |value: Option<u64>| value.map(number);

        self.memory_app_bytes
            .push(bytes(memory.and_then(|memory| memory.app_bytes)), place);
        self.memory_compressed_bytes
            .push(bytes(memory.map(|memory| memory.compressed_bytes)), place);
        self.memory_total_bytes
            .push(bytes(memory.map(|memory| memory.total_bytes)), place);
        self.memory_wired_bytes
            .push(bytes(memory.map(|memory| memory.wired_bytes)), place);
        self.memory_used_bytes.push(
            bytes(memory.and_then(MachineStretch::add_memory_used)),
            place,
        );
        self.swap_used_bytes
            .push(bytes(memory.map(|memory| memory.swap_used_bytes)), place);
        self.swap_total_bytes
            .push(bytes(memory.map(|memory| memory.swap_total_bytes)), place);
        self.pressure
            .push(memory.map(|memory| pressure_level(memory.pressure)), place);
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
