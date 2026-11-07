//! Typed cells of one saved reading's machine rows, and the checks those rows pass before a
//! reader takes them.
use persistence::{Error, Result};

#[path = "live_raw/cells.rs"]
mod cells;
pub(crate) use cells::{Cell, View};

pub(crate) fn validate_machine_tick(tick: &crate::recording::TickRow) -> Result<()> {
    validate_machine_numbers(&MachineNumbers {
        smc: [tick.power_watts, tick.soc_celsius],
        temperatures: [tick.cpu_celsius, tick.gpu_celsius],
        load_average: tick.load_average(),
        system: tick.system.as_ref(),
        disks: tick.disks.as_ref(),
    })
}

struct MachineNumbers<'tick> {
    smc: [Option<f32>; 2],
    temperatures: [Option<f32>; 2],
    load_average: [Option<f64>; 3],
    system: Option<&'tick collector::ring::SystemSnapshot>,
    disks: Option<&'tick collector::disks::DiskSnapshot>,
}

fn validate_machine_numbers(numbers: &MachineNumbers<'_>) -> Result<()> {
    for number in numbers.smc.into_iter().flatten() {
        if !number.is_finite() {
            return Err(Error::Bound("unsupported or nonfinite cell"));
        }
    }
    for number in numbers.temperatures.into_iter().flatten() {
        if collector::smc::usable_celsius(number).is_none() {
            return Err(Error::Bound("temperature reading"));
        }
    }
    validate_machine_numbers_load_average(&numbers.load_average)?;
    if let Some(system) = numbers.system {
        validate_system(system)?;
    }
    if let Some(disks) = numbers.disks {
        let mut words = [0; collector::disks::encoding::WORDS_MAX];
        collector::disks::encoding::encode(disks, &mut words)
            .map_err(|_| Error::Bound("physical disk snapshot"))?;
    }
    Ok(())
}

fn validate_machine_numbers_load_average(averages: &[Option<f64>; 3]) -> Result<()> {
    for average in averages.iter().flatten() {
        if !average.is_finite() {
            return Err(Error::Bound("system load average"));
        }
        if *average < 0.0 {
            return Err(Error::Bound("system load average"));
        }
    }
    Ok(())
}

fn validate_system(system: &collector::ring::SystemSnapshot) -> Result<()> {
    let machine = &system.machine;
    for text in [
        machine.model.as_ref(),
        machine.chip.as_ref(),
        machine.architecture.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if text.as_str().is_empty() {
            return Err(Error::Bound("machine fact text"));
        }
    }
    if let Some(levels) = &machine.levels {
        for level in levels {
            if level.name.as_str().is_empty() {
                return Err(Error::Bound("machine fact text"));
            }
        }
    }
    Ok(())
}
