use std::ffi::{CStr, c_char, c_void};
use std::io;
use std::ptr;

use crate::clock::Timebase;
use crate::push_without_growing;

type CFTypeRef = *const c_void;

/// The callbacks an array runs on its values, as `CFArrayCallBacks` declares them.
#[repr(C)]
struct ArrayCallBacks {
    version: isize,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
}

const _: () = assert!(size_of::<ArrayCallBacks>() == 40);
const _: () = assert!(align_of::<ArrayCallBacks>() == 8);

const UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: ArrayCallBacks;
    fn CFRelease(object: CFTypeRef);
    fn CFGetTypeID(object: CFTypeRef) -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
    fn CFArrayCreateMutable(
        allocator: CFTypeRef,
        capacity: isize,
        callbacks: *const ArrayCallBacks,
    ) -> CFTypeRef;
    fn CFArrayAppendValue(array: CFTypeRef, value: CFTypeRef);
    fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFDictionarySetValue(dictionary: CFTypeRef, key: CFTypeRef, value: CFTypeRef);
    fn CFStringCreateWithCString(
        allocator: CFTypeRef,
        text: *const c_char,
        encoding: u32,
    ) -> CFTypeRef;
    fn CFStringGetCString(text: CFTypeRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
}

#[link(name = "IOReport")]
unsafe extern "C" {
    fn IOReportCopyChannelsInGroup(
        group: CFTypeRef,
        subgroup: CFTypeRef,
        a: u64,
        b: u64,
        c: u64,
    ) -> CFTypeRef;
    fn IOReportMergeChannels(into: CFTypeRef, from: CFTypeRef, unused: CFTypeRef);
    fn IOReportCreateSubscription(
        unused: CFTypeRef,
        desired: CFTypeRef,
        subscribed: *mut CFTypeRef,
        channel_id: u64,
        options: CFTypeRef,
    ) -> CFTypeRef;
    fn IOReportCreateSamples(
        subscription: CFTypeRef,
        subscribed: CFTypeRef,
        options: CFTypeRef,
    ) -> CFTypeRef;
    fn IOReportChannelGetGroup(channel: CFTypeRef) -> CFTypeRef;
    fn IOReportChannelGetSubGroup(channel: CFTypeRef) -> CFTypeRef;
    fn IOReportChannelGetChannelName(channel: CFTypeRef) -> CFTypeRef;
    fn IOReportChannelGetUnitLabel(channel: CFTypeRef) -> CFTypeRef;
    fn IOReportSimpleGetIntegerValue(channel: CFTypeRef, index: i32) -> i64;
    fn IOReportStateGetCount(channel: CFTypeRef) -> i32;
    fn IOReportStateGetNameForIndex(channel: CFTypeRef, index: i32) -> CFTypeRef;
    fn IOReportStateGetResidency(channel: CFTypeRef, index: i32) -> i64;
}

/// A CoreFoundation object a create or copy call returned, which is never null and which only
/// this owner releases.
struct Owned(CFTypeRef);

impl Owned {
    /// Owns `object`, which the call `call` names returned, or reports the call where it
    /// returned null.
    fn new(object: CFTypeRef, call: &str) -> io::Result<Self> {
        if object.is_null() {
            return Err(io::Error::other(format!("{call} returned null")));
        }

        Ok(Self(object))
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        assert!(!self.0.is_null());

        unsafe { CFRelease(self.0) };
    }
}

#[derive(Debug, Clone)]
pub struct States {
    pub active_ns: Vec<u64>,
    /// The index of each state the channel counts as active, learned once at open from the
    /// state names, since `IOReport` allocates for each name it gives.
    active_indices: Vec<i32>,
    /// The states the channel reported at open, which `active_indices` index.
    state_count: i32,
}

/// The most active performance states read from one channel, three times the 20 a core of
/// the M5 Pro reports.
const ACTIVE_STATES_MAX: usize = 64;
/// The most states learned from one channel, past ten times the 22 a core of the M5 Pro
/// reports.
const STATES_MAX: i32 = 256;

impl States {
    fn new(active_states_max: usize) -> Self {
        Self {
            active_ns: Vec::with_capacity(active_states_max),
            active_indices: Vec::with_capacity(active_states_max),
            state_count: 0,
        }
    }
}

pub struct IOReport {
    timebase: Timebase,
    channels_key: Owned,
    subscription: Owned,
    subscribed: Owned,
    pub gpu_energy_nj: u64,
    /// The nanojoules in one unit of the GPU's energy, learned at open.
    gpu_nj_per_unit: u64,
    pub gpu: States,
    /// One for each CPU the machine runs, of which the first `counts.cores` hold the cores open
    /// found.
    cores: Vec<States>,
    /// The channels of each kind open found, which every read finds again.
    counts: ChannelCounts,
}

impl IOReport {
    pub fn open(timebase: Timebase, cpu_limit: usize) -> io::Result<Self> {
        let channels_key = string(c"IOReportChannels")?;
        let wanted = channels_in_group(c"Energy Model", None)?;
        merge_group(&wanted, c"GPU Stats", c"GPU Performance States")?;
        merge_group(&wanted, c"CPU Stats", c"CPU Core Performance States")?;
        keep_read_channels(&wanted, &channels_key)?;

        let mut subscribed = ptr::null();
        let subscription = unsafe {
            IOReportCreateSubscription(ptr::null(), wanted.0, &raw mut subscribed, 0, ptr::null())
        };
        let subscription = Owned::new(subscription, "subscribing to IOReport's channels")?;
        let subscribed = Owned::new(subscribed, "listing the channels IOReport subscribed to")?;

        let mut ioreport = Self {
            timebase,
            channels_key,
            subscription,
            subscribed,
            gpu_energy_nj: 0,
            gpu_nj_per_unit: 0,
            gpu: States::new(ACTIVE_STATES_MAX),
            cores: (0..cpu_limit)
                .map(|_| States::new(ACTIVE_STATES_MAX))
                .collect(),
            counts: ChannelCounts::default(),
        };
        ioreport.learn()?;

        Ok(ioreport)
    }

    /// Learns from one sample what each read takes from the channels by index, the GPU's
    /// energy unit and the active states of the GPU and of every core, so a read asks `IOReport`
    /// for no name.
    fn learn(&mut self) -> io::Result<()> {
        let samples = sample(&self.subscription, &self.subscribed)?;
        let channels = channels_of(&samples, &self.channels_key)?;

        let mut counts = ChannelCounts::default();
        for channel in each(channels)? {
            match Kind::of(channel) {
                Some(Kind::GpuEnergy) => {
                    self.gpu_nj_per_unit = nj_per_unit(channel)?;
                    counts.gpu_energy += 1;
                }
                Some(Kind::GpuStates) => {
                    learn_active_states(channel, &mut self.gpu)?;
                    counts.gpu_states += 1;
                }
                Some(Kind::CoreStates) => {
                    learn_active_states(channel, core_at(&mut self.cores, counts.cores)?)?;
                    counts.cores += 1;
                }
                None => {}
            }
        }
        counts.learn_check()?;
        self.counts = counts;

        assert!(self.gpu_nj_per_unit > 0);
        Ok(())
    }

    pub fn read(&mut self) -> io::Result<()> {
        let samples = sample(&self.subscription, &self.subscribed)?;
        let channels = channels_of(&samples, &self.channels_key)?;

        let mut counts = ChannelCounts::default();
        let cores = &mut self.cores[..self.counts.cores];
        for channel in each(channels)? {
            match Kind::of(channel) {
                Some(Kind::GpuEnergy) => {
                    self.gpu_energy_nj = energy_nj(channel, self.gpu_nj_per_unit)?;
                    counts.gpu_energy += 1;
                }
                Some(Kind::GpuStates) => {
                    read_active_states(channel, &mut self.gpu, self.timebase)?;
                    counts.gpu_states += 1;
                }
                Some(Kind::CoreStates) => {
                    let core = core_at(cores, counts.cores)?;
                    read_active_states(channel, core, self.timebase)?;
                    counts.cores += 1;
                }
                None => {}
            }
        }

        counts.read_check(&self.counts)
    }

    /// The performance states of each core open found.
    #[must_use]
    pub fn cores(&self) -> &[States] {
        &self.cores[..self.counts.cores]
    }
}

/// The count of channels of each kind one sample held.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ChannelCounts {
    gpu_energy: usize,
    gpu_states: usize,
    cores: usize,
}

impl ChannelCounts {
    /// Checks that open found what a read takes, one channel of the GPU's energy, one of its
    /// states, and at least one core.
    fn learn_check(&self) -> io::Result<()> {
        if self.gpu_energy != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "IOReport gave {} channels of the GPU's energy",
                    self.gpu_energy
                ),
            ));
        }
        if self.gpu_states != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "IOReport gave {} channels of the GPU's states",
                    self.gpu_states
                ),
            ));
        }
        if self.cores == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "IOReport gave no core's states",
            ));
        }

        Ok(())
    }

    /// Checks that a read found the channels open found, since a channel it missed would keep
    /// the value of the read before.
    fn read_check(&self, at_open: &ChannelCounts) -> io::Result<()> {
        if self != at_open {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("IOReport gave the channels {self:?}, and {at_open:?} at open"),
            ));
        }

        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    GpuEnergy,
    GpuStates,
    CoreStates,
}

impl Kind {
    fn of(channel: CFTypeRef) -> Option<Kind> {
        let group = unsafe { IOReportChannelGetGroup(channel) };
        let subgroup = unsafe { IOReportChannelGetSubGroup(channel) };
        let name = unsafe { IOReportChannelGetChannelName(channel) };

        if is(group, c"Energy Model") && is(name, c"GPU Energy") {
            Some(Kind::GpuEnergy)
        } else if is(group, c"GPU Stats")
            && is(subgroup, c"GPU Performance States")
            && is(name, c"GPUPH")
        {
            Some(Kind::GpuStates)
        } else if is(group, c"CPU Stats") && is(subgroup, c"CPU Core Performance States") {
            Some(Kind::CoreStates)
        } else {
            None
        }
    }
}

/// Takes a sample of every channel `subscribed` holds.
fn sample(subscription: &Owned, subscribed: &Owned) -> io::Result<Owned> {
    let samples = unsafe { IOReportCreateSamples(subscription.0, subscribed.0, ptr::null()) };
    Owned::new(samples, "sampling IOReport")
}

/// The core at `index` among `cores`, which hold one for each CPU the machine runs, so a core
/// past them is an error.
fn core_at(cores: &mut [States], index: usize) -> io::Result<&mut States> {
    let count = cores.len();
    cores.get_mut(index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::QuotaExceeded,
            format!("IOReport reported cores past {count}"),
        )
    })
}

/// Learns which of `channel`'s states count as active into `into`, whose room it does not
/// grow, so a channel with more active states than that is an error.
fn learn_active_states(channel: CFTypeRef, into: &mut States) -> io::Result<()> {
    let capacity = into.active_indices.capacity();
    let state_count = state_count_check(unsafe { IOReportStateGetCount(channel) })?;
    into.active_indices.clear();
    for index in 0..state_count {
        let name = unsafe { IOReportStateGetNameForIndex(channel, index) };
        if !learn_active_states_counts(name)? {
            continue;
        }

        if into.active_indices.len() == capacity {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                format!("an IOReport channel reported active states past {capacity}"),
            ));
        }
        into.active_indices.push(index);
    }
    into.state_count = state_count;

    assert_eq!(capacity, into.active_indices.capacity());
    Ok(())
}

/// Reads the time `channel` spent in each state `into` learned as active. A channel whose
/// states changed since then is an error, since the learned indices name other states.
fn read_active_states(channel: CFTypeRef, into: &mut States, timebase: Timebase) -> io::Result<()> {
    let state_count = unsafe { IOReportStateGetCount(channel) };
    if state_count != into.state_count {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "an IOReport channel reported {state_count} states, and {} at open",
                into.state_count
            ),
        ));
    }

    into.active_ns.clear();
    for &index in &into.active_indices {
        let residency = unsafe { IOReportStateGetResidency(channel, index) };
        let ticks = read_active_states_residency(residency)?;
        push_without_growing(&mut into.active_ns, timebase.nanos(ticks));
    }

    Ok(())
}

/// The ticks of the time in a state `IOReport` gave as `residency`, or an error where it is
/// negative.
fn read_active_states_residency(residency: i64) -> io::Result<u64> {
    let Ok(ticks) = u64::try_from(residency) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("IOReport reported a time of {residency} in a state"),
        ));
    };

    Ok(ticks)
}

/// Whether the state `name` names counts as active, or an error where `IOReport` gave the state
/// no name.
fn learn_active_states_counts(name: CFTypeRef) -> io::Result<bool> {
    if name.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IOReport gave a state no name",
        ));
    }

    for inactive in [c"OFF", c"IDLE", c"DOWN"] {
        if is(name, inactive) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Checks that a channel's count of states is a count, and within the bound on learning them.
fn state_count_check(state_count: i32) -> io::Result<i32> {
    if state_count < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("an IOReport channel reported {state_count} states"),
        ));
    }
    if state_count > STATES_MAX {
        return Err(io::Error::new(
            io::ErrorKind::QuotaExceeded,
            format!("an IOReport channel reported {state_count} states, past {STATES_MAX}"),
        ));
    }

    Ok(state_count)
}

/// The units `IOReport` can give an energy in, each with the nanojoules in one of it.
const ENERGY_UNITS: [(&CStr, u64); 3] = [(c"nJ", 1), (c"uJ", 1_000), (c"mJ", 1_000_000)];

/// The nanojoules in one unit of the energy `channel` gives.
fn nj_per_unit(channel: CFTypeRef) -> io::Result<u64> {
    let unit = unsafe { IOReportChannelGetUnitLabel(channel) };
    for (label, nj_per_unit) in ENERGY_UNITS {
        if is(unit, label) {
            return Ok(nj_per_unit);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "IOReport gave the GPU's energy in an unknown unit",
    ))
}

/// Reads the energy `channel` gives in units of `nj_per_unit` nanojoules, which open learned.
fn energy_nj(channel: CFTypeRef, nj_per_unit: u64) -> io::Result<u64> {
    let value = unsafe { IOReportSimpleGetIntegerValue(channel, 0) };
    energy_nj_of(value, nj_per_unit)
}

/// The nanojoules in `value` units of `nj_per_unit` nanojoules each, or an error where the value
/// is negative or its nanojoules pass what a u64 counts.
fn energy_nj_of(value: i64, nj_per_unit: u64) -> io::Result<u64> {
    assert!(
        ENERGY_UNITS
            .iter()
            .any(|&(_, unit_nj)| unit_nj == nj_per_unit)
    );

    let Ok(units) = u64::try_from(value) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("IOReport gave the GPU's energy as {value} units"),
        ));
    };
    let Some(energy_nj) = units.checked_mul(nj_per_unit) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the GPU's energy of {units} units of {nj_per_unit} nJ passes a u64"),
        ));
    };

    assert!(energy_nj >= units);
    Ok(energy_nj)
}

fn channels_in_group(group: &CStr, subgroup: Option<&CStr>) -> io::Result<Owned> {
    let group = string(group)?;
    let subgroup = subgroup.map(string).transpose()?;
    let channels = unsafe {
        IOReportCopyChannelsInGroup(
            group.0,
            subgroup.as_ref().map_or(ptr::null(), |s| s.0),
            0,
            0,
            0,
        )
    };
    Owned::new(channels, "copying IOReport's channels in a group")
}

fn merge_group(into: &Owned, group: &CStr, subgroup: &CStr) -> io::Result<()> {
    let more = channels_in_group(group, Some(subgroup))?;
    unsafe { IOReportMergeChannels(into.0, more.0, ptr::null()) };

    Ok(())
}

fn keep_read_channels(channels: &Owned, key: &Owned) -> io::Result<()> {
    let all = channels_of(channels, key)?;
    let kept = unsafe { CFArrayCreateMutable(ptr::null(), 0, &raw const kCFTypeArrayCallBacks) };
    let kept = Owned::new(kept, "creating an array")?;

    for channel in each(all)? {
        if Kind::of(channel).is_some() {
            unsafe { CFArrayAppendValue(kept.0, channel) };
        }
    }
    unsafe { CFDictionarySetValue(channels.0, key.0, kept.0) };

    Ok(())
}

fn channels_of(dictionary: &Owned, key: &Owned) -> io::Result<CFTypeRef> {
    let channels = unsafe { CFDictionaryGetValue(dictionary.0, key.0) };
    if channels.is_null() || unsafe { CFGetTypeID(channels) != CFArrayGetTypeID() } {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IOReport gave no list of channels",
        ));
    }

    Ok(channels)
}

/// The most channels walked in one list, past ten times the 383 the groups the collector reads
/// hold on an M5 Pro.
const CHANNELS_MAX: isize = 4096;

/// Each channel in `array`, or an error where its count is not a count within the bound on
/// walking one.
fn each(array: CFTypeRef) -> io::Result<impl Iterator<Item = CFTypeRef>> {
    let channel_count = channel_count_check(unsafe { CFArrayGetCount(array) })?;

    Ok((0..channel_count).map(move |index| unsafe { CFArrayGetValueAtIndex(array, index) }))
}

/// Checks that a list's count of channels is a count, and within the bound on walking one.
fn channel_count_check(channel_count: isize) -> io::Result<isize> {
    if channel_count < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("IOReport gave a list of {channel_count} channels"),
        ));
    }
    if channel_count > CHANNELS_MAX {
        return Err(io::Error::new(
            io::ErrorKind::QuotaExceeded,
            format!("IOReport gave a list of {channel_count} channels, past {CHANNELS_MAX}"),
        ));
    }

    Ok(channel_count)
}

fn string(text: &CStr) -> io::Result<Owned> {
    let created = unsafe { CFStringCreateWithCString(ptr::null(), text.as_ptr(), UTF8) };
    Owned::new(created, "creating a string")
}

fn is(text: CFTypeRef, want: &CStr) -> bool {
    let mut buffer = [0; 64];
    !text.is_null()
        && unsafe { CFStringGetCString(text, buffer.as_mut_ptr(), 64, UTF8) } != 0
        && unsafe { CStr::from_ptr(buffer.as_ptr()) } == want
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::cost::spin_on_this_thread_for;
    use crate::machine;

    fn logical_cpus() -> u32 {
        u32::from_ne_bytes(crate::kernel::sysctl_bytes(c"hw.logicalcpu"))
    }

    fn open() -> IOReport {
        IOReport::open(Timebase::read(), machine::cpu_limit().unwrap()).unwrap()
    }

    /// The first channel of `kind` among the channels `samples` holds, a sample of
    /// `ioreport`'s.
    fn first_channel(ioreport: &IOReport, samples: &Owned, kind: Kind) -> CFTypeRef {
        each(channels_of(samples, &ioreport.channels_key).unwrap())
            .unwrap()
            .find(|&channel| Kind::of(channel) == Some(kind))
            .unwrap()
    }

    fn active_time_of_every_core(ioreport: &IOReport) -> Duration {
        Duration::from_nanos(
            ioreport
                .cores()
                .iter()
                .flat_map(|core| &core.active_ns)
                .sum(),
        )
    }

    #[test]
    fn refuses_to_own_null() {
        // Goal: a CoreFoundation call that returned null is an error, never an object a drop
        // would release.
        // Method: own a null pointer, as a create or copy call that failed returns.
        let owned = Owned::new(ptr::null(), "creating an object");

        assert_eq!(
            Some(io::ErrorKind::Other),
            owned.err().map(|error| error.kind())
        );
    }

    #[test]
    fn converts_the_gpus_energy_into_nanojoules() {
        // Goal: an energy IOReport gives in some unit reads as that many of the unit's
        // nanojoules.
        // Method: convert 2,500 units of a microjoule.
        let energy_nj = energy_nj_of(2_500, 1_000);

        assert_eq!(2_500_000, energy_nj.unwrap());
    }

    #[test]
    fn refuses_a_negative_gpu_energy() {
        // Goal: a negative energy is an error, never a reading of 0.
        // Method: convert i64::MIN, which IOReport of an M5 Pro gave for the value of a channel
        // of states.
        let energy_nj = energy_nj_of(i64::MIN, 1);

        assert_eq!(io::ErrorKind::InvalidData, energy_nj.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_gpu_energy_past_what_its_nanojoules_can_count() {
        // Goal: an energy whose nanojoules pass a u64 is an error, never a wrapped reading.
        // Method: convert i64::MAX units of a millijoule.
        let energy_nj = energy_nj_of(i64::MAX, 1_000_000);

        assert_eq!(io::ErrorKind::InvalidData, energy_nj.unwrap_err().kind());
    }

    #[test]
    fn takes_a_residency_of_no_time() {
        // Goal: a state the channel never entered reads as no time in it.
        // Method: convert a residency of 0.
        let residency = read_active_states_residency(0);

        assert_eq!(0, residency.unwrap());
    }

    #[test]
    fn refuses_a_negative_residency() {
        // Goal: a negative time in a state is an error, never a reading of 0.
        // Method: convert i64::MIN, which IOReport of an M5 Pro gave for the residency of the
        // GPU's energy channel.
        let residency = read_active_states_residency(i64::MIN);

        assert_eq!(io::ErrorKind::InvalidData, residency.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_state_with_no_name() {
        // Goal: a state IOReport gives no name is an error, never a state counted as active.
        // Method: ask the filter about null, the name IOReport of an M5 Pro gave for the index
        // one past a channel's last state.
        let counts = learn_active_states_counts(ptr::null());

        assert_eq!(io::ErrorKind::InvalidData, counts.unwrap_err().kind());
    }

    #[test]
    fn takes_as_many_states_as_its_bound() {
        // Goal: a channel with as many states as the bound passes the check.
        // Method: check a count equal to the bound.
        let state_count = state_count_check(STATES_MAX);

        assert_eq!(STATES_MAX, state_count.unwrap());
    }

    #[test]
    fn refuses_more_states_than_its_bound() {
        // Goal: a channel with more states than the bound is an error, so learning a channel's
        // states has a bound.
        // Method: check a count one past the bound.
        let state_count = state_count_check(STATES_MAX + 1);

        assert_eq!(
            io::ErrorKind::QuotaExceeded,
            state_count.unwrap_err().kind()
        );
    }

    #[test]
    fn refuses_a_negative_state_count() {
        // Goal: a negative count of states is an error, never a channel with no states.
        // Method: check -1, the count IOReport of an M5 Pro gave for the GPU's energy channel.
        let state_count = state_count_check(-1);

        assert_eq!(io::ErrorKind::InvalidData, state_count.unwrap_err().kind());
    }

    #[test]
    fn takes_as_many_channels_as_its_bound() {
        // Goal: a list with as many channels as the bound passes the check.
        // Method: check a count equal to the bound.
        let channel_count = channel_count_check(CHANNELS_MAX);

        assert_eq!(CHANNELS_MAX, channel_count.unwrap());
    }

    #[test]
    fn refuses_more_channels_than_its_bound() {
        // Goal: a list with more channels than the bound is an error, so walking a list of
        // channels has a bound.
        // Method: check a count one past the bound.
        let channel_count = channel_count_check(CHANNELS_MAX + 1);

        assert_eq!(
            io::ErrorKind::QuotaExceeded,
            channel_count.unwrap_err().kind()
        );
    }

    #[test]
    fn refuses_a_negative_channel_count() {
        // Goal: a negative count of channels is an error, never a list with no channels.
        // Method: check a count of -1.
        let channel_count = channel_count_check(-1);

        assert_eq!(
            io::ErrorKind::InvalidData,
            channel_count.unwrap_err().kind()
        );
    }

    /// What a sample of an M5 Pro holds, one channel of the GPU's energy, one of its states,
    /// and 18 cores.
    fn counts_on_an_m5_pro() -> ChannelCounts {
        ChannelCounts {
            gpu_energy: 1,
            gpu_states: 1,
            cores: 18,
        }
    }

    #[test]
    fn learns_from_a_sample_with_the_gpus_energy_its_states_and_a_core() {
        // Goal: a sample with one channel of the GPU's energy, one of its states, and at least
        // one core gives open what a read needs.
        // Method: check what a sample of an M5 Pro holds.
        let checked = counts_on_an_m5_pro().learn_check();

        assert!(checked.is_ok());
    }

    #[test]
    fn refuses_to_learn_from_a_sample_without_the_gpus_energy() {
        // Goal: open without the GPU's energy is an error, never a reading of no energy.
        // Method: check a sample with no channel of the GPU's energy.
        let counts = ChannelCounts {
            gpu_energy: 0,
            ..counts_on_an_m5_pro()
        };

        let checked = counts.learn_check();

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_to_learn_from_a_sample_with_two_channels_of_the_gpus_energy() {
        // Goal: two channels of the GPU's energy are an error, never one read over the other.
        // Method: check a sample with two.
        let counts = ChannelCounts {
            gpu_energy: 2,
            ..counts_on_an_m5_pro()
        };

        let checked = counts.learn_check();

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_to_learn_from_a_sample_without_the_gpus_states() {
        // Goal: open without the GPU's states is an error, never a GPU that is never active.
        // Method: check a sample with no channel of the GPU's states.
        let counts = ChannelCounts {
            gpu_states: 0,
            ..counts_on_an_m5_pro()
        };

        let checked = counts.learn_check();

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_to_learn_from_a_sample_with_two_channels_of_the_gpus_states() {
        // Goal: two channels of the GPU's states are an error, never one read over the other.
        // Method: check a sample with two.
        let counts = ChannelCounts {
            gpu_states: 2,
            ..counts_on_an_m5_pro()
        };

        let checked = counts.learn_check();

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_to_learn_from_a_sample_without_a_core() {
        // Goal: open without a core is an error, never a machine whose cores are never active.
        // Method: check a sample with no channel of a core's states.
        let counts = ChannelCounts {
            cores: 0,
            ..counts_on_an_m5_pro()
        };

        let checked = counts.learn_check();

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn reads_a_sample_with_the_channels_open_found() {
        // Goal: a read that finds the channels open found passes the check.
        // Method: check a sample of an M5 Pro against the same at open.
        let checked = counts_on_an_m5_pro().read_check(&counts_on_an_m5_pro());

        assert!(checked.is_ok());
    }

    #[test]
    fn refuses_to_read_a_sample_with_fewer_cores_than_open_found() {
        // Goal: a read missing a core open found is an error, never a core that read no time.
        // Method: check a sample with one core fewer than at open.
        let counts = ChannelCounts {
            cores: 17,
            ..counts_on_an_m5_pro()
        };

        let checked = counts.read_check(&counts_on_an_m5_pro());

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_to_read_a_sample_without_the_gpus_energy_open_found() {
        // Goal: a read missing the GPU's energy is an error, never the energy of the read
        // before.
        // Method: check a sample with no channel of the GPU's energy against one at open.
        let counts = ChannelCounts {
            gpu_energy: 0,
            ..counts_on_an_m5_pro()
        };

        let checked = counts.read_check(&counts_on_an_m5_pro());

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_to_learn_the_states_of_a_channel_without_states() {
        // Goal: learning the states of a channel that has none is an error, never a channel
        // with no active state.
        // Method: learn the GPU's energy channel, whose state count IOReport gives as -1.
        let ioreport = open();
        let samples = sample(&ioreport.subscription, &ioreport.subscribed).unwrap();
        let energy = first_channel(&ioreport, &samples, Kind::GpuEnergy);
        let mut states = States::new(ACTIVE_STATES_MAX);

        let learned = learn_active_states(energy, &mut states);

        assert_eq!(io::ErrorKind::InvalidData, learned.unwrap_err().kind());
    }

    #[test]
    fn reads_the_performance_states_of_every_logical_cpu() {
        let mut ioreport = open();

        ioreport.read().unwrap();

        assert_eq!(
            usize::try_from(logical_cpus()).unwrap(),
            ioreport.cores().len()
        );
    }

    #[test]
    fn refuses_more_cores_than_its_cpu_limit() {
        // Goal: IOReport reporting more cores than the CPU limit is an error.
        // Method: open with a limit of one CPU on this Mac, which has more.
        let opened = IOReport::open(Timebase::read(), 1);

        let refused = opened.err().map(|error| error.kind());
        assert_eq!(Some(io::ErrorKind::QuotaExceeded), refused);
    }

    #[test]
    fn refuses_a_channel_with_more_active_states_than_its_room() {
        // Goal: a channel with more active states than the room taken for them is an error, and
        // the list keeps that room.
        // Method: learn a core's channel on this Mac, which has many states, into room for one.
        let ioreport = open();
        let samples = sample(&ioreport.subscription, &ioreport.subscribed).unwrap();
        let core = first_channel(&ioreport, &samples, Kind::CoreStates);
        let mut states = States::new(1);
        let capacity = states.active_indices.capacity();

        let learned = learn_active_states(core, &mut states);

        assert_eq!(io::ErrorKind::QuotaExceeded, learned.unwrap_err().kind());
        assert_eq!(capacity, states.active_indices.capacity());
    }

    #[test]
    fn refuses_a_channel_whose_states_changed_since_open() {
        // Goal: a channel that reports another number of states than it did at open is an
        // error, since the active states learned at open no longer name its states.
        // Method: learn the GPU's channel, 16 states on this Mac, and read a core's channel, 17
        // or 22 states, with what was learned.
        let ioreport = open();
        let samples = sample(&ioreport.subscription, &ioreport.subscribed).unwrap();
        let gpu = first_channel(&ioreport, &samples, Kind::GpuStates);
        let core = first_channel(&ioreport, &samples, Kind::CoreStates);
        let mut states = States::new(ACTIVE_STATES_MAX);
        learn_active_states(gpu, &mut states).unwrap();

        let read = read_active_states(core, &mut states, ioreport.timebase);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn counts_a_busy_cpu_in_its_cores_active_time() {
        let mut ioreport = open();
        let busy = Duration::from_millis(200);
        let start = Instant::now();
        ioreport.read().unwrap();
        let before = active_time_of_every_core(&ioreport);

        spin_on_this_thread_for(busy);

        ioreport.read().unwrap();
        let elapsed = start.elapsed();
        let active = active_time_of_every_core(&ioreport)
            .checked_sub(before)
            .expect("the cores' active time only grows");
        assert!(
            active >= busy && active <= elapsed * logical_cpus(),
            "{active:?} active across the cores in {elapsed:?}"
        );
    }

    #[test]
    fn leaves_the_states_a_core_or_the_gpu_spends_idle_out_of_its_active_time() {
        // Goal: counting IDLE, DOWN or OFF as active would read every core and the GPU as
        // busy for the whole of each second.
        // Method: ask the filter about each name these states carry on this Mac.
        assert!(!learn_active_states_counts(string(c"IDLE").unwrap().0).unwrap());
        assert!(!learn_active_states_counts(string(c"DOWN").unwrap().0).unwrap());
        assert!(!learn_active_states_counts(string(c"OFF").unwrap().0).unwrap());
    }

    #[test]
    fn counts_the_performance_states_of_a_core_and_the_gpu_as_active() {
        // Goal: a performance state left out would hide the time spent in it.
        // Method: ask the filter about a core's state name and the GPU's, as this Mac
        // names them.
        assert!(learn_active_states_counts(string(c"V0P14").unwrap().0).unwrap());
        assert!(learn_active_states_counts(string(c"P1").unwrap().0).unwrap());
    }

    #[test]
    fn finds_a_state_left_out_of_active_time_in_every_core_and_the_gpu() {
        // Goal: a macOS that renamed the idle states would read every core as busy all the
        // time, while the two tests above still passed.
        // Method: sample IOReport and look for a state the filter leaves out in each
        // channel of core or GPU states.
        let ioreport = open();
        let samples = sample(&ioreport.subscription, &ioreport.subscribed).unwrap();
        let channels = channels_of(&samples, &ioreport.channels_key).unwrap();

        let states: Vec<CFTypeRef> = each(channels)
            .unwrap()
            .filter(|&channel| {
                matches!(Kind::of(channel), Some(Kind::CoreStates | Kind::GpuStates))
            })
            .collect();
        let without_idle = states
            .iter()
            .filter(|&&channel| {
                (0..unsafe { IOReportStateGetCount(channel) }).all(|index| {
                    learn_active_states_counts(unsafe {
                        IOReportStateGetNameForIndex(channel, index)
                    })
                    .unwrap()
                })
            })
            .count();

        assert_eq!(usize::try_from(logical_cpus()).unwrap() + 1, states.len());
        assert_eq!(0, without_idle);
    }

    #[test]
    fn counts_time_the_gpu_spends_in_active_states() {
        let mut ioreport = open();

        ioreport.read().unwrap();

        assert!(ioreport.gpu.active_ns.iter().sum::<u64>() > 0);
    }

    #[test]
    fn counts_the_energy_the_gpu_spends() {
        let mut ioreport = open();

        ioreport.read().unwrap();

        assert!(ioreport.gpu_energy_nj > 0);
    }
}
