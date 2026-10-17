use std::io::Write;

use collector::processes::Usage;
use collector::ring::Tick;
use collector::tracker::{Event, Noticed};
use persistence::Result;

pub const REFERENCE_BYTES_MAX: u64 = 8 * 1024 * 1024 * 1024;
pub const REFERENCE_LINES_MAX: u64 = 100_000_000;
pub const REFERENCE_LINE_BYTES_MAX: u64 = 32_768;

const _: () = {
    assert!(REFERENCE_LINE_BYTES_MAX > 4 * 4095 + 1024);
    assert!(REFERENCE_LINE_BYTES_MAX < REFERENCE_BYTES_MAX);
};

pub const TABLES: [(&str, &str); 10] = [
    ("ticks", "publication"),
    ("memory", "publication"),
    ("cpus", "publication,cpu"),
    ("interfaces", "publication,interface"),
    ("gpu_states", "publication,state"),
    ("core_states", "publication,core,state"),
    ("fans", "publication,fan"),
    ("processes", "publication,pid,awake_ns_at_start"),
    ("coalitions", "publication,coalition"),
    ("events", "publication,event"),
];

pub struct Row<'writer, W: Write> {
    writer: RowWriter<'writer, W>,
}

struct RowWriter<'writer, W: Write> {
    writer: &'writer mut W,
    bytes: u64,
}

impl<W: Write> Write for RowWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let count = u64::try_from(buffer.len()).map_err(std::io::Error::other)?;
        if count > REFERENCE_LINE_BYTES_MAX - self.bytes {
            return Err(std::io::Error::other("reference disk row bound"));
        }
        self.writer.write_all(buffer)?;
        self.bytes += count;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

impl<'writer, W: Write> Row<'writer, W> {
    pub fn new(writer: &'writer mut W, table: &str, publication: u64) -> Result<Self> {
        let mut writer = RowWriter { writer, bytes: 0 };
        write!(writer, "{table} u{publication}")?;
        Ok(Self { writer })
    }
    pub fn unsigned(&mut self, value: u64) -> Result<()> {
        write!(self.writer, " u{value}")?;
        Ok(())
    }
    pub fn signed(&mut self, value: i64) -> Result<()> {
        write!(self.writer, " i{value}")?;
        Ok(())
    }
    pub fn boolean(&mut self, value: bool) -> Result<()> {
        write!(self.writer, " b{}", u8::from(value))?;
        Ok(())
    }
    pub fn float(&mut self, value: f32) -> Result<()> {
        write!(self.writer, " f{}", value.to_bits())?;
        Ok(())
    }
    pub fn text(&mut self, value: &str) -> Result<()> {
        write!(self.writer, " s")?;
        for byte in value.bytes() {
            write!(self.writer, "{byte:02x}")?;
        }
        Ok(())
    }
    pub fn blob(&mut self, bytes: &[u8]) -> Result<()> {
        write!(self.writer, " x")?;
        for byte in bytes {
            write!(self.writer, "{byte:02x}")?;
        }

        Ok(())
    }
    pub fn null(&mut self) -> Result<()> {
        write!(self.writer, " n")?;
        Ok(())
    }
    pub fn optional(&mut self, value: Option<u64>) -> Result<()> {
        match value {
            Some(value) => self.unsigned(value),
            None => self.null(),
        }
    }
    pub fn numbers(&mut self, values: &[u64]) -> Result<()> {
        for value in values {
            self.unsigned(*value)?;
        }
        Ok(())
    }
    pub fn end(mut self) -> Result<()> {
        writeln!(self.writer)?;
        Ok(())
    }
}

pub fn tick(writer: &mut impl Write, publication: u64, tick: &Tick) -> Result<()> {
    let mut row = Row::new(writer, "ticks", publication)?;
    row.unsigned(tick.run_tick)?;
    row.optional(tick.times.map(|value| value.wall_ns))?;
    row.optional(tick.times.map(|value| value.continuous_ns))?;
    row.optional(tick.times.map(|value| value.awake_ns))?;
    row.boolean(tick.ioreport_read)?;
    row.boolean(tick.smc_read)?;
    row.optional(tick.ioreport_read.then_some(tick.gpu_energy_nj))?;
    if tick.smc_read {
        row.float(tick.power_watts)?;
        row.float(tick.soc_celsius)?;
    } else {
        row.null()?;
        row.null()?;
    }
    row.numbers(&[
        u64::from(tick.processes_refused),
        u64::from(tick.processes_gone),
        tick.events.start,
        tick.events.end,
        tick.dropped.starts,
        tick.dropped.execs,
        tick.dropped.exits,
        u64::try_from(tick.gpu_active_ns.len()).map_err(|_| bound())?,
        u64::try_from(tick.core_count()).map_err(|_| bound())?,
    ])?;
    row.end()?;
    memory(writer, publication, tick)?;
    machine(writer, publication, tick)?;
    processes(writer, publication, tick)
}

fn memory(writer: &mut impl Write, publication: u64, tick: &Tick) -> Result<()> {
    let memory = tick.memory;
    let pressure = match memory.pressure {
        collector::machine::Pressure::Normal => 0,
        collector::machine::Pressure::Warning => 1,
        collector::machine::Pressure::Critical => 2,
    };
    let mut row = Row::new(writer, "memory", publication)?;
    row.numbers(&[
        memory.total_bytes,
        memory.free_bytes,
        memory.active_bytes,
        memory.inactive_bytes,
        memory.speculative_bytes,
        memory.wired_bytes,
        memory.compressed_bytes,
        memory.anonymous_bytes,
        memory.file_backed_bytes,
        memory.purgeable_bytes,
        memory.swap_total_bytes,
        memory.swap_used_bytes,
        memory.swapped_in_bytes,
        memory.swapped_out_bytes,
        memory.pageins,
        memory.pageouts,
        memory.compressions,
        memory.decompressions,
        pressure,
    ])?;
    row.end()
}

fn machine(writer: &mut impl Write, publication: u64, tick: &Tick) -> Result<()> {
    for (index, cpu) in tick.cpus.iter().enumerate() {
        let mut row = Row::new(writer, "cpus", publication)?;
        row.numbers(&[
            u64::try_from(index).map_err(|_| bound())?,
            u64::from(cpu.user),
            u64::from(cpu.system),
            u64::from(cpu.nice),
            u64::from(cpu.idle),
        ])?;
        row.end()?;
    }
    let mut interfaces: Vec<_> = tick.interfaces.iter().collect();
    interfaces.sort_unstable_by_key(|value| value.index);
    for interface in interfaces {
        let mut row = Row::new(writer, "interfaces", publication)?;
        row.unsigned(u64::from(interface.index))?;
        row.text(interface.name.as_str())?;
        row.numbers(&[
            interface.received_bytes,
            interface.sent_bytes,
            interface.received_packets,
            interface.sent_packets,
            interface.receive_errors,
            interface.send_errors,
            interface.receive_drops,
        ])?;
        row.end()?;
    }
    states(writer, publication, tick)
}

fn states(writer: &mut impl Write, publication: u64, tick: &Tick) -> Result<()> {
    for (index, active) in tick.gpu_active_ns.iter().enumerate() {
        let mut row = Row::new(writer, "gpu_states", publication)?;
        row.numbers(&[u64::try_from(index).map_err(|_| bound())?, *active])?;
        row.end()?;
    }
    for core in 0..tick.core_count() {
        for (state, active) in tick.core_active_ns(core).iter().enumerate() {
            let mut row = Row::new(writer, "core_states", publication)?;
            row.numbers(&[
                u64::try_from(core).map_err(|_| bound())?,
                u64::try_from(state).map_err(|_| bound())?,
                *active,
            ])?;
            row.end()?;
        }
    }
    for (index, rpm) in tick.fan_rpm.iter().enumerate() {
        let mut row = Row::new(writer, "fans", publication)?;
        row.unsigned(u64::try_from(index).map_err(|_| bound())?)?;
        row.float(*rpm)?;
        row.end()?;
    }
    Ok(())
}

fn processes(writer: &mut impl Write, publication: u64, tick: &Tick) -> Result<()> {
    let mut processes: Vec<_> = tick.processes.iter().collect();
    processes.sort_unstable_by_key(|value| (value.pid, value.usage.awake_ns_at_start));
    for process in processes {
        let mut row = Row::new(writer, "processes", publication)?;
        row.signed(i64::from(process.pid))?;
        process_payload(&mut row, process)?;
        row.end()?;
    }
    let mut coalitions: Vec<_> = tick.coalitions.iter().collect();
    coalitions.sort_unstable_by_key(|value| value.id);
    for coalition in coalitions {
        let mut row = Row::new(writer, "coalitions", publication)?;
        row.numbers(&[
            coalition.id,
            coalition.tasks_started,
            coalition.tasks_exited,
            coalition.cpu_ns,
            coalition.interrupt_wakeups,
            coalition.idle_wakeups,
            coalition.disk_read_bytes,
            coalition.disk_written_bytes,
            coalition.gpu_ns,
            coalition.energy_nj,
        ])?;
        row.end()?;
    }
    Ok(())
}

pub fn process_payload<W: Write>(
    row: &mut Row<'_, W>,
    process: &collector::processes::Process,
) -> Result<()> {
    usage(row, Some(process.usage))?;
    for value in [
        process.task.threads,
        process.task.faults,
        process.task.context_switches,
        process.task.mach_syscalls,
        process.task.unix_syscalls,
    ] {
        row.signed(i64::from(value))?;
    }

    Ok(())
}

pub fn event(
    writer: &mut impl Write,
    publication: u64,
    number: u64,
    run_tick: u64,
    event: &Event,
) -> Result<()> {
    let mut row = Row::new(writer, "events", publication)?;
    row.numbers(&[number, run_tick])?;
    match event {
        Event::Start { pid, coalition } => {
            row.signed(i64::from(*pid))?;
            row.unsigned(1)?;
            row.optional(*coalition)?;
            row.unsigned(3)?;
            row.null()?;
            usage(&mut row, None)?;
        }
        Event::Exec { pid } => {
            row.signed(i64::from(*pid))?;
            row.unsigned(2)?;
            row.null()?;
            row.unsigned(3)?;
            row.null()?;
            usage(&mut row, None)?;
        }
        Event::Exit { pid, last, noticed } => {
            row.signed(i64::from(*pid))?;
            row.unsigned(3)?;
            row.null()?;
            let (availability, last) = match last {
                Ok(value) => (0, Some(*value)),
                Err(collector::processes::Unreadable::Refused) => (1, None),
                Err(collector::processes::Unreadable::Gone) => (2, None),
            };
            row.unsigned(availability)?;
            row.unsigned(match noticed {
                Noticed::OnExit => 0,
                Noticed::AlreadyExited => 1,
            })?;
            usage(&mut row, last)?;
        }
    }
    row.end()
}

fn usage<W: Write>(row: &mut Row<'_, W>, value: Option<Usage>) -> Result<()> {
    let values = value.map(|value| {
        [
            value.awake_ns_at_start,
            value.user_ns,
            value.system_ns,
            value.energy_nj,
            value.instructions,
            value.super_instructions,
            value.cycles,
            value.idle_wakeups,
            value.interrupt_wakeups,
            value.pageins,
            value.resident_bytes,
            value.footprint_bytes,
            value.disk_read_bytes,
            value.disk_written_bytes,
        ]
    });
    for index in 0..14 {
        row.optional(values.map(|value| value[index]))?;
    }
    Ok(())
}

fn bound() -> persistence::Error {
    persistence::Error::Bound("reference row count")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_disk_bytes_before_growing_the_reserved_row_buffer() {
        // Goal: neither text nor BLOB expansion allocates past the serialized row limit.
        // Method: format each oversized cell into a buffer reserved to the exact bound.
        for text in [true, false] {
            let mut bytes = Vec::with_capacity(usize::try_from(REFERENCE_LINE_BYTES_MAX).unwrap());
            let capacity = bytes.capacity();
            let mut row = Row::new(&mut bytes, "interfaces", 1).unwrap();
            let value = "a".repeat(20_000);
            let result = if text {
                row.text(&value)
            } else {
                row.blob(value.as_bytes())
            };
            assert!(result.is_err());
            assert!(u64::try_from(bytes.len()).unwrap() <= REFERENCE_LINE_BYTES_MAX);
            assert_eq!(bytes.capacity(), capacity);
        }
    }
}
