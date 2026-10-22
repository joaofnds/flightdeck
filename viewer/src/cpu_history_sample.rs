use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::data::Table;
use crate::session::Observation;

pub(crate) const CPUS_MAX: usize = 1024;
pub(crate) type Counters = Vec<[Option<u64>; 4]>;

#[derive(Clone)]
pub(crate) struct Identity {
    pub run: String,
    pub boot: String,
    pub observation: Observation,
}

pub(crate) struct CounterSample {
    pub identity: Identity,
    pub counters: Counters,
}

pub(crate) struct Interval<'a> {
    pub current: &'a CounterSample,
    pub previous: Option<&'a CounterSample>,
}

#[derive(Clone, Copy)]
pub(crate) struct CounterInterval<'a> {
    pub current: &'a Counters,
    pub previous: &'a Counters,
}

#[derive(Clone, Copy)]
struct BusyInterval<'a> {
    current: &'a [Option<u64>; 4],
    previous: &'a [Option<u64>; 4],
}

const _: () = {
    assert!(size_of::<CounterInterval<'static>>() <= 16);
    assert!(size_of::<BusyInterval<'static>>() <= 16);
    assert!(size_of::<Option<f64>>() == 16);
    assert!(size_of::<Option<u64>>() == 16);
};

impl Identity {
    pub fn read(value: &Value, observation: &Observation, output: &mut Option<Self>) -> Result<()> {
        assert!(output.is_none());

        let run = value["run"]
            .as_str()
            .ok_or(Error::Bound("CPU run identity"))?;
        let boot = value["boot"]
            .as_str()
            .ok_or(Error::Bound("CPU boot identity"))?;
        if run.len() > 64 {
            return Err(Error::Bound("CPU run identity bytes"));
        }

        if boot.len() > 64 {
            return Err(Error::Bound("CPU boot identity bytes"));
        }

        *output = Some(Self {
            run: run.to_owned(),
            boot: boot.to_owned(),
            observation: observation.clone(),
        });

        Ok(())
    }

    pub fn json(&self) -> Value {
        json!({"run":self.run,"boot":self.boot,
            "publication":self.observation.publication,"run_tick":self.observation.run_tick,
            "wall_ns":self.observation.wall})
    }

    pub fn same(&self, other: &Self) -> bool {
        self.run == other.run
            && self.boot == other.boot
            && self.observation.publication == other.observation.publication
            && self.observation.run_tick == other.observation.run_tick
            && self.observation.wall == other.observation.wall
    }
}

pub(crate) fn counters(table: &Table, publication: u64, output: &mut Counters) -> Result<()> {
    assert!(output.is_empty());
    if table.rows.len() > CPUS_MAX {
        return Err(Error::Bound("logical CPU count"));
    }

    let mut ordered = vec![None; table.rows.len()];
    for row in &table.rows {
        if table.integer(row, "publication")? != Some(publication) {
            return Err(Error::Bound("CPU detail publication"));
        }

        let cpu = table
            .integer(row, "cpu")?
            .ok_or(Error::Bound("missing logical CPU index"))?;
        let cpu = u32::try_from(cpu).map_err(|_| Error::Bound("logical CPU index"))?;
        let cpu = usize::try_from(cpu).map_err(|_| Error::Bound("logical CPU index"))?;
        let slot = ordered
            .get_mut(cpu)
            .ok_or(Error::Bound("incomplete logical CPU domain"))?;
        if slot.is_some() {
            return Err(Error::Bound("duplicate logical CPU index"));
        }

        let mut values = [None; 4];
        for (index, name) in ["user_ticks", "system_ticks", "nice_ticks", "idle_ticks"]
            .into_iter()
            .enumerate()
        {
            values[index] = table.integer(row, name)?;
        }
        *slot = Some(values);
    }

    output.reserve_exact(ordered.len());
    for slot in ordered {
        output.push(slot.ok_or(Error::Bound("incomplete logical CPU domain"))?);
    }

    Ok(())
}

pub(crate) fn derive(
    interval: &Interval<'_>,
    output: &mut Vec<Option<f64>>,
) -> Result<Option<&'static str>> {
    assert!(output.is_empty());
    assert!(interval.current.counters.len() <= CPUS_MAX);

    let unavailable = derive_unavailable(interval);
    output.reserve_exact(interval.current.counters.len());
    for (cpu, current) in interval.current.counters.iter().enumerate() {
        let busy = if unavailable.is_none() {
            let previous = interval
                .previous
                .ok_or(Error::Bound("CPU interval predecessor"))?;
            derive_busy(BusyInterval {
                current,
                previous: &previous.counters[cpu],
            })?
        } else {
            None
        };
        output.push(busy);
    }

    Ok(unavailable)
}

pub(crate) fn sum_busy(interval: CounterInterval<'_>) -> Result<Option<f64>> {
    sum_busy_slices(&CounterSlices {
        current: interval.current,
        previous: interval.previous,
    })
}

/// The counters of one interval for some of the logical CPUs.
struct CounterSlices<'a> {
    current: &'a [[Option<u64>; 4]],
    previous: &'a [[Option<u64>; 4]],
}

fn sum_busy_slices(interval: &CounterSlices<'_>) -> Result<Option<f64>> {
    assert!(interval.current.len() <= CPUS_MAX);
    assert!(interval.previous.len() <= CPUS_MAX);
    if interval.current.is_empty() {
        return Ok(None);
    }

    if interval.current.len() != interval.previous.len() {
        return Ok(None);
    }

    let mut sum = 0.0;
    for (current, previous) in interval.current.iter().zip(interval.previous.iter()) {
        let Some(percent) = derive_busy(BusyInterval { current, previous })? else {
            return Ok(None);
        };

        sum += percent;
    }
    assert!(sum.is_finite());

    Ok(Some(sum))
}

/// The mean busy share of each performance level's CPUs, level 0 first. The slower level's
/// CPUs take the lowest indices, as macOS numbers them. Levels that do not cover the recorded
/// CPUs read nothing, since no CPU can then be placed in a level.
pub(crate) fn level_busy(
    interval: CounterInterval<'_>,
    levels: collector::machine::PerformanceLevels,
) -> Result<[Option<f64>; 2]> {
    assert!(interval.current.len() <= CPUS_MAX);
    let [fast, slow] = levels.logical_cpus.map(|count| usize::try_from(count).ok());
    let (Some(fast), Some(slow)) = (fast, slow) else {
        return Ok([None, None]);
    };
    if fast.checked_add(slow) != Some(interval.current.len()) {
        return Ok([None, None]);
    }
    if interval.previous.len() != interval.current.len() {
        return Ok([None, None]);
    }

    let slow_busy = mean_busy(&CounterSlices {
        current: &interval.current[..slow],
        previous: &interval.previous[..slow],
    })?;
    let fast_busy = mean_busy(&CounterSlices {
        current: &interval.current[slow..],
        previous: &interval.previous[slow..],
    })?;

    Ok([fast_busy, slow_busy])
}

fn mean_busy(interval: &CounterSlices<'_>) -> Result<Option<f64>> {
    let Some(sum) = sum_busy_slices(interval)? else {
        return Ok(None);
    };
    let count = u32::try_from(interval.current.len())
        .map_err(|_| Error::Bound("performance level CPU count"))?;
    assert!(count > 0);

    Ok(Some(sum / f64::from(count)))
}

fn derive_unavailable(interval: &Interval<'_>) -> Option<&'static str> {
    let Some(previous) = interval.previous else {
        return Some("Previous CPU reading unavailable");
    };

    if interval.current.identity.run != previous.identity.run {
        return Some("CPU readings belong to different runs");
    }

    if interval.current.identity.boot != previous.identity.boot {
        return Some("CPU readings belong to different boots");
    }

    if !crate::metrics::compatible(
        &previous.identity.observation,
        &interval.current.identity.observation,
    ) {
        return Some("Requires consecutive compatible CPU readings");
    }

    if interval.current.counters.is_empty() {
        return Some("CPU readings unavailable");
    }

    if interval.current.counters.len() != previous.counters.len() {
        return Some("Logical CPU domain changed");
    }

    None
}

fn derive_busy(interval: BusyInterval<'_>) -> Result<Option<f64>> {
    let mut total = 0_u64;
    let mut busy = 0_u64;
    for index in 0..4 {
        let (Some(now), Some(old)) = (interval.current[index], interval.previous[index]) else {
            return Ok(None);
        };

        let Some(delta) = now.checked_sub(old) else {
            return Ok(None);
        };

        total = total
            .checked_add(delta)
            .ok_or(Error::Bound("CPU counter total"))?;
        if index < 3 {
            busy = busy
                .checked_add(delta)
                .ok_or(Error::Bound("CPU busy total"))?;
        }
    }

    if total == 0 {
        return Ok(None);
    }

    let percent = crate::metrics::number(busy) / crate::metrics::number(total) * 100.0;
    assert!(percent.is_finite());
    assert!((0.0..=100.0).contains(&percent));

    Ok(Some(percent))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticks(busy: u64, idle: u64) -> [Option<u64>; 4] {
        [Some(busy), Some(0), Some(0), Some(idle)]
    }

    #[test]
    fn level_busy_averages_each_level_over_its_own_cpus() {
        // Goal: each level reads the mean busy share of its own CPUs, the slow level's CPUs
        // taking the lowest indices.
        // Method: two slow CPUs at 0% and 50% and one fast CPU at 100% over one interval.
        let previous = vec![ticks(0, 0), ticks(0, 0), ticks(0, 0)];
        let current = vec![ticks(0, 100), ticks(50, 50), ticks(100, 0)];
        let levels = collector::machine::PerformanceLevels {
            logical_cpus: [1, 2],
        };

        let busy = level_busy(
            CounterInterval {
                current: &current,
                previous: &previous,
            },
            levels,
        )
        .unwrap();

        assert_eq!(busy, [Some(100.0), Some(25.0)]);
    }

    #[test]
    fn level_busy_is_unavailable_when_the_levels_do_not_cover_the_recorded_cpus() {
        // Goal: a recording whose CPUs the levels do not cover reads no level, since its CPUs
        // cannot be placed in a level.
        // Method: levels of four CPUs against three recorded CPUs.
        let previous = vec![ticks(0, 0), ticks(0, 0), ticks(0, 0)];
        let current = vec![ticks(0, 100), ticks(50, 50), ticks(100, 0)];
        let levels = collector::machine::PerformanceLevels {
            logical_cpus: [2, 2],
        };

        let busy = level_busy(
            CounterInterval {
                current: &current,
                previous: &previous,
            },
            levels,
        )
        .unwrap();

        assert_eq!(busy, [None, None]);
    }

    #[test]
    fn level_busy_withholds_only_the_level_with_an_unread_cpu() {
        // Goal: a CPU without a reading withholds its own level and leaves the other level read.
        // Method: the second slow CPU lacks its idle ticks.
        let previous = vec![ticks(0, 0), ticks(0, 0), ticks(0, 0)];
        let mut unread = ticks(50, 50);
        unread[3] = None;
        let current = vec![ticks(0, 100), unread, ticks(100, 0)];
        let levels = collector::machine::PerformanceLevels {
            logical_cpus: [1, 2],
        };

        let busy = level_busy(
            CounterInterval {
                current: &current,
                previous: &previous,
            },
            levels,
        )
        .unwrap();

        assert_eq!(busy, [Some(100.0), None]);
    }
}
