use model::catalog;
use persistence::{Error, Result, schema};

use super::{Operation, Raw};

pub(crate) enum Cell<'a> {
    Null,
    Bool(bool),
    Unsigned(u64),
    Signed(i64),
    Float32(f32),
    Float64(f64),
    Text(&'a str),
    Bytes(&'a [u8]),
    Disks(&'a collector::disks::DiskSnapshot),
}

pub(crate) struct View<'a> {
    raw: Option<&'a Raw>,
    machine: crate::store::Reading<'a>,
    table: &'static catalog::Table,
    levels: Vec<LevelState>,
    derived: bool,
}

struct LevelState {
    level: u64,
    state: u64,
    active_ns: u64,
    frequency_khz: Option<u64>,
}

const LEVEL_COLUMNS: [&str; 5] = [
    "publication",
    "level",
    "state",
    "active_ns",
    "frequency_khz",
];

impl<'a> View<'a> {
    pub(super) fn native_scratch_bytes(raw: &Raw, name: &str) -> usize {
        if name != "core_level_states" {
            return 0;
        }
        if raw
            .machine
            .ticks
            .first()
            .is_none_or(|tick| tick.performance_levels.is_none())
        {
            return 0;
        }

        let count = raw.machine.core_states.len().min(512);
        let node = 11 * size_of::<((u64, u64), LevelState)>() + 12 * size_of::<usize>() + 32;

        count * (node + size_of::<LevelState>())
    }

    pub fn native(raw: &'a Raw, name: &str, output: &mut Option<Self>) -> Result<()> {
        assert!(output.is_none());
        let derived = name == "core_level_states";
        let table = if derived {
            &catalog::CORE_STATES
        } else {
            catalog::TABLES
                .into_iter()
                .find(|table| table.name == name)
                .ok_or(Error::Bound("table name"))?
        };

        *output = Some(Self {
            raw: Some(raw),
            machine: crate::store::readings(&raw.machine)
                .next()
                .ok_or(Error::Bound("native machine reading"))?,
            table,
            levels: Vec::new(),
            derived,
        });
        if derived {
            output
                .as_mut()
                .ok_or(Error::Bound("live raw view"))?
                .aggregate_levels()?;
        }

        Ok(())
    }

    /// Borrows already decoded machine rows; the native catalog and cell mapping stay shared.
    pub fn machine(reading: &crate::store::Reading<'a>, name: &str) -> Result<Self> {
        let derived = name == "core_level_states";
        let table = if derived {
            &catalog::CORE_STATES
        } else {
            catalog::TABLES
                .into_iter()
                .find(|table| table.name == name)
                .ok_or(Error::Bound("table name"))?
        };
        let mut view = Self {
            raw: None,
            machine: *reading,
            table,
            levels: Vec::new(),
            derived,
        };
        if derived {
            view.aggregate_levels()?;
        }
        Ok(view)
    }

    fn raw(&self) -> &'a Raw {
        self.raw.unwrap_or_else(|| unreachable!("raw-only table"))
    }

    fn aggregate_levels(&mut self) -> Result<()> {
        let Some(levels) = self.machine.tick.performance_levels else {
            return Ok(());
        };

        // At most two levels times 256 states; this bounded scratch is never retained.
        let mut states = std::collections::BTreeMap::new();
        for row in self.machine.core_states {
            let level = u64::from(row.core < levels[1].logical_cpus);
            let frequency = row.frequency_khz.map(u64::from);
            let full = states.len() == 512;
            let state = match states.entry((level, u64::from(row.state))) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    if full {
                        return Err(Error::Bound("core level states"));
                    }

                    entry.insert(LevelState {
                        level,
                        state: u64::from(row.state),
                        active_ns: 0,
                        frequency_khz: frequency,
                    })
                }
            };
            state.active_ns = state
                .active_ns
                .checked_add(row.active_ns)
                .ok_or(Error::Bound("core level state sum"))?;
            if state.frequency_khz != frequency {
                state.frequency_khz = None;
            }
        }
        assert!(states.len() <= 512);

        self.levels = states.into_values().collect();
        Ok(())
    }

    pub fn len(&self) -> usize {
        if self.derived {
            return self.levels.len();
        }

        let machine = &self.machine;
        match self.table.name {
            "ticks" => 1,
            "memory" => usize::from(machine.memory.is_some()),
            "cpus" => machine.cpus.len(),
            "interfaces" => machine.interfaces.len(),
            "gpu_states" => machine.gpu_states.len(),
            "core_states" => machine.core_states.len(),
            "fans" => machine.fans.len(),
            "coalitions" => self.raw().coalitions.len(),
            "processes" => self
                .raw()
                .identities
                .as_ref()
                .map_or(self.raw().processes.len(), Vec::len),
            "events" => self.raw().events.len(),
            _ => unreachable!("catalog raw table"),
        }
    }

    pub fn holds(&self, name: &str) -> bool {
        if self.derived {
            return LEVEL_COLUMNS.contains(&name);
        }
        self.table
            .columns
            .iter()
            .chain(self.table.groups.iter().flat_map(|group| group.iter()))
            .any(|column| column.name == name)
    }

    fn captured_group(&self, group: &[catalog::Column]) -> bool {
        let tick = &self.machine.tick;
        match group[0].name {
            "battery_version" => tick.battery.is_some(),
            "physical_disks" => tick.disks.is_some(),
            "cpu_celsius" => tick.cpu_celsius.is_some() || tick.gpu_celsius.is_some(),
            "load_average_1" | "frequency_khz" => self.machine.tick.system.is_some(),
            "identity_version" => self.raw().identities.is_some(),
            "gpu_ns" => self.machine.tick.system.is_some() && self.raw().identities.is_some(),
            _ => unreachable!("catalog optional group"),
        }
    }

    pub fn columns(&self) -> Vec<String> {
        if self.derived {
            return LEVEL_COLUMNS.map(String::from).to_vec();
        }
        // Query normalization appended absent groups after the encoder's captured groups.
        // Preserve that public order, including a system-only or battery-only tick.
        let mut columns: Vec<_> = self
            .table
            .columns
            .iter()
            .map(|column| column.name.to_owned())
            .collect();
        for captured in [true, false] {
            for group in self.table.groups {
                if self.captured_group(group) == captured {
                    columns.extend(group.iter().map(|column| column.name.to_owned()));
                }
            }
        }
        columns
    }

    pub(super) fn column_storage_bytes(&self) -> usize {
        if self.derived {
            return LEVEL_COLUMNS.len() * size_of::<String>()
                + LEVEL_COLUMNS.iter().map(|name| name.len()).sum::<usize>();
        }

        let columns = self
            .table
            .columns
            .iter()
            .chain(self.table.groups.iter().flat_map(|group| group.iter()));
        let mut count = 0;
        let mut names = 0;
        for column in columns {
            count += 1;
            names += column.name.len();
        }

        // Geometric growth can briefly keep the old and replacement column vectors together.
        3 * count * size_of::<String>() + names
    }

    pub fn order(&self, operation: &Operation) -> Result<Vec<usize>> {
        if self.len() > crate::data::ROWS_MAX {
            return Err(Error::Bound("live raw rows"));
        }

        let mut order: Vec<_> = (0..self.len()).collect();
        operation.check()?;
        match self.table.name {
            "processes" => order.sort_unstable_by_key(|index| self.process_key(*index)),
            "events" => order.sort_unstable_by_key(|index| {
                let event = &self.raw().events[*index];
                let (pid, usage, _, _, _, _) = event_facts(&event.event);
                let awake = usage.map(|usage| usage.awake_ns_at_start);
                (pid, awake.is_none(), awake, event.number)
            }),
            "interfaces" => {
                order.sort_unstable_by_key(|index| self.machine.interfaces[*index].interface.index);
            }
            "coalitions" => order.sort_unstable_by_key(|index| self.raw().coalitions[*index].id),
            _ => {}
        }
        operation.check()?;
        Ok(order)
    }

    fn process_key(&self, index: usize) -> (i32, bool, Option<u64>) {
        let (pid, process, _) = self.process(index);
        let awake = process.map(|process| process.usage.awake_ns_at_start);
        (pid, awake.is_none(), awake)
    }

    fn process(
        &self,
        index: usize,
    ) -> (
        i32,
        Option<&'a collector::processes::Process>,
        Option<&'a super::Identity>,
    ) {
        if let Some(identities) = &self.raw().identities {
            let identity = &identities[index];
            let process = if identity.usage == schema::availability::MEASURED {
                let index = self
                    .raw()
                    .processes
                    .binary_search_by_key(&identity.pid, |process| process.pid)
                    .unwrap_or_else(|_| unreachable!("capture validated each measured identity"));
                Some(&self.raw().processes[index])
            } else {
                None
            };
            (identity.pid, process, Some(identity))
        } else {
            let process = &self.raw().processes[index];
            (process.pid, Some(process), None)
        }
    }

    pub fn disks(&self, snapshot: &mut collector::disks::DiskSnapshot) -> bool {
        if let Some(disks) = self.machine.tick.disks {
            *snapshot = disks;
            true
        } else {
            false
        }
    }

    pub fn cell(&self, row: usize, name: &str) -> Result<Cell<'a>> {
        if row >= self.len() {
            return Err(Error::Bound("live raw row"));
        }

        if !self.holds(name) {
            return Err(Error::Bound("missing schema column"));
        }

        let publication = self.machine.tick.publication;
        if name == "publication" {
            return Ok(Cell::Unsigned(publication));
        }

        if self.derived {
            let state = &self.levels[row];
            return Ok(match name {
                "level" => unsigned(state.level),
                "state" => unsigned(state.state),
                "active_ns" => unsigned(state.active_ns),
                "frequency_khz" => optional_unsigned(state.frequency_khz),
                _ => unreachable!("derived columns"),
            });
        }

        Ok(match self.table.name {
            "ticks" => self.tick(name),
            "memory" => self.memory(name),
            "cpus" => self.cpu(row, name),
            "interfaces" => self.interface(row, name),
            "gpu_states" => match name {
                "state" => unsigned(self.machine.gpu_states[row].state),
                "active_ns" => unsigned(self.machine.gpu_states[row].active_ns),
                _ => unreachable!("GPU columns"),
            },
            "core_states" => self.core(row, name),
            "fans" => match name {
                "fan" => unsigned(self.machine.fans[row].fan),
                "rpm" => Cell::Float32(self.machine.fans[row].rpm),
                _ => unreachable!("fan columns"),
            },
            "processes" => self.process_cell(row, name),
            "events" => self.event_cell(row, name),
            "coalitions" => self.coalition(row, name),
            _ => unreachable!("catalog raw table"),
        })
    }
}

impl Cell<'_> {
    pub(super) fn formatted_capacity(&self, column: &str) -> Result<usize> {
        match self {
            Self::Null => Ok(0),
            Self::Bool(_) | Self::Unsigned(_) | Self::Signed(_) => Ok(128),
            Self::Float32(value) if value.is_finite() => Ok(128),
            Self::Float64(value) if value.is_finite() => Ok(1024),
            Self::Float32(_) if column == "rpm" => Ok(0),
            Self::Float32(_) | Self::Float64(_) => {
                Err(Error::Bound("unsupported or nonfinite cell"))
            }
            Self::Text(text) => Ok(text.len()),
            Self::Bytes(bytes) => Ok(bytes.len() * 4),
            Self::Disks(_) => Ok(persistence::disks::HEX_BYTES_MAX),
        }
    }

    pub fn number(&self) -> Result<Option<f64>> {
        match self {
            Self::Null => Ok(None),
            Self::Float64(number) => Ok(Some(*number)),
            Self::Float32(number) => {
                if !number.is_finite() {
                    return Ok(None);
                }

                // Legacy Float32 cells used their shortest decimal before f64 parsing.
                // Preserve that public scalar presentation without building a string table.
                number
                    .to_string()
                    .parse()
                    .map(Some)
                    .map_err(|_| Error::Bound("f32 presentation"))
            }
            _ => Err(Error::Bound("number metric cell")),
        }
    }

    pub fn format(&self, column: &str) -> Result<Option<String>> {
        persistence::work::record(|counts| counts.string_cells += 1);
        let text = match self {
            Self::Bool(value) => value.to_string(),
            Self::Unsigned(value) => value.to_string(),
            Self::Signed(value) => value.to_string(),
            Self::Float32(value) if value.is_finite() => value.to_string(),
            Self::Null => return Ok(None),
            Self::Float32(_) if column == "rpm" => return Ok(None),
            Self::Float64(value) if value.is_finite() => value.to_string(),
            Self::Float32(_) | Self::Float64(_) => {
                return Err(Error::Bound("unsupported or nonfinite cell"));
            }
            Self::Text(value) => (*value).to_owned(),
            Self::Disks(value) => persistence::disks::encode(value)?,
            Self::Bytes(value) => {
                let mut text = String::with_capacity(value.len() * 4);
                crate::byte_display::append(&mut text, value);
                return Ok(Some(text));
            }
        };
        if text.len() > 4096 {
            return Err(Error::Bound("cell bytes"));
        }

        Ok(Some(text))
    }
}

fn unsigned(value: impl Into<u64>) -> Cell<'static> {
    Cell::Unsigned(value.into())
}
fn optional_unsigned(value: Option<impl Into<u64>>) -> Cell<'static> {
    value.map_or(Cell::Null, |value| unsigned(value.into()))
}
fn boolean(value: Option<bool>) -> Cell<'static> {
    value.map_or(Cell::Null, Cell::Bool)
}

fn event_facts(
    event: &collector::tracker::Event,
) -> (
    i32,
    Option<&collector::processes::Usage>,
    u8,
    Option<u64>,
    u8,
    Option<u8>,
) {
    use collector::processes::Unreadable;
    use collector::tracker::{Event, Noticed};
    match event {
        Event::Start { pid, coalition } => (
            *pid,
            None,
            schema::event_kind::START,
            *coalition,
            schema::availability::NOT_APPLICABLE,
            None,
        ),
        Event::Exec { pid } => (
            *pid,
            None,
            schema::event_kind::EXEC,
            None,
            schema::availability::NOT_APPLICABLE,
            None,
        ),
        Event::Exit { pid, last, noticed } => {
            let (usage, availability) = match last {
                Ok(usage) => (Some(usage), schema::availability::MEASURED),
                Err(Unreadable::Refused) => (None, schema::availability::REFUSED),
                Err(Unreadable::Gone) => (None, schema::availability::GONE),
            };
            let noticed = match noticed {
                Noticed::OnExit => schema::noticed::ON_EXIT,
                Noticed::AlreadyExited => schema::noticed::ALREADY_EXITED,
            };
            (
                *pid,
                usage,
                schema::event_kind::EXIT,
                None,
                availability,
                Some(noticed),
            )
        }
    }
}

impl<'a> View<'a> {
    fn tick(&self, name: &str) -> Cell<'a> {
        let tick = &self.machine.tick;
        match name {
            "run_tick" => unsigned(tick.run_tick),
            "wall_ns" => optional_unsigned(tick.wall_ns),
            "continuous_ns" => optional_unsigned(tick.continuous_ns),
            "awake_ns" => optional_unsigned(tick.awake_ns),
            "ioreport_read" => Cell::Bool(tick.ioreport_read),
            "smc_read" => Cell::Bool(tick.smc_read),
            "gpu_energy_nj" => optional_unsigned(tick.gpu_energy_nj),
            "power_watts" => tick.power_watts.map_or(Cell::Null, Cell::Float32),
            "soc_celsius" => tick.soc_celsius.map_or(Cell::Null, Cell::Float32),
            "cpu_celsius" => tick.cpu_celsius.map_or(Cell::Null, Cell::Float32),
            "gpu_celsius" => tick.gpu_celsius.map_or(Cell::Null, Cell::Float32),
            "processes_refused" => unsigned(self.raw().refused),
            "processes_gone" => unsigned(self.raw().gone),
            "events_first" => unsigned(self.raw().event_range.start),
            "events_end" => unsigned(self.raw().event_range.end),
            "dropped_starts" => unsigned(self.raw().dropped.starts),
            "dropped_execs" => unsigned(self.raw().dropped.execs),
            "dropped_exits" => unsigned(self.raw().dropped.exits),
            "gpu_state_count" => unsigned(collector::u64_from_usize(self.machine.gpu_states.len())),
            "core_count" => unsigned(self.raw().core_count),
            "physical_disks" => tick.disks.as_ref().map_or(Cell::Null, Cell::Disks),
            _ if name.starts_with("battery_") => self.battery(name),
            _ => self.system(name),
        }
    }

    fn battery(&self, name: &str) -> Cell<'a> {
        let reading = self.machine.tick.battery.as_ref();
        match name {
            "battery_version" => optional_unsigned(reading.map(|_| schema::BATTERY_VERSION)),
            "battery_source_continuous_ns" => {
                optional_unsigned(reading.map(|reading| reading.source_continuous_ns))
            }
            "battery_charge_current" => {
                optional_unsigned(reading.and_then(|reading| reading.charge_current))
            }
            "battery_charge_maximum" => {
                optional_unsigned(reading.and_then(|reading| reading.charge_maximum))
            }
            "battery_full_charge_capacity" => {
                optional_unsigned(reading.and_then(|reading| reading.full_charge_capacity))
            }
            "battery_design_capacity" => {
                optional_unsigned(reading.and_then(|reading| reading.design_capacity))
            }
            "battery_cycle_count" => {
                optional_unsigned(reading.and_then(|reading| reading.cycle_count))
            }
            "battery_voltage_mv" => {
                optional_unsigned(reading.and_then(|reading| reading.voltage_mv))
            }
            "battery_adapter_watts" => {
                optional_unsigned(reading.and_then(|reading| reading.adapter_watts))
            }
            "battery_charging" => boolean(reading.and_then(|reading| reading.charging)),
            "battery_external_power" => boolean(reading.and_then(|reading| reading.external_power)),
            "battery_current_ma" => reading
                .and_then(|reading| reading.current_ma)
                .map_or(Cell::Null, Cell::Signed),
            _ => unreachable!("battery columns"),
        }
    }

    fn system(&self, name: &str) -> Cell<'a> {
        let system = self.machine.tick.system.as_ref();
        let reading = system.map(|system| &system.reading);
        let machine = system.map(|system| &system.machine);
        match name {
            "load_average_1" => reading
                .and_then(|reading| reading.load_average)
                .map_or(Cell::Null, |averages| Cell::Float64(averages[0])),
            "load_average_5" => reading
                .and_then(|reading| reading.load_average)
                .map_or(Cell::Null, |averages| Cell::Float64(averages[1])),
            "load_average_15" => reading
                .and_then(|reading| reading.load_average)
                .map_or(Cell::Null, |averages| Cell::Float64(averages[2])),
            "open_files" => optional_unsigned(reading.and_then(|reading| reading.open_files)),
            "open_files_limit" => {
                optional_unsigned(reading.and_then(|reading| reading.open_files_limit))
            }
            "thermal_pressure" => {
                optional_unsigned(reading.and_then(|reading| reading.thermal_pressure))
            }
            "boot_wall_ns" => optional_unsigned(reading.and_then(|reading| reading.boot_wall_ns)),
            "gpu_memory_in_use_bytes" => optional_unsigned(
                reading
                    .and_then(|reading| reading.gpu_memory)
                    .and_then(|memory| memory.in_use_bytes),
            ),
            "gpu_memory_allocated_bytes" => optional_unsigned(
                reading
                    .and_then(|reading| reading.gpu_memory)
                    .and_then(|memory| memory.allocated_bytes),
            ),
            "machine_model" => self.machine.tick.computer_text()[0].map_or(Cell::Null, Cell::Text),
            "machine_chip" => self.machine.tick.computer_text()[1].map_or(Cell::Null, Cell::Text),
            "machine_architecture" => {
                self.machine.tick.computer_text()[2].map_or(Cell::Null, Cell::Text)
            }
            "machine_cpu_cores" => optional_unsigned(machine.and_then(|machine| machine.cpu_cores)),
            "machine_gpu_cores" => optional_unsigned(machine.and_then(|machine| machine.gpu_cores)),
            "machine_memory_bytes" => {
                optional_unsigned(machine.and_then(|machine| machine.memory_bytes))
            }
            "machine_performance_level0_name" => machine
                .and_then(|machine| machine.levels.as_ref())
                .map_or(Cell::Null, |levels| Cell::Text(levels[0].name.as_str())),
            "machine_performance_level0_logical_cpus" => optional_unsigned(
                machine
                    .and_then(|machine| machine.levels.as_ref())
                    .map(|levels| levels[0].logical_cpus),
            ),
            "machine_performance_level1_name" => machine
                .and_then(|machine| machine.levels.as_ref())
                .map_or(Cell::Null, |levels| Cell::Text(levels[1].name.as_str())),
            "machine_performance_level1_logical_cpus" => optional_unsigned(
                machine
                    .and_then(|machine| machine.levels.as_ref())
                    .map(|levels| levels[1].logical_cpus),
            ),
            _ => unreachable!("system columns"),
        }
    }

    fn memory(&self, name: &str) -> Cell<'a> {
        let memory = &self
            .machine
            .memory
            .unwrap_or_else(|| unreachable!("memory row"))
            .memory;
        match name {
            "total_bytes" => unsigned(memory.total_bytes),
            "free_bytes" => unsigned(memory.free_bytes),
            "active_bytes" => unsigned(memory.active_bytes),
            "inactive_bytes" => unsigned(memory.inactive_bytes),
            "speculative_bytes" => unsigned(memory.speculative_bytes),
            "wired_bytes" => unsigned(memory.wired_bytes),
            "compressed_bytes" => unsigned(memory.compressed_bytes),
            "anonymous_bytes" => unsigned(memory.anonymous_bytes),
            "file_backed_bytes" => unsigned(memory.file_backed_bytes),
            "purgeable_bytes" => unsigned(memory.purgeable_bytes),
            "swap_total_bytes" => unsigned(memory.swap_total_bytes),
            "swap_used_bytes" => unsigned(memory.swap_used_bytes),
            "swapped_in_bytes" => unsigned(memory.swapped_in_bytes),
            "swapped_out_bytes" => unsigned(memory.swapped_out_bytes),
            "pageins" => unsigned(memory.pageins),
            "pageouts" => unsigned(memory.pageouts),
            "compressions" => unsigned(memory.compressions),
            "decompressions" => unsigned(memory.decompressions),
            "pressure" => unsigned(match memory.pressure {
                collector::machine::Pressure::Normal => schema::pressure::NORMAL,
                collector::machine::Pressure::Warning => schema::pressure::WARNING,
                collector::machine::Pressure::Critical => schema::pressure::CRITICAL,
            }),
            _ => unreachable!("memory columns"),
        }
    }

    fn cpu(&self, row: usize, name: &str) -> Cell<'a> {
        let cpu = &self.machine.cpus[row];
        match name {
            "cpu" => unsigned(cpu.cpu),
            "user_ticks" => unsigned(cpu.ticks.user),
            "system_ticks" => unsigned(cpu.ticks.system),
            "nice_ticks" => unsigned(cpu.ticks.nice),
            "idle_ticks" => unsigned(cpu.ticks.idle),
            _ => unreachable!("CPU columns"),
        }
    }

    fn interface(&self, row: usize, name: &str) -> Cell<'a> {
        let interface = &self.machine.interfaces[row].interface;
        match name {
            "interface" => unsigned(interface.index),
            "name" => Cell::Text(interface.name.as_str()),
            "received_bytes" => unsigned(interface.received_bytes),
            "sent_bytes" => unsigned(interface.sent_bytes),
            "received_packets" => unsigned(interface.received_packets),
            "sent_packets" => unsigned(interface.sent_packets),
            "receive_errors" => unsigned(interface.receive_errors),
            "send_errors" => unsigned(interface.send_errors),
            "receive_drops" => unsigned(interface.receive_drops),
            _ => unreachable!("interface columns"),
        }
    }

    fn core(&self, row: usize, name: &str) -> Cell<'a> {
        let core = &self.machine.core_states[row];
        match name {
            "core" => unsigned(core.core),
            "state" => unsigned(core.state),
            "active_ns" => unsigned(core.active_ns),
            "frequency_khz" => optional_unsigned(core.frequency_khz),
            _ => unreachable!("core columns"),
        }
    }

    fn coalition(&self, row: usize, name: &str) -> Cell<'a> {
        let coalition = &self.raw().coalitions[row];
        match name {
            "coalition" => unsigned(coalition.id),
            "tasks_started" => unsigned(coalition.tasks_started),
            "tasks_exited" => unsigned(coalition.tasks_exited),
            "cpu_ns" => unsigned(coalition.cpu_ns),
            "interrupt_wakeups" => unsigned(coalition.interrupt_wakeups),
            "idle_wakeups" => unsigned(coalition.idle_wakeups),
            "disk_read_bytes" => unsigned(coalition.disk_read_bytes),
            "disk_written_bytes" => unsigned(coalition.disk_written_bytes),
            "gpu_ns" => unsigned(coalition.gpu_ns),
            "energy_nj" => unsigned(coalition.energy_nj),
            _ => unreachable!("coalition columns"),
        }
    }

    fn process_cell(&self, row: usize, name: &str) -> Cell<'a> {
        let (pid, process, identity) = self.process(row);
        match name {
            "pid" => Cell::Signed(i64::from(pid)),
            "gpu_ns" => optional_unsigned(self.raw().gpu_ns.get(row).copied().flatten()),
            "threads" => process.map_or(Cell::Null, |process| {
                Cell::Signed(i64::from(process.task.threads))
            }),
            "faults" => process.map_or(Cell::Null, |process| {
                Cell::Signed(i64::from(process.task.faults))
            }),
            "context_switches" => process.map_or(Cell::Null, |process| {
                Cell::Signed(i64::from(process.task.context_switches))
            }),
            "mach_syscalls" => process.map_or(Cell::Null, |process| {
                Cell::Signed(i64::from(process.task.mach_syscalls))
            }),
            "unix_syscalls" => process.map_or(Cell::Null, |process| {
                Cell::Signed(i64::from(process.task.unix_syscalls))
            }),
            _ if name.starts_with("identity_")
                || [
                    "usage_availability",
                    "process_unique_id",
                    "pid_version",
                    "executable_name",
                    "app_path",
                ]
                .contains(&name) =>
            {
                self.identity(identity, name)
            }
            _ => usage(process.map(|process| &process.usage), name),
        }
    }

    fn identity(&self, identity: Option<&'a super::Identity>, name: &str) -> Cell<'a> {
        match name {
            "identity_version" => optional_unsigned(identity.map(|_| schema::IDENTITY_VERSION)),
            "identity_availability" => unsigned(
                identity.map_or(schema::identity_availability::UNSUPPORTED, |identity| {
                    identity.availability
                }),
            ),
            "usage_availability" => {
                unsigned(identity.map_or(schema::availability::MEASURED, |identity| identity.usage))
            }
            "process_unique_id" => optional_unsigned(
                identity
                    .and_then(|identity| identity.creation)
                    .map(|creation| creation.unique_id),
            ),
            "pid_version" => identity
                .and_then(|identity| identity.creation)
                .map_or(Cell::Null, |creation| {
                    Cell::Signed(i64::from(creation.pid_version))
                }),
            "executable_name" => identity
                .and_then(|identity| identity.name.as_ref())
                .map_or(Cell::Null, |range| {
                    Cell::Bytes(&self.raw().arena[range.clone()])
                }),
            "app_path" => identity
                .and_then(|identity| identity.app.as_ref())
                .map_or(Cell::Null, |range| {
                    Cell::Bytes(&self.raw().arena[range.clone()])
                }),
            _ => unreachable!("identity columns"),
        }
    }

    fn event_cell(&self, row: usize, name: &str) -> Cell<'a> {
        let event = &self.raw().events[row];
        let (pid, measured, kind, coalition, availability, noticed) = event_facts(&event.event);
        match name {
            "event" => unsigned(event.number),
            "run_tick" => unsigned(event.tick),
            "pid" => Cell::Signed(i64::from(pid)),
            "kind" => unsigned(kind),
            "coalition" => optional_unsigned(coalition),
            "availability" => unsigned(availability),
            "noticed" => optional_unsigned(noticed),
            "usage_availability" if event.identity.is_none() => unsigned(availability),
            _ if name.starts_with("identity_")
                || [
                    "usage_availability",
                    "process_unique_id",
                    "pid_version",
                    "executable_name",
                    "app_path",
                ]
                .contains(&name) =>
            {
                self.identity(event.identity.as_ref(), name)
            }
            _ => usage(measured, name),
        }
    }
}

fn usage<'a>(usage: Option<&collector::processes::Usage>, name: &str) -> Cell<'a> {
    match name {
        "awake_ns_at_start" => optional_unsigned(usage.map(|usage| usage.awake_ns_at_start)),
        "user_ns" => optional_unsigned(usage.map(|usage| usage.user_ns)),
        "system_ns" => optional_unsigned(usage.map(|usage| usage.system_ns)),
        "energy_nj" => optional_unsigned(usage.map(|usage| usage.energy_nj)),
        "instructions" => optional_unsigned(usage.map(|usage| usage.instructions)),
        "super_instructions" => optional_unsigned(usage.map(|usage| usage.super_instructions)),
        "cycles" => optional_unsigned(usage.map(|usage| usage.cycles)),
        "idle_wakeups" => optional_unsigned(usage.map(|usage| usage.idle_wakeups)),
        "interrupt_wakeups" => optional_unsigned(usage.map(|usage| usage.interrupt_wakeups)),
        "pageins" => optional_unsigned(usage.map(|usage| usage.pageins)),
        "resident_bytes" => optional_unsigned(usage.map(|usage| usage.resident_bytes)),
        "footprint_bytes" => optional_unsigned(usage.map(|usage| usage.footprint_bytes)),
        "disk_read_bytes" => optional_unsigned(usage.map(|usage| usage.disk_read_bytes)),
        "disk_written_bytes" => optional_unsigned(usage.map(|usage| usage.disk_written_bytes)),
        _ => unreachable!("usage columns"),
    }
}
