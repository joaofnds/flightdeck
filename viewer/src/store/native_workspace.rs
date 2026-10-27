//! The existing native decoder buffers, charged before Source allocates them.
use collector::ring::{IdentityObservation, Shape, Tick};

pub(super) fn bytes(shape: &Shape) -> persistence::Result<usize> {
    persistence::Limits::from_shape(shape)?;
    let [cpus, interfaces, cores, states, fans, processes, coalitions] = [
        shape.cpu_capacity,
        shape.interface_capacity,
        shape.core_capacity,
        shape.state_capacity,
        shape.fan_capacity,
        shape.process_capacity,
        shape.coalition_capacity,
    ]
    .map(|count| usize::try_from(count).map_err(|_| persistence::Error::Bound("native capacity")));
    let (cpus, interfaces, cores, states, fans, processes, coalitions) = (
        cpus?,
        interfaces?,
        cores?,
        states?,
        fans?,
        processes?,
        coalitions?,
    );
    let tick = size_of::<Tick>()
        + cpus * size_of::<collector::machine::CPUTicks>()
        + interfaces * size_of::<collector::interfaces::Interface>()
        + states * size_of::<u64>()
        + cores * states * (size_of::<u64>() + size_of::<u32>())
        + cores * size_of::<usize>()
        + fans * size_of::<f32>()
        + processes * (size_of::<collector::processes::Process>() + size_of::<Option<u64>>())
        + coalitions * size_of::<collector::coalitions::Coalition>();
    let machine = size_of::<crate::recording::TickRow>()
        + size_of::<crate::recording::MemoryRow>()
        + cpus * size_of::<crate::recording::CpuRow>()
        + interfaces * size_of::<crate::recording::InterfaceRow>()
        + states * size_of::<crate::recording::GpuStateRow>()
        + cores * states * size_of::<crate::recording::CoreStateRow>()
        + fans * size_of::<crate::recording::FanRow>();
    let events = collector::tracker::EVENTS_PER_TICK_MAX
        * (size_of::<(u64, u64, collector::tracker::Event)>()
            + size_of::<IdentityObservation>()
            + size_of::<persistence::NumberedEvent<'static>>());
    Ok(tick + 2 * machine + processes * size_of::<IdentityObservation>() + events)
}
