use std::ffi::{CStr, c_char, c_ulong, c_void};
use std::io;
use std::ptr;

use crate::clock::Timebase;
use crate::push_without_growing;

pub struct IOReport {
    timebase: Timebase,
    channels_key: ChannelsKey,
    subscription: Subscription,
    gpu_energy_nj: u64,
    /// The nanojoules in one unit of the GPU's energy, learned at open.
    gpu_nj_per_unit: u64,
    gpu: States,
    /// One for each CPU the machine runs, of which the first `counts.core_states` hold the
    /// cores open found.
    cores: Vec<States>,
    /// The channels of each kind open found, which every read finds again.
    counts: ChannelCounts,
}

#[derive(Debug, Clone)]
pub struct States {
    active_ns: Vec<u64>,
    /// The index of each state the channel counts as active, learned once at open from the
    /// state names, since `IOReport` allocates for each name it gives.
    active_indices: Vec<i32>,
    /// The most active states learned, which both lists have room for.
    active_states_max: usize,
    /// The states the channel reported at open, which `active_indices` index.
    state_count: i32,
}

/// The most active performance states read from one channel, three times the 20 a core of
/// the M5 Pro reports.
const ACTIVE_STATES_MAX: usize = 64;

impl States {
    fn new(active_states_max: usize) -> Self {
        assert!(active_states_max > 0);

        Self {
            active_ns: Vec::with_capacity(active_states_max),
            active_indices: Vec::with_capacity(active_states_max),
            active_states_max,
            state_count: 0,
        }
    }

    /// The time spent in each active state, in nanoseconds, at the last read.
    #[must_use]
    pub fn active_ns(&self) -> &[u64] {
        assert!(self.active_ns.len() <= self.active_states_max);
        // A read pushes one time for each active state open learned, and none before it.
        assert!(self.active_ns.len() <= self.active_indices.len());

        &self.active_ns
    }
}

/// The count of channels of each kind one sample held.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ChannelCounts {
    gpu_energy: usize,
    gpu_states: usize,
    core_states: usize,
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
        if self.core_states == 0 {
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
    GPUEnergy,
    GPUStates,
    CoreStates,
}

impl Kind {
    fn of(channel: CFTypeRef) -> Option<Kind> {
        let name = unsafe { IOReportChannelGetChannelName(channel) };

        if ENERGY_MODEL.holds(channel) {
            if is(name, c"GPU Energy") {
                return Some(Kind::GPUEnergy);
            }
        }
        if GPU_PERFORMANCE_STATES.holds(channel) {
            if is(name, c"GPUPH") {
                return Some(Kind::GPUStates);
            }
        }
        if CPU_CORE_PERFORMANCE_STATES.holds(channel) {
            return Some(Kind::CoreStates);
        }

        None
    }
}

/// A group of `IOReport`'s channels, and the subgroup within it where only one is wanted.
struct Group {
    name: &'static CStr,
    subgroup: Option<&'static CStr>,
}

const ENERGY_MODEL: Group = Group {
    name: c"Energy Model",
    subgroup: None,
};
const GPU_PERFORMANCE_STATES: Group = Group {
    name: c"GPU Stats",
    subgroup: Some(c"GPU Performance States"),
};
const CPU_CORE_PERFORMANCE_STATES: Group = Group {
    name: c"CPU Stats",
    subgroup: Some(c"CPU Core Performance States"),
};

impl Group {
    /// Whether `channel` is in this group, and in its subgroup where it has one.
    fn holds(&self, channel: CFTypeRef) -> bool {
        assert!(!self.name.is_empty());

        if !is(unsafe { IOReportChannelGetGroup(channel) }, self.name) {
            return false;
        }
        let Some(subgroup) = self.subgroup else {
            return true;
        };
        assert!(!subgroup.is_empty());

        is(unsafe { IOReportChannelGetSubGroup(channel) }, subgroup)
    }
}

/// `IOReport`'s subscription to the channels a read takes, and the channels it subscribed to.
struct Subscription {
    handle: Owned,
    channels: Owned,
}

impl Subscription {
    fn new(wanted: &Owned) -> io::Result<Self> {
        assert!(!wanted.0.is_null());

        let mut channels = ptr::null();
        let handle = unsafe {
            IOReportCreateSubscription(ptr::null(), wanted.0, &raw mut channels, 0, ptr::null())
        };
        let handle = Owned::new(handle, "subscribing to IOReport's channels");
        let channels = Owned::new(channels, "listing the channels IOReport subscribed to");

        Ok(Self {
            handle: handle?,
            channels: channels?,
        })
    }

    /// Takes a sample of every channel the subscription holds.
    fn sample(&self) -> io::Result<Owned> {
        assert!(!self.handle.0.is_null());
        assert!(!self.channels.0.is_null());

        let samples = unsafe { IOReportCreateSamples(self.handle.0, self.channels.0, ptr::null()) };
        Owned::new(samples, "sampling IOReport")
    }
}

/// The key under which `IOReport`'s dictionaries hold their list of channels.
struct ChannelsKey(Owned);

impl ChannelsKey {
    fn new() -> io::Result<Self> {
        Ok(Self(string(c"IOReportChannels")?))
    }

    fn channels_of(&self, dictionary: &Owned) -> io::Result<CFTypeRef> {
        assert!(!self.0.0.is_null());
        assert!(!dictionary.0.is_null());

        let channels = unsafe { CFDictionaryGetValue(dictionary.0, self.0.0) };
        if channels.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "IOReport gave no list of channels",
            ));
        }
        if unsafe { CFGetTypeID(channels) } != unsafe { CFArrayGetTypeID() } {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "IOReport gave a list of channels that is not an array",
            ));
        }

        Ok(channels)
    }
}

/// A CoreFoundation object a create or copy call returned, which is never null and which only
/// this owner releases.
struct Owned(CFTypeRef);

impl Owned {
    /// Owns `object`, which the call `call` names returned, or reports the call where it
    /// returned null.
    fn new(object: CFTypeRef, call: &str) -> io::Result<Self> {
        assert!(!call.is_empty());

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
    fn CFGetTypeID(object: CFTypeRef) -> c_ulong;
    fn CFArrayGetTypeID() -> c_ulong;
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

// Apple documents none of IOReport, so the meaning of the last three arguments of
// IOReportCopyChannelsInGroup is unknown, and the collector passes 0 for each.
#[link(name = "IOReport")]
unsafe extern "C" {
    fn IOReportCopyChannelsInGroup(
        group: CFTypeRef,
        subgroup: CFTypeRef,
        unknown_1: u64,
        unknown_2: u64,
        unknown_3: u64,
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

impl IOReport {
    pub fn open(timebase: Timebase, cpu_limit: usize) -> io::Result<Self> {
        assert!(cpu_limit > 0);

        let channels_key = ChannelsKey::new()?;

        let wanted = channels_in_group(&ENERGY_MODEL)?;
        open_merge_group(&wanted, &GPU_PERFORMANCE_STATES)?;
        open_merge_group(&wanted, &CPU_CORE_PERFORMANCE_STATES)?;
        open_keep_read_channels(&wanted, &channels_key)?;

        let subscription = Subscription::new(&wanted)?;

        let mut ioreport = Self {
            timebase,
            channels_key,
            subscription,
            gpu_energy_nj: 0,
            gpu_nj_per_unit: 0,
            gpu: States::new(ACTIVE_STATES_MAX),
            cores: (0..cpu_limit)
                .map(|_| States::new(ACTIVE_STATES_MAX))
                .collect(),
            counts: ChannelCounts::default(),
        };
        ioreport.learn()?;

        assert!(ioreport.counts.core_states > 0);
        Ok(ioreport)
    }

    /// Learns from one sample the GPU's energy unit and which states of the GPU and of every
    /// core are active, so a read finds each active state by its index and asks `IOReport` for
    /// no state's name.
    fn learn(&mut self) -> io::Result<()> {
        assert_eq!(ChannelCounts::default(), self.counts);

        let samples = self.subscription.sample()?;
        let channels = self.channels_key.channels_of(&samples)?;

        let mut counts = ChannelCounts::default();
        for channel in each(channels)? {
            match Kind::of(channel) {
                Some(Kind::GPUEnergy) => {
                    self.gpu_nj_per_unit = learn_nj_per_unit(channel)?;
                    counts.gpu_energy += 1;
                }
                Some(Kind::GPUStates) => {
                    learn_active_states(channel, &mut self.gpu)?;
                    counts.gpu_states += 1;
                }
                Some(Kind::CoreStates) => {
                    learn_active_states(channel, core_at(&mut self.cores, counts.core_states)?)?;
                    counts.core_states += 1;
                }
                None => {}
            }
        }
        counts.learn_check()?;
        self.counts = counts;

        assert!(self.gpu_nj_per_unit > 0);
        assert!(self.counts.core_states <= self.cores.len());
        Ok(())
    }

    pub fn read(&mut self) -> io::Result<()> {
        assert!(self.gpu_nj_per_unit > 0);
        assert_eq!(1, self.counts.gpu_energy);
        assert_eq!(1, self.counts.gpu_states);
        assert!(self.counts.core_states > 0);
        assert!(self.counts.core_states <= self.cores.len());

        let samples = self.subscription.sample()?;
        let channels = self.channels_key.channels_of(&samples)?;

        let mut counts = ChannelCounts::default();
        for channel in each(channels)? {
            match Kind::of(channel) {
                Some(Kind::GPUEnergy) => {
                    self.gpu_energy_nj = read_energy_nj(channel, self.gpu_nj_per_unit)?;
                    counts.gpu_energy += 1;
                }
                Some(Kind::GPUStates) => {
                    read_active_states(self.timebase, channel, &mut self.gpu)?;
                    counts.gpu_states += 1;
                }
                Some(Kind::CoreStates) => {
                    let core = core_at(
                        &mut self.cores[..self.counts.core_states],
                        counts.core_states,
                    )?;
                    read_active_states(self.timebase, channel, core)?;
                    counts.core_states += 1;
                }
                None => {}
            }
        }
        counts.read_check(&self.counts)?;

        assert_eq!(self.gpu.active_indices.len(), self.gpu.active_ns.len());
        Ok(())
    }

    /// The energy the GPU had spent at the last read, in nanojoules.
    #[must_use]
    pub fn gpu_energy_nj(&self) -> u64 {
        assert!(self.gpu_nj_per_unit > 0);
        // A read counts whole units of the energy, and none before it.
        assert!(self.gpu_energy_nj.is_multiple_of(self.gpu_nj_per_unit));

        self.gpu_energy_nj
    }

    /// The GPU's performance states.
    #[must_use]
    pub fn gpu(&self) -> &States {
        assert_eq!(1, self.counts.gpu_states);
        assert!(self.gpu.active_ns.len() <= self.gpu.active_indices.len());

        &self.gpu
    }

    /// The performance states of each core open found.
    #[must_use]
    pub fn cores(&self) -> &[States] {
        assert!(self.counts.core_states > 0);
        assert!(self.counts.core_states <= self.cores.len());

        &self.cores[..self.counts.core_states]
    }
}

fn channels_in_group(group: &Group) -> io::Result<Owned> {
    assert!(!group.name.is_empty());
    if let Some(subgroup) = group.subgroup {
        assert!(!subgroup.is_empty());
    }

    let name = string(group.name)?;
    let subgroup = group.subgroup.map(string).transpose()?;

    let channels = unsafe {
        IOReportCopyChannelsInGroup(
            name.0,
            subgroup.as_ref().map_or(ptr::null(), |subgroup| subgroup.0),
            0,
            0,
            0,
        )
    };
    Owned::new(channels, "copying IOReport's channels in a group")
}

fn open_merge_group(into: &Owned, group: &Group) -> io::Result<()> {
    assert!(!into.0.is_null());

    let more = channels_in_group(group)?;

    unsafe { IOReportMergeChannels(into.0, more.0, ptr::null()) };

    Ok(())
}

/// Keeps in `channels` only those a read takes, so each sample copies the 20 channels an M5 Pro
/// reads rather than the 383 the groups hold.
fn open_keep_read_channels(channels: &Owned, key: &ChannelsKey) -> io::Result<()> {
    assert!(!channels.0.is_null());

    let all = key.channels_of(channels)?;

    let kept = unsafe { CFArrayCreateMutable(ptr::null(), 0, &raw const kCFTypeArrayCallBacks) };
    let kept = Owned::new(kept, "creating an array")?;

    for channel in each(all)? {
        if Kind::of(channel).is_some() {
            unsafe { CFArrayAppendValue(kept.0, channel) };
        }
    }
    unsafe { CFDictionarySetValue(channels.0, key.0.0, kept.0) };

    Ok(())
}

/// The most channels walked in one list, past ten times the 383 the groups the collector reads
/// hold on an M5 Pro.
const CHANNELS_MAX: isize = 4096;

/// Each channel in `array`, or an error where its count is not a count within the bound on
/// walking one.
fn each(array: CFTypeRef) -> io::Result<impl Iterator<Item = CFTypeRef>> {
    assert!(!array.is_null());

    let channel_count = unsafe { CFArrayGetCount(array) };
    each_channel_count_check(channel_count)?;

    Ok((0..channel_count).map(move |index| unsafe { CFArrayGetValueAtIndex(array, index) }))
}

/// Checks that a list's count of channels is a count, and within the bound on walking one.
fn each_channel_count_check(channel_count: isize) -> io::Result<()> {
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

    Ok(())
}

/// The core at `index` among `cores`, or an error where `IOReport` reports more cores than
/// `cores` holds, which is one for each CPU the machine runs at open and the cores open found
/// at a read.
fn core_at(cores: &mut [States], index: usize) -> io::Result<&mut States> {
    let count = cores.len();
    // Open holds a core for each CPU, and a read the cores open found, one at least.
    assert!(count > 0);
    assert!(index <= count);

    cores.get_mut(index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::QuotaExceeded,
            format!("IOReport reported more cores than {count}"),
        )
    })
}

/// Learns which of `channel`'s states count as active into `into`, whose room it does not
/// grow, so a channel with more active states than that is an error.
fn learn_active_states(channel: CFTypeRef, into: &mut States) -> io::Result<()> {
    assert!(into.active_states_max <= into.active_indices.capacity());

    let state_count = unsafe { IOReportStateGetCount(channel) };
    learn_active_states_count_check(state_count)?;

    into.active_indices.clear();
    for index in 0..state_count {
        let name = unsafe { IOReportStateGetNameForIndex(channel, index) };
        if !learn_active_states_is_active(name)? {
            continue;
        }

        if into.active_indices.len() == into.active_states_max {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                format!(
                    "an IOReport channel reported active states past {}",
                    into.active_states_max
                ),
            ));
        }

        push_without_growing(&mut into.active_indices, index);
    }
    into.state_count = state_count;
    Ok(())
}

/// Whether the state `name` names counts as active, or an error where `IOReport` gave the state
/// no name.
fn learn_active_states_is_active(name: CFTypeRef) -> io::Result<bool> {
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

/// The most states learned from one channel, past ten times the 22 a core of the M5 Pro
/// reports.
const STATES_MAX: i32 = 256;

/// Checks that a channel's count of states is a count, and within the bound on learning them.
fn learn_active_states_count_check(state_count: i32) -> io::Result<()> {
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

    Ok(())
}

/// Reads the time `channel` spent in each state `into` learned as active. A channel whose
/// states changed since then is an error, since the learned indices name other states.
fn read_active_states(timebase: Timebase, channel: CFTypeRef, into: &mut States) -> io::Result<()> {
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

    // Learning bounded the states and the active ones among them.
    assert!(into.state_count <= STATES_MAX);
    assert!(into.active_indices.len() <= into.active_states_max);
    into.active_ns.clear();
    for &index in &into.active_indices {
        assert!(index < into.state_count);
        let residency = unsafe { IOReportStateGetResidency(channel, index) };
        let ticks = read_active_states_residency(residency)?;
        push_without_growing(&mut into.active_ns, timebase.nanoseconds(ticks)?);
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

/// The units `IOReport` can give an energy in, each with the nanojoules in one of it.
const ENERGY_UNITS: [(&CStr, u64); 3] = [(c"nJ", 1), (c"uJ", 1_000), (c"mJ", 1_000_000)];

const _: () = assert!(ENERGY_UNITS[0].1 == 1);
const _: () = assert!(ENERGY_UNITS[1].1 == ENERGY_UNITS[0].1 * 1_000);
const _: () = assert!(ENERGY_UNITS[2].1 == ENERGY_UNITS[1].1 * 1_000);

/// The nanojoules in one unit of the energy `channel` gives.
fn learn_nj_per_unit(channel: CFTypeRef) -> io::Result<u64> {
    let nj_per_unit = learn_nj_per_unit_of(unsafe { IOReportChannelGetUnitLabel(channel) })?;

    assert!(nj_per_unit >= ENERGY_UNITS[0].1);
    assert!(nj_per_unit <= ENERGY_UNITS[2].1);
    Ok(nj_per_unit)
}

/// The nanojoules in one of the unit `unit` labels, or an error where it labels no unit of
/// `ENERGY_UNITS`.
fn learn_nj_per_unit_of(unit: CFTypeRef) -> io::Result<u64> {
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
fn read_energy_nj(channel: CFTypeRef, nj_per_unit: u64) -> io::Result<u64> {
    assert!(nj_per_unit > 0);

    let value = unsafe { IOReportSimpleGetIntegerValue(channel, 0) };
    let energy_nj = read_energy_nj_of(value, nj_per_unit)?;

    assert!(energy_nj.is_multiple_of(nj_per_unit));
    Ok(energy_nj)
}

/// The nanojoules in `value` units of `nj_per_unit` nanojoules each, or an error where the value
/// is negative or its nanojoules pass what a u64 counts.
fn read_energy_nj_of(value: i64, nj_per_unit: u64) -> io::Result<u64> {
    assert!(
        ENERGY_UNITS
            .iter()
            .any(|&(_, nj_per_unit_listed)| nj_per_unit_listed == nj_per_unit)
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

    Ok(energy_nj)
}

fn string(text: &CStr) -> io::Result<Owned> {
    assert!(!text.is_empty());

    let created = unsafe { CFStringCreateWithCString(ptr::null(), text.as_ptr(), UTF8) };
    Owned::new(created, "creating a string")
}

/// Whether `text` is a string that holds `want`. A string the buffer cannot hold is longer than
/// `want`, which fits it, so it is not `want`.
fn is(text: CFTypeRef, want: &CStr) -> bool {
    let mut buffer = [0_u8; 64];
    assert!(!want.is_empty());
    assert!(want.count_bytes() < buffer.len());

    if text.is_null() {
        return false;
    }

    let size = buffer.len().cast_signed();
    let converted = unsafe { CFStringGetCString(text, buffer.as_mut_ptr().cast(), size, UTF8) };
    if converted == 0 {
        return false;
    }

    CStr::from_bytes_until_nul(&buffer).is_ok_and(|copied| copied == want)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::cost::spin_on_this_thread_for;
    use crate::machine;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRetain(object: CFTypeRef) -> CFTypeRef;
        fn CFGetRetainCount(object: CFTypeRef) -> isize;
        fn CFDictionaryCreateMutable(
            allocator: CFTypeRef,
            capacity: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFTypeRef;
    }

    fn logical_cpus() -> u32 {
        u32::from_ne_bytes(crate::kernel::sysctl_bytes(c"hw.logicalcpu"))
    }

    fn open() -> IOReport {
        IOReport::open(Timebase::read().unwrap(), machine::cpu_limit().unwrap()).unwrap()
    }

    /// The first channel of `kind` among the channels `samples` holds, a sample of
    /// `ioreport`'s.
    fn first_channel(ioreport: &IOReport, samples: &Owned, kind: Kind) -> CFTypeRef {
        each(ioreport.channels_key.channels_of(samples).unwrap())
            .unwrap()
            .find(|&channel| Kind::of(channel) == Some(kind))
            .unwrap()
    }

    /// A new, empty dictionary, which with no callbacks compares its keys as pointers and
    /// holds its values without retaining them.
    fn empty_dictionary() -> Owned {
        let dictionary =
            unsafe { CFDictionaryCreateMutable(ptr::null(), 0, ptr::null(), ptr::null()) };
        Owned::new(dictionary, "creating a dictionary").unwrap()
    }

    fn active_time_of_every_core(ioreport: &IOReport) -> Duration {
        Duration::from_nanos(ioreport.cores().iter().flat_map(States::active_ns).sum())
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
    fn releases_the_object_it_owns() {
        // Goal: dropping an owner releases its object once, so no object leaks.
        // Method: take a second reference to a new array, drop its owner, and count the
        // references left.
        let array =
            unsafe { CFArrayCreateMutable(ptr::null(), 0, &raw const kCFTypeArrayCallBacks) };
        let owned = Owned::new(array, "creating an array").unwrap();
        unsafe { CFRetain(array) };

        drop(owned);

        let references = unsafe { CFGetRetainCount(array) };
        unsafe { CFRelease(array) };
        assert_eq!(1, references);
    }

    #[test]
    fn takes_an_energy_in_millijoules() {
        // Goal: an energy IOReport gives in millijoules reads as a million nanojoules a unit.
        // Method: ask for the nanojoules in the unit IOReport labels "mJ".
        let nj_per_unit = learn_nj_per_unit_of(string(c"mJ").unwrap().0);

        assert_eq!(1_000_000, nj_per_unit.unwrap());
    }

    #[test]
    fn refuses_an_energy_in_an_unknown_unit() {
        // Goal: an energy in a unit the collector does not know is an error, never a reading
        // in another unit.
        // Method: ask for the nanojoules in a unit labelled "kJ".
        let nj_per_unit = learn_nj_per_unit_of(string(c"kJ").unwrap().0);

        assert_eq!(io::ErrorKind::InvalidData, nj_per_unit.unwrap_err().kind());
    }

    #[test]
    fn refuses_an_energy_with_no_unit() {
        // Goal: an energy IOReport gives no unit for is an error.
        // Method: ask for the nanojoules in a null unit.
        let nj_per_unit = learn_nj_per_unit_of(ptr::null());

        assert_eq!(io::ErrorKind::InvalidData, nj_per_unit.unwrap_err().kind());
    }

    #[test]
    fn converts_the_gpus_energy_into_nanojoules() {
        // Goal: an energy IOReport gives in some unit reads as that many of the unit's
        // nanojoules.
        // Method: convert 2,500 units of a microjoule.
        let energy_nj = read_energy_nj_of(2_500, 1_000);

        assert_eq!(2_500_000, energy_nj.unwrap());
    }

    #[test]
    fn refuses_a_negative_gpu_energy() {
        // Goal: a negative energy is an error, never a reading of 0.
        // Method: convert i64::MIN, which IOReport of an M5 Pro gave for the value of a channel
        // of states.
        let energy_nj = read_energy_nj_of(i64::MIN, 1);

        assert_eq!(io::ErrorKind::InvalidData, energy_nj.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_gpu_energy_past_what_its_nanojoules_can_count() {
        // Goal: an energy whose nanojoules pass a u64 is an error, never a wrapped reading.
        // Method: convert i64::MAX units of a millijoule.
        let energy_nj = read_energy_nj_of(i64::MAX, 1_000_000);

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
        let is_active = learn_active_states_is_active(ptr::null());

        assert_eq!(io::ErrorKind::InvalidData, is_active.unwrap_err().kind());
    }

    #[test]
    fn takes_as_many_states_as_its_bound() {
        // Goal: a channel with as many states as the bound passes the check.
        // Method: check a count equal to the bound.
        let checked = learn_active_states_count_check(STATES_MAX);

        assert!(checked.is_ok());
    }

    #[test]
    fn takes_a_channel_with_no_states() {
        // Goal: a channel with no states passes the check, and has no active state to learn.
        // Method: check a count of 0.
        let checked = learn_active_states_count_check(0);

        assert!(checked.is_ok());
    }

    #[test]
    fn refuses_more_states_than_its_bound() {
        // Goal: a channel with more states than the bound is an error, so learning a channel's
        // states has a bound.
        // Method: check a count one past the bound.
        let checked = learn_active_states_count_check(STATES_MAX + 1);

        assert_eq!(io::ErrorKind::QuotaExceeded, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_negative_state_count() {
        // Goal: a negative count of states is an error, never a channel with no states.
        // Method: check -1, the count IOReport of an M5 Pro gave for the GPU's energy channel.
        let checked = learn_active_states_count_check(-1);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_dictionary_with_no_list_of_channels() {
        // Goal: a dictionary IOReport gives without a list of channels is an error, never an
        // empty list.
        // Method: look for the list in an empty dictionary.
        let key = ChannelsKey::new().unwrap();
        let dictionary = empty_dictionary();

        let channels = key.channels_of(&dictionary);

        assert_eq!(io::ErrorKind::InvalidData, channels.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_list_of_channels_that_is_not_an_array() {
        // Goal: a list of channels of another type than an array is an error, never walked as
        // an array.
        // Method: put a string where the list goes in a dictionary.
        let key = ChannelsKey::new().unwrap();
        let dictionary = empty_dictionary();
        let text = string(c"not an array").unwrap();
        unsafe { CFDictionarySetValue(dictionary.0, key.0.0, text.0) };

        let channels = key.channels_of(&dictionary);

        assert_eq!(io::ErrorKind::InvalidData, channels.unwrap_err().kind());
    }

    #[test]
    fn takes_as_many_channels_as_its_bound() {
        // Goal: a list with as many channels as the bound passes the check.
        // Method: check a count equal to the bound.
        let checked = each_channel_count_check(CHANNELS_MAX);

        assert!(checked.is_ok());
    }

    #[test]
    fn takes_a_list_of_no_channels() {
        // Goal: an empty list of channels passes the check, which leaves to open whether the
        // channels it found are enough.
        // Method: check a count of 0.
        let checked = each_channel_count_check(0);

        assert!(checked.is_ok());
    }

    #[test]
    fn refuses_more_channels_than_its_bound() {
        // Goal: a list with more channels than the bound is an error, so walking a list of
        // channels has a bound.
        // Method: check a count one past the bound.
        let checked = each_channel_count_check(CHANNELS_MAX + 1);

        assert_eq!(io::ErrorKind::QuotaExceeded, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_negative_channel_count() {
        // Goal: a negative count of channels is an error, never a list with no channels.
        // Method: check a count of -1.
        let checked = each_channel_count_check(-1);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    /// What a sample of an M5 Pro holds, one channel of the GPU's energy, one of its states,
    /// and 18 cores.
    fn counts_on_an_m5_pro() -> ChannelCounts {
        ChannelCounts {
            gpu_energy: 1,
            gpu_states: 1,
            core_states: 18,
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
            core_states: 0,
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
            core_states: 17,
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
    fn refuses_to_learn_a_channel_with_a_negative_state_count() {
        // Goal: a channel whose count of states is negative, as IOReport gives for a channel
        // that is not one of states, is an error, never a channel with no active state.
        // Method: learn the GPU's energy channel, whose state count IOReport gives as -1.
        let ioreport = open();
        let samples = ioreport.subscription.sample().unwrap();
        let energy = first_channel(&ioreport, &samples, Kind::GPUEnergy);
        let mut states = States::new(ACTIVE_STATES_MAX);

        let learned = learn_active_states(energy, &mut states);

        assert_eq!(io::ErrorKind::InvalidData, learned.unwrap_err().kind());
    }

    #[test]
    fn reads_the_performance_states_of_every_logical_cpu() {
        // Goal: every logical CPU has its performance states read, so no CPU's active time is
        // missing.
        // Method: read IOReport once on this Mac, and count the cores read against its
        // logical CPUs.
        let mut ioreport = open();

        ioreport.read().unwrap();

        assert_eq!(
            usize::try_from(logical_cpus()).unwrap(),
            ioreport.cores().len()
        );
    }

    #[test]
    fn subscribes_only_to_the_channels_a_read_takes() {
        // Goal: the subscription holds only the channels a read takes, so a sample copies no
        // channel the read throws away.
        // Method: open IOReport on this Mac, and count the channels it subscribed to against
        // those open found.
        let ioreport = open();

        let subscribed = each(
            ioreport
                .channels_key
                .channels_of(&ioreport.subscription.channels)
                .unwrap(),
        )
        .unwrap()
        .count();

        let counts = ioreport.counts;
        assert_eq!(
            counts.gpu_energy + counts.gpu_states + counts.core_states,
            subscribed
        );
    }

    #[test]
    fn refuses_more_cores_than_its_cpu_limit() {
        // Goal: IOReport reporting more cores than the CPU limit is an error.
        // Method: open with a limit of one CPU on this Mac, which has more.
        let opened = IOReport::open(Timebase::read().unwrap(), 1);

        let refused = opened.err().map(|error| error.kind());
        assert_eq!(Some(io::ErrorKind::QuotaExceeded), refused);
    }

    #[test]
    fn refuses_a_channel_with_more_active_states_than_its_room() {
        // Goal: a channel with more active states than the room taken for them is an error, and
        // the list keeps that room.
        // Method: learn a core's channel on this Mac, which has many states, into room for one.
        let ioreport = open();
        let samples = ioreport.subscription.sample().unwrap();
        let core = first_channel(&ioreport, &samples, Kind::CoreStates);
        let mut states = States::new(1);
        let capacity = states.active_indices.capacity();

        let learned = learn_active_states(core, &mut states);

        assert_eq!(io::ErrorKind::QuotaExceeded, learned.unwrap_err().kind());
        assert_eq!(capacity, states.active_indices.capacity());
    }

    #[test]
    fn learns_only_the_active_states_of_a_core() {
        // Goal: the states learned as active are those the filter counts as active, so a read
        // adds no idle time and leaves out no active time.
        // Method: learn a core's channel on this Mac, against the filter asked about each of
        // its states.
        let ioreport = open();
        let samples = ioreport.subscription.sample().unwrap();
        let core = first_channel(&ioreport, &samples, Kind::CoreStates);
        let mut states = States::new(ACTIVE_STATES_MAX);

        learn_active_states(core, &mut states).unwrap();

        let active: Vec<i32> = (0..states.state_count)
            .filter(|&index| {
                learn_active_states_is_active(unsafe { IOReportStateGetNameForIndex(core, index) })
                    .unwrap()
            })
            .collect();
        assert_eq!(active, states.active_indices);
    }

    #[test]
    fn refuses_a_channel_whose_states_changed_since_open() {
        // Goal: a channel that reports another number of states than it did at open is an
        // error, since the active states learned at open no longer name its states.
        // Method: learn the GPU's channel, 16 states on this Mac, and read a core's channel, 17
        // or 22 states, with what was learned.
        let ioreport = open();
        let samples = ioreport.subscription.sample().unwrap();
        let gpu = first_channel(&ioreport, &samples, Kind::GPUStates);
        let core = first_channel(&ioreport, &samples, Kind::CoreStates);
        let mut states = States::new(ACTIVE_STATES_MAX);
        learn_active_states(gpu, &mut states).unwrap();

        let read = read_active_states(ioreport.timebase, core, &mut states);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_read_that_finds_other_channels_than_open() {
        // Goal: a read that finds another count of channels than open found is an error, so a
        // channel missing from a sample never leaves the read before standing.
        // Method: open with room for one core more than this Mac has, record that open found
        // that core too, and read.
        let cpu_limit = machine::cpu_limit().unwrap();
        let mut ioreport = IOReport::open(Timebase::read().unwrap(), cpu_limit + 1).unwrap();
        ioreport.counts.core_states += 1;

        let read = ioreport.read();

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_the_energy_unit_of_a_channel_that_gives_no_energy() {
        // Goal: a channel whose unit is not one of energy is an error where open learns the
        // GPU's energy unit, never a unit of nanojoules.
        // Method: learn the unit of the GPU's states channel, 24Mticks on this Mac.
        let ioreport = open();
        let samples = ioreport.subscription.sample().unwrap();
        let gpu = first_channel(&ioreport, &samples, Kind::GPUStates);

        let nj_per_unit = learn_nj_per_unit(gpu);

        assert_eq!(io::ErrorKind::InvalidData, nj_per_unit.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_negative_energy_read_from_a_channel() {
        // Goal: a channel's value below zero is an error where a read takes it as energy, never
        // 0 nJ.
        // Method: read the GPU's states channel as energy in nanojoules, whose value on this
        // Mac is the least an i64 holds.
        let ioreport = open();
        let samples = ioreport.subscription.sample().unwrap();
        let gpu = first_channel(&ioreport, &samples, Kind::GPUStates);

        let energy = read_energy_nj(gpu, 1);

        assert_eq!(io::ErrorKind::InvalidData, energy.unwrap_err().kind());
    }

    #[test]
    fn counts_a_busy_cpu_in_its_cores_active_time() {
        // Goal: time a CPU spends running shows in the cores' active time, and never more of it
        // than every CPU could spend in that time.
        // Method: spin this thread for 200 ms between two reads, and bound the active time the
        // cores gained below by the spin and above by the time passed on every logical CPU.
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
            active >= busy,
            "{active:?} active across the cores in {elapsed:?}"
        );
        assert!(
            active <= elapsed * logical_cpus(),
            "{active:?} active across the cores in {elapsed:?}"
        );
    }

    #[test]
    fn leaves_the_idle_state_out_of_active_time() {
        // Goal: counting IDLE as active would read every core and the GPU as busy for the
        // whole of each second.
        // Method: ask the filter about the name IDLE, as this Mac names the state.
        assert!(!learn_active_states_is_active(string(c"IDLE").unwrap().0).unwrap());
    }

    #[test]
    fn leaves_the_down_state_out_of_active_time() {
        // Goal: counting DOWN as active would read every core and the GPU as busy for the
        // whole of each second.
        // Method: ask the filter about the name DOWN, as this Mac names the state.
        assert!(!learn_active_states_is_active(string(c"DOWN").unwrap().0).unwrap());
    }

    #[test]
    fn leaves_the_off_state_out_of_active_time() {
        // Goal: counting OFF as active would read every core and the GPU as busy for the
        // whole of each second.
        // Method: ask the filter about the name OFF, as this Mac names the state.
        assert!(!learn_active_states_is_active(string(c"OFF").unwrap().0).unwrap());
    }

    #[test]
    fn counts_a_performance_state_of_a_core_as_active() {
        // Goal: a core's performance state left out would hide the time spent in it.
        // Method: ask the filter about V0P14, as this Mac names a core's state.
        assert!(learn_active_states_is_active(string(c"V0P14").unwrap().0).unwrap());
    }

    #[test]
    fn counts_a_performance_state_of_the_gpu_as_active() {
        // Goal: a GPU performance state left out would hide the time spent in it.
        // Method: ask the filter about P1, as this Mac names a GPU state.
        assert!(learn_active_states_is_active(string(c"P1").unwrap().0).unwrap());
    }

    #[test]
    fn finds_a_state_left_out_of_active_time_in_every_core_and_the_gpu() {
        // Goal: a macOS that renamed the idle states would read every core as busy all the
        // time, while the tests above on the state names still passed.
        // Method: sample IOReport and look for a state the filter leaves out in each
        // channel of core or GPU states.
        let ioreport = open();
        let samples = ioreport.subscription.sample().unwrap();
        let channels = ioreport.channels_key.channels_of(&samples).unwrap();

        let states: Vec<CFTypeRef> = each(channels)
            .unwrap()
            .filter(|&channel| {
                matches!(Kind::of(channel), Some(Kind::CoreStates | Kind::GPUStates))
            })
            .collect();
        let without_idle = states
            .iter()
            .filter(|&&channel| {
                (0..unsafe { IOReportStateGetCount(channel) }).all(|index| {
                    learn_active_states_is_active(unsafe {
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
        // Goal: the GPU's time in its active states reads as more than none.
        // Method: read IOReport once on this Mac, whose GPU has run since it started.
        let mut ioreport = open();

        ioreport.read().unwrap();

        assert!(ioreport.gpu().active_ns().iter().sum::<u64>() > 0);
    }

    #[test]
    fn reads_the_gpus_energy_as_its_counter_holds_it() {
        // Goal: each read takes the GPU's energy from its counter, which only grows.
        // Method: sample the GPU's energy channel on its own between two reads, and hold what
        // it gave between what the reads gave.
        let mut ioreport = open();

        ioreport.read().unwrap();
        let before = ioreport.gpu_energy_nj();
        let samples = ioreport.subscription.sample().unwrap();
        let energy = first_channel(&ioreport, &samples, Kind::GPUEnergy);
        let units = u64::try_from(unsafe { IOReportSimpleGetIntegerValue(energy, 0) }).unwrap();
        ioreport.read().unwrap();
        let after = ioreport.gpu_energy_nj();

        let between = units * ioreport.gpu_nj_per_unit;
        assert!(before <= between, "{before} nJ, then {between} nJ");
        assert!(between <= after, "{between} nJ, then {after} nJ");
    }

    #[test]
    fn counts_the_energy_the_gpu_spends() {
        // Goal: the GPU's energy reads as more than none.
        // Method: read IOReport once on this Mac, whose GPU has run since it started.
        let mut ioreport = open();

        ioreport.read().unwrap();

        assert!(ioreport.gpu_energy_nj() > 0);
    }
}
