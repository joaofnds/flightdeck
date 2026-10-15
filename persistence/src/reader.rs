//! Bounded owned buffers for persistence and independent source capture.
use collector::ring::{Shape, Tick, TickBuffers};

use crate::{Limits, Result};

/// Allocates once for a validated source shape, before reading its publications.
pub fn tick_buffer(shape: &Shape) -> Result<Tick> {
    let limits = Limits::from_shape(shape)?;
    Ok(Tick::from_buffers(TickBuffers {
        cpus: Vec::with_capacity(limits.cpus),
        interfaces: Vec::with_capacity(limits.interfaces),
        gpu_active_ns: Vec::with_capacity(limits.states),
        core_active_ns: vec![0; limits.cores * limits.states],
        core_state_counts: Vec::with_capacity(limits.cores),
        state_capacity: limits.states,
        fan_rpm: Vec::with_capacity(limits.fans),
        processes: Vec::with_capacity(limits.processes),
        coalitions: Vec::with_capacity(limits.coalitions),
    }))
}
