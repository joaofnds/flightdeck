use std::collections::BTreeSet;

use crate::clock::Timebase;

#[repr(C)]
#[derive(Default)]
struct ResourceUsage {
    tasks_started: u64,
    tasks_exited: u64,
    time_nonempty: u64,
    cpu_time: u64,
    interrupt_wakeups: u64,
    platform_idle_wakeups: u64,
    bytesread: u64,
    byteswritten: u64,
    gpu_time: u64,
    cpu_time_billed_to_me: u64,
    cpu_time_billed_to_others: u64,
    energy: u64,
}

#[repr(C)]
struct PidCoalitionInfo {
    coalition_id: [u64; 2],
    reserved: [u64; 3],
}

const PROC_PIDCOALITIONINFO: libc::c_int = 20;
const COALITION_TYPE_RESOURCE: usize = 0;

unsafe extern "C" {
    fn coalition_info_resource_usage(cid: u64, cru: *mut ResourceUsage, sz: usize) -> libc::c_int;
}

#[derive(Debug, Clone, Copy)]
pub struct Coalition {
    pub id: u64,
    pub tasks_started: u64,
    pub tasks_exited: u64,
    pub cpu_ns: u64,
    pub interrupt_wakeups: u64,
    pub idle_wakeups: u64,
    pub disk_read_bytes: u64,
    pub disk_written_bytes: u64,
    pub gpu_time: u64,
    pub energy: u64,
}

pub struct Coalitions {
    timebase: Timebase,
    ids: BTreeSet<u64>,
    pub read: Vec<Coalition>,
}

impl Coalitions {
    #[must_use]
    pub fn new(timebase: Timebase) -> Self {
        Self {
            timebase,
            ids: BTreeSet::new(),
            read: Vec::new(),
        }
    }

    pub fn join(&mut self, pid: i32) {
        let mut info = std::mem::MaybeUninit::<PidCoalitionInfo>::uninit();
        let size = libc::c_int::try_from(size_of::<PidCoalitionInfo>()).unwrap_or(0);
        if unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDCOALITIONINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        } != size
        {
            return;
        }

        let info = unsafe { info.assume_init() };
        self.ids.insert(info.coalition_id[COALITION_TYPE_RESOURCE]);
    }

    pub fn read_all(&mut self) {
        self.read.clear();
        let timebase = self.timebase;
        let read = &mut self.read;

        self.ids.retain(|&id| {
            let mut usage = ResourceUsage::default();
            if unsafe {
                coalition_info_resource_usage(id, &raw mut usage, size_of::<ResourceUsage>())
            } != 0
            {
                return false;
            }

            read.push(Coalition {
                id,
                tasks_started: usage.tasks_started,
                tasks_exited: usage.tasks_exited,
                cpu_ns: timebase.nanos(usage.cpu_time),
                interrupt_wakeups: usage.interrupt_wakeups,
                idle_wakeups: usage.platform_idle_wakeups,
                disk_read_bytes: usage.bytesread,
                disk_written_bytes: usage.byteswritten,
                gpu_time: usage.gpu_time,
                energy: usage.energy,
            });
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burn_cpu(for_ns: u64) {
        let start = std::time::Instant::now();
        while start.elapsed().as_nanos() < u128::from(for_ns) {
            std::hint::black_box(0);
        }
    }

    #[test]
    fn counts_the_cpu_time_this_process_spends_in_its_coalition() {
        let mut coalitions = Coalitions::new(Timebase::read());
        coalitions.join(std::process::id().cast_signed());
        coalitions.read_all();
        let before = coalitions.read[0];

        burn_cpu(50_000_000);
        coalitions.read_all();

        let after = coalitions.read[0];
        assert!(
            after.cpu_ns - before.cpu_ns >= 50_000_000,
            "grew {} ns",
            after.cpu_ns - before.cpu_ns
        );
    }

    #[test]
    fn reads_each_coalition_once_however_many_processes_it_holds() {
        let mut coalitions = Coalitions::new(Timebase::read());
        let own = std::process::id().cast_signed();
        coalitions.join(own);
        coalitions.join(own);

        coalitions.read_all();

        assert_eq!(1, coalitions.read.len());
    }
}
