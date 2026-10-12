use std::ffi::{CStr, c_char, c_void};
use std::io;
use std::ptr;

use crate::clock::Timebase;

type CfRef = *const c_void;

#[repr(C)]
struct ArrayCallBacks {
    version: isize,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
}

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

#[derive(Debug, Clone, Default)]
pub struct States {
    pub active_ns: Vec<u64>,
}

pub struct IoReport {
    timebase: Timebase,
    channels_key: Owned,
    subscription: Owned,
    subscribed: Owned,
    pub gpu_energy_nj: u64,
    pub gpu: States,
    pub cores: Vec<States>,
}

impl IoReport {
    pub fn open(timebase: Timebase) -> io::Result<Self> {
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
            gpu: States::default(),
            cores: Vec::new(),
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

        let mut cores = 0;
        for channel in each(channels) {
            match Kind::of(channel) {
                Some(Kind::GpuEnergy) => self.gpu_energy_nj = energy_nj(channel)?,
                Some(Kind::GpuStates) => read_active_states(channel, &mut self.gpu, self.timebase),
                Some(Kind::CoreStates) => {
                    if cores == self.cores.len() {
                        self.cores.push(States::default());
                    }
                    read_active_states(channel, &mut self.cores[cores], self.timebase);
                    cores += 1;
                }
                None => {}
            }
        }
        self.cores.truncate(cores);
        Ok(())
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

fn read_active_states(channel: CfRef, into: &mut States, timebase: Timebase) {
    into.active_ns.clear();
    for index in 0..unsafe { IOReportStateGetCount(channel) } {
        let name = unsafe { IOReportStateGetNameForIndex(channel, index) };
        if [c"OFF", c"IDLE", c"DOWN"]
            .iter()
            .any(|inactive| is(name, inactive))
        {
            continue;
        }

        let residency = unsafe { IOReportStateGetResidency(channel, index) };
        into.active_ns
            .push(timebase.nanos(u64::try_from(residency).unwrap_or(0)));
    }
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
    use crate::machine::Machine;

    fn logical_cpus() -> u32 {
        let mut value = 0u32;
        let mut size = size_of::<u32>();
        unsafe {
            libc::sysctlbyname(
                c"hw.logicalcpu".as_ptr(),
                (&raw mut value).cast(),
                &raw mut size,
                ptr::null_mut(),
                0,
            )
        };
        value
    }

    fn active_time_of_every_core(ioreport: &IoReport) -> Duration {
        Duration::from_nanos(ioreport.cores.iter().flat_map(|core| &core.active_ns).sum())
    }

    fn scheduler_tick() -> Duration {
        let per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        Duration::from_secs(1) / u32::try_from(per_second).unwrap()
    }

    fn busy_time_of_every_cpu(machine: &Machine) -> Duration {
        let ticks: u64 = machine
            .cpus
            .iter()
            .map(|cpu| u64::from(cpu.user) + u64::from(cpu.system) + u64::from(cpu.nice))
            .sum();
        Duration::from_nanos(ticks * u64::try_from(scheduler_tick().as_nanos()).unwrap())
    }

    #[test]
    fn reads_the_performance_states_of_every_logical_cpu() {
        let mut ioreport = IoReport::open(Timebase::read()).unwrap();

        ioreport.read().unwrap();

        assert_eq!(logical_cpus() as usize, ioreport.cores.len());
    }

    #[test]
    fn counts_a_busy_cpu_in_its_cores_active_time() {
        let mut ioreport = IoReport::open(Timebase::read()).unwrap();
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
    fn leaves_the_time_cores_spend_idle_out_of_their_active_time() {
        let mut ioreport = IoReport::open(Timebase::read()).unwrap();
        let mut machine = Machine::new().unwrap();
        ioreport.read().unwrap();
        machine.read().unwrap();
        let active_before = active_time_of_every_core(&ioreport);
        let busy_before = busy_time_of_every_cpu(&machine);

        std::thread::sleep(Duration::from_millis(200));

        ioreport.read().unwrap();
        machine.read().unwrap();
        let active = active_time_of_every_core(&ioreport)
            .checked_sub(active_before)
            .expect("the cores' active time only grows");
        let busy = busy_time_of_every_cpu(&machine)
            .checked_sub(busy_before)
            .expect("the scheduler's busy time only grows");
        let tick_rounding = scheduler_tick() * logical_cpus();
        assert!(
            active <= busy + tick_rounding,
            "{active:?} active across the cores, {busy:?} busy by the scheduler's count"
        );
    }

    #[test]
    fn counts_time_the_gpu_spends_in_active_states() {
        let mut ioreport = IoReport::open(Timebase::read()).unwrap();

        ioreport.read().unwrap();

        assert!(ioreport.gpu.active_ns.iter().sum::<u64>() > 0);
    }

    #[test]
    fn counts_the_energy_the_gpu_spends() {
        let mut ioreport = IoReport::open(Timebase::read()).unwrap();

        ioreport.read().unwrap();

        assert!(ioreport.gpu_energy_nj > 0);
    }
}
