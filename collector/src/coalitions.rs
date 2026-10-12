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

#[must_use]
pub fn coalition_of(pid: i32) -> Option<u64> {
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
        return None;
    }

    let info = unsafe { info.assume_init() };
    Some(info.coalition_id[COALITION_TYPE_RESOURCE])
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
    pub gpu_ns: u64,
    pub energy_nj: u64,
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

    pub fn add(&mut self, id: u64) {
        self.ids.insert(id);
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
                gpu_ns: usage.gpu_time,
                energy_nj: usage.energy,
            });
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::cost::spin_on_this_thread_for;
    use crate::processes;

    fn own_coalition() -> u64 {
        coalition_of(std::process::id().cast_signed()).unwrap()
    }

    #[test]
    fn counts_the_cpu_time_this_process_spends_in_its_coalition() {
        let mut coalitions = Coalitions::new(Timebase::read());
        coalitions.add(own_coalition());
        coalitions.read_all();
        let before = coalitions.read[0];

        spin_on_this_thread_for(Duration::from_millis(50));
        coalitions.read_all();

        let after = coalitions.read[0];
        assert!(
            after.cpu_ns - before.cpu_ns >= 50_000_000,
            "grew {} ns",
            after.cpu_ns - before.cpu_ns
        );
    }

    #[test]
    fn counts_the_energy_this_process_spends_in_its_coalition() {
        let timebase = Timebase::read();
        let pid = std::process::id().cast_signed();
        let mut coalitions = Coalitions::new(timebase);
        coalitions.add(own_coalition());
        coalitions.read_all();
        let before = coalitions.read[0];
        let own_before = processes::read_usage(pid, timebase).unwrap();

        spin_on_this_thread_for(Duration::from_millis(50));

        let own_after = processes::read_usage(pid, timebase).unwrap();
        coalitions.read_all();
        let after = coalitions.read[0];
        let own_nj = own_after.energy_nj - own_before.energy_nj;
        assert!(
            after.energy_nj - before.energy_nj >= own_nj,
            "coalition grew {} nJ, this process {own_nj} nJ",
            after.energy_nj - before.energy_nj
        );
    }

    #[test]
    fn reads_each_coalition_once_however_many_processes_it_holds() {
        let mut coalitions = Coalitions::new(Timebase::read());
        coalitions.add(own_coalition());
        coalitions.add(own_coalition());

        coalitions.read_all();

        assert_eq!(1, coalitions.read.len());
    }

    #[test]
    fn finds_a_child_in_its_parents_coalition() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("1")
            .spawn()
            .unwrap();

        let found = coalition_of(child.id().cast_signed());
        child.kill().unwrap();
        child.wait().unwrap();

        assert_eq!(Some(own_coalition()), found);
    }

    #[test]
    fn finds_no_coalition_for_a_reaped_pid() {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        child.wait().unwrap();

        assert_eq!(None, coalition_of(child.id().cast_signed()));
    }
}
