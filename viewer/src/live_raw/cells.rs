use model::catalog;
use persistence::{Error, Result, schema};

pub(crate) enum Cell<'a> {
    Null,
    Bool(bool),
    Unsigned(u64),
    Signed(i64),
    Float32(f32),
    Float64(f64),
    Text(&'a str),
}

pub(crate) struct View<'a> {
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
            _ => unreachable!("catalog raw table"),
        })
    }
}

impl Cell<'_> {
    pub fn number(&self) -> Result<Option<f64>> {
        match self {
            Self::Null => Ok(None),
            Self::Float64(number) => Ok(Some(*number)),
            Self::Float32(number) => Ok(number.is_finite().then(|| f64::from(*number))),
            _ => Err(Error::Bound("number metric cell")),
        }
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
            "gpu_state_count" => unsigned(collector::u64_from_usize(self.machine.gpu_states.len())),
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
            "load_average_1" => {
                self.machine.tick.load_average()[0].map_or(Cell::Null, Cell::Float64)
            }
            "load_average_5" => {
                self.machine.tick.load_average()[1].map_or(Cell::Null, Cell::Float64)
            }
            "load_average_15" => {
                self.machine.tick.load_average()[2].map_or(Cell::Null, Cell::Float64)
            }
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
}
