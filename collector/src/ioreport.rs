use std::ffi::{CStr, c_char, c_void};
use std::io;
use std::ptr;

use crate::clock::Timebase;

type CfRef = *const c_void;

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
    fn CFRelease(object: CfRef);
    fn CFGetTypeID(object: CfRef) -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: CfRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CfRef, index: isize) -> CfRef;
    fn CFArrayCreateMutable(
        allocator: CfRef,
        capacity: isize,
        callbacks: *const ArrayCallBacks,
    ) -> CfRef;
    fn CFArrayAppendValue(array: CfRef, value: CfRef);
    fn CFDictionaryGetValue(dictionary: CfRef, key: CfRef) -> CfRef;
    fn CFDictionarySetValue(dictionary: CfRef, key: CfRef, value: CfRef);
    fn CFStringCreateWithCString(allocator: CfRef, text: *const c_char, encoding: u32) -> CfRef;
    fn CFStringGetCString(text: CfRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
}

#[link(name = "IOReport")]
unsafe extern "C" {
    fn IOReportCopyChannelsInGroup(group: CfRef, subgroup: CfRef, a: u64, b: u64, c: u64) -> CfRef;
    fn IOReportMergeChannels(into: CfRef, from: CfRef, unused: CfRef);
    fn IOReportCreateSubscription(
        unused: CfRef,
        desired: CfRef,
        subscribed: *mut CfRef,
        channel_id: u64,
        options: CfRef,
    ) -> CfRef;
    fn IOReportCreateSamples(subscription: CfRef, subscribed: CfRef, options: CfRef) -> CfRef;
    fn IOReportChannelGetGroup(channel: CfRef) -> CfRef;
    fn IOReportChannelGetSubGroup(channel: CfRef) -> CfRef;
    fn IOReportChannelGetChannelName(channel: CfRef) -> CfRef;
    fn IOReportChannelGetUnitLabel(channel: CfRef) -> CfRef;
    fn IOReportSimpleGetIntegerValue(channel: CfRef, index: i32) -> i64;
    fn IOReportStateGetCount(channel: CfRef) -> i32;
    fn IOReportStateGetNameForIndex(channel: CfRef, index: i32) -> CfRef;
    fn IOReportStateGetResidency(channel: CfRef, index: i32) -> i64;
}

struct Owned(CfRef);

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

#[derive(Debug, Clone)]
pub struct States {
    pub active_ns: Vec<u64>,
}

/// The most active performance states read from one channel, three times the 20 a core of
/// the M5 Pro reports.
const ACTIVE_STATES_MAX: usize = 64;

impl States {
    fn new() -> Self {
        Self {
            active_ns: Vec::with_capacity(ACTIVE_STATES_MAX),
        }
    }
}

pub struct IoReport {
    timebase: Timebase,
    channels_key: Owned,
    subscription: Owned,
    subscribed: Owned,
    pub gpu_energy_nj: u64,
    pub gpu: States,
    /// One for each CPU the machine runs, of which the first `core_count` hold the last read.
    cores: Vec<States>,
    core_count: usize,
}

impl IoReport {
    pub fn open(timebase: Timebase, cpu_limit: usize) -> io::Result<Self> {
        let channels_key = string(c"IOReportChannels");
        let wanted = channels_in_group(c"Energy Model", None)?;
        merge_group(&wanted, c"GPU Stats", c"GPU Performance States")?;
        merge_group(&wanted, c"CPU Stats", c"CPU Core Performance States")?;
        keep_read_channels(&wanted, &channels_key)?;

        let mut subscribed = ptr::null();
        let subscription = unsafe {
            IOReportCreateSubscription(ptr::null(), wanted.0, &raw mut subscribed, 0, ptr::null())
        };
        if subscription.is_null() || subscribed.is_null() {
            return Err(io::Error::other("subscribing to IOReport's channels"));
        }

        Ok(Self {
            timebase,
            channels_key,
            subscription: Owned(subscription),
            subscribed: Owned(subscribed),
            gpu_energy_nj: 0,
            gpu: States::new(),
            cores: (0..cpu_limit).map(|_| States::new()).collect(),
            core_count: 0,
        })
    }

    pub fn read(&mut self) -> io::Result<()> {
        let samples =
            unsafe { IOReportCreateSamples(self.subscription.0, self.subscribed.0, ptr::null()) };
        if samples.is_null() {
            return Err(io::Error::other("sampling IOReport"));
        }

        let samples = Owned(samples);
        let channels = channels_of(&samples, &self.channels_key)?;

        let mut core_count = 0;
        for channel in each(channels) {
            match Kind::of(channel) {
                Some(Kind::GpuEnergy) => self.gpu_energy_nj = energy_nj(channel)?,
                Some(Kind::GpuStates) => {
                    read_active_states(channel, &mut self.gpu, self.timebase)?;
                }
                Some(Kind::CoreStates) => {
                    let Some(core) = self.cores.get_mut(core_count) else {
                        return Err(io::Error::new(
                            io::ErrorKind::QuotaExceeded,
                            format!("IOReport reported cores past {}", self.cores.len()),
                        ));
                    };
                    read_active_states(channel, core, self.timebase)?;
                    core_count += 1;
                }
                None => {}
            }
        }
        self.core_count = core_count;

        Ok(())
    }

    /// The performance states of each core the last read found.
    #[must_use]
    pub fn cores(&self) -> &[States] {
        &self.cores[..self.core_count]
    }
}

#[derive(Clone, Copy)]
enum Kind {
    GpuEnergy,
    GpuStates,
    CoreStates,
}

impl Kind {
    fn of(channel: CfRef) -> Option<Kind> {
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

/// Reads the time `channel` spent in each active state into `into`, whose room it does not
/// grow, so a channel with more active states than that is an error.
fn read_active_states(channel: CfRef, into: &mut States, timebase: Timebase) -> io::Result<()> {
    let capacity = into.active_ns.capacity();
    into.active_ns.clear();
    for index in 0..unsafe { IOReportStateGetCount(channel) } {
        let name = unsafe { IOReportStateGetNameForIndex(channel, index) };
        if !read_active_states_counts(name) {
            continue;
        }

        if into.active_ns.len() == capacity {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                format!("an IOReport channel reported active states past {capacity}"),
            ));
        }
        let residency = unsafe { IOReportStateGetResidency(channel, index) };
        into.active_ns
            .push(timebase.nanos(u64::try_from(residency).unwrap_or(0)));
    }

    assert_eq!(capacity, into.active_ns.capacity());
    Ok(())
}

fn read_active_states_counts(state: CfRef) -> bool {
    ![c"OFF", c"IDLE", c"DOWN"]
        .iter()
        .any(|inactive| is(state, inactive))
}

fn energy_nj(channel: CfRef) -> io::Result<u64> {
    let unit = unsafe { IOReportChannelGetUnitLabel(channel) };
    let nj_per_unit = if is(unit, c"nJ") {
        1
    } else if is(unit, c"uJ") {
        1_000
    } else if is(unit, c"mJ") {
        1_000_000
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IOReport gave the GPU's energy in an unknown unit",
        ));
    };

    let value = unsafe { IOReportSimpleGetIntegerValue(channel, 0) };

    Ok(u64::try_from(value).unwrap_or(0) * nj_per_unit)
}

fn channels_in_group(group: &CStr, subgroup: Option<&CStr>) -> io::Result<Owned> {
    let group = string(group);
    let subgroup = subgroup.map(string);
    let channels = unsafe {
        IOReportCopyChannelsInGroup(
            group.0,
            subgroup.as_ref().map_or(ptr::null(), |s| s.0),
            0,
            0,
            0,
        )
    };
    if channels.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "IOReport has no such channel group",
        ));
    }

    Ok(Owned(channels))
}

fn merge_group(into: &Owned, group: &CStr, subgroup: &CStr) -> io::Result<()> {
    let more = channels_in_group(group, Some(subgroup))?;
    unsafe { IOReportMergeChannels(into.0, more.0, ptr::null()) };

    Ok(())
}

fn keep_read_channels(channels: &Owned, key: &Owned) -> io::Result<()> {
    let all = channels_of(channels, key)?;
    let kept =
        Owned(unsafe { CFArrayCreateMutable(ptr::null(), 0, &raw const kCFTypeArrayCallBacks) });

    for channel in each(all) {
        if Kind::of(channel).is_some() {
            unsafe { CFArrayAppendValue(kept.0, channel) };
        }
    }
    unsafe { CFDictionarySetValue(channels.0, key.0, kept.0) };

    Ok(())
}

fn channels_of(dictionary: &Owned, key: &Owned) -> io::Result<CfRef> {
    let channels = unsafe { CFDictionaryGetValue(dictionary.0, key.0) };
    if channels.is_null() || unsafe { CFGetTypeID(channels) != CFArrayGetTypeID() } {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IOReport gave no list of channels",
        ));
    }

    Ok(channels)
}

fn each(array: CfRef) -> impl Iterator<Item = CfRef> {
    (0..unsafe { CFArrayGetCount(array) })
        .map(move |index| unsafe { CFArrayGetValueAtIndex(array, index) })
}

fn string(text: &CStr) -> Owned {
    Owned(unsafe { CFStringCreateWithCString(ptr::null(), text.as_ptr(), UTF8) })
}

fn is(text: CfRef, want: &CStr) -> bool {
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

    fn open() -> IoReport {
        IoReport::open(Timebase::read(), machine::cpu_limit().unwrap()).unwrap()
    }

    fn active_time_of_every_core(ioreport: &IoReport) -> Duration {
        Duration::from_nanos(
            ioreport
                .cores()
                .iter()
                .flat_map(|core| &core.active_ns)
                .sum(),
        )
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
        // Method: open with a limit of one CPU and read this Mac, which has more.
        let mut ioreport = IoReport::open(Timebase::read(), 1).unwrap();

        let read = ioreport.read();

        assert_eq!(io::ErrorKind::QuotaExceeded, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_channel_with_more_active_states_than_its_room() {
        // Goal: a channel with more active states than the room taken for them is an error, and
        // the list keeps that room.
        // Method: read a core's channel on this Mac, which has many states, into room for one.
        let ioreport = open();
        let samples = Owned(unsafe {
            IOReportCreateSamples(ioreport.subscription.0, ioreport.subscribed.0, ptr::null())
        });
        let channels = channels_of(&samples, &ioreport.channels_key).unwrap();
        let core = each(channels)
            .find(|&channel| matches!(Kind::of(channel), Some(Kind::CoreStates)))
            .unwrap();
        let mut states = States {
            active_ns: Vec::with_capacity(1),
        };
        let capacity = states.active_ns.capacity();

        let read = read_active_states(core, &mut states, ioreport.timebase);

        assert_eq!(io::ErrorKind::QuotaExceeded, read.unwrap_err().kind());
        assert_eq!(capacity, states.active_ns.capacity());
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
        assert!(!read_active_states_counts(string(c"IDLE").0));
        assert!(!read_active_states_counts(string(c"DOWN").0));
        assert!(!read_active_states_counts(string(c"OFF").0));
    }

    #[test]
    fn counts_the_performance_states_of_a_core_and_the_gpu_as_active() {
        // Goal: a performance state left out would hide the time spent in it.
        // Method: ask the filter about a core's state name and the GPU's, as this Mac
        // names them.
        assert!(read_active_states_counts(string(c"V0P14").0));
        assert!(read_active_states_counts(string(c"P1").0));
    }

    #[test]
    fn finds_a_state_left_out_of_active_time_in_every_core_and_the_gpu() {
        // Goal: a macOS that renamed the idle states would read every core as busy all the
        // time, while the two tests above still passed.
        // Method: sample IOReport and look for a state the filter leaves out in each
        // channel of core or GPU states.
        let ioreport = open();
        let samples = Owned(unsafe {
            IOReportCreateSamples(ioreport.subscription.0, ioreport.subscribed.0, ptr::null())
        });
        let channels = channels_of(&samples, &ioreport.channels_key).unwrap();

        let states: Vec<CfRef> = each(channels)
            .filter(|&channel| {
                matches!(Kind::of(channel), Some(Kind::CoreStates | Kind::GpuStates))
            })
            .collect();
        let without_idle = states
            .iter()
            .filter(|&&channel| {
                (0..unsafe { IOReportStateGetCount(channel) }).all(|index| {
                    read_active_states_counts(unsafe {
                        IOReportStateGetNameForIndex(channel, index)
                    })
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
