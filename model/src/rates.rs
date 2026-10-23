//! What a counter's growth over an interval comes to per unit of time, divided only once the
//! exact sums are known, so every stage that shows a rate shows the same one.

/// Numerator and continuous-time denominator for one-core CPU utilization.
#[derive(Clone, Copy)]
pub struct CpuRate {
    pub cpu_ns: u64,
    pub elapsed_ns: u64,
}

/// Energy delta and continuous-time denominator for average power.
#[derive(Clone, Copy)]
pub struct EnergyRate {
    pub energy_nj: u64,
    pub elapsed_ns: u64,
}

/// GPU time delta and continuous-time denominator for the share of the interval the GPU spent.
#[derive(Clone, Copy)]
pub struct GpuRate {
    pub gpu_ns: u64,
    pub elapsed_ns: u64,
}

/// Exact interval bytes and its continuous-time denominator for disk throughput.
#[derive(Clone, Copy)]
pub struct BytesRate {
    pub bytes: u64,
    pub elapsed_ns: u64,
}

#[derive(Clone, Copy)]
pub struct EventRate {
    pub events: u64,
    pub elapsed_ns: u64,
}

/// Floating conversion happens only at the display boundary, after exact delta summation.
#[must_use]
pub fn cpu_percent_one_core(rate: CpuRate) -> Option<f64> {
    if rate.elapsed_ns == 0 {
        return None;
    }
    Some(number(rate.cpu_ns) / number(rate.elapsed_ns) * 100.0)
}

#[must_use]
pub fn energy_joules(energy_nj: u64) -> f64 {
    number(energy_nj) / 1_000_000_000.0
}

#[must_use]
pub fn energy_watts(rate: EnergyRate) -> Option<f64> {
    if rate.elapsed_ns == 0 {
        return None;
    }
    Some(number(rate.energy_nj) / number(rate.elapsed_ns))
}

/// The GPU time as a percent of the interval, which passes 100 where an App's clients overlap.
#[must_use]
pub fn gpu_percent(rate: GpuRate) -> Option<f64> {
    if rate.elapsed_ns == 0 {
        return None;
    }

    Some(number(rate.gpu_ns) / number(rate.elapsed_ns) * 100.0)
}

#[must_use]
pub fn bytes_per_second(rate: BytesRate) -> Option<f64> {
    if rate.elapsed_ns == 0 {
        return None;
    }

    Some(number(rate.bytes) / number(rate.elapsed_ns) * 1_000_000_000.0)
}

/// Exact event counts divided only after aggregation at the presentation boundary.
#[must_use]
pub fn events_per_second(rate: EventRate) -> Option<f64> {
    if rate.elapsed_ns == 0 {
        return None;
    }

    Some(number(rate.events) / number(rate.elapsed_ns) * 1_000_000_000.0)
}

fn number(value: u64) -> f64 {
    let upper = u32::try_from(value >> 32).unwrap_or_else(|_| unreachable!());
    let lower = u32::try_from(value & u64::from(u32::MAX)).unwrap_or_else(|_| unreachable!());
    f64::from(upper) * 4_294_967_296.0 + f64::from(lower)
}
