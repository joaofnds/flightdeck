use std::ffi::c_int;
use std::io;

use crate::clock::Timebase;
use crate::processes;
use crate::push_without_growing;

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

const PROC_PIDCOALITIONINFO: c_int = 20;
const COALITION_TYPE_RESOURCE: usize = 0;

unsafe extern "C" {
    fn coalition_info_resource_usage(cid: u64, cru: *mut ResourceUsage, sz: usize) -> c_int;
}

#[must_use]
pub fn coalition_of(pid: i32) -> Option<u64> {
    let info: PidCoalitionInfo = unsafe { processes::pid_info(pid, PROC_PIDCOALITIONINFO) }.ok()?;

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
    /// The coalitions to read, sorted, with each id once.
    ids: Vec<u64>,
    pub read: Vec<Coalition>,
}

impl Coalitions {
    /// Holds at most `coalition_limit` coalitions, with room for them taken once here.
    #[must_use]
    pub fn new(timebase: Timebase, coalition_limit: usize) -> Self {
        assert!(coalition_limit > 0);

        Self {
            timebase,
            ids: Vec::with_capacity(coalition_limit),
            read: Vec::with_capacity(coalition_limit),
        }
    }

    /// Adds the coalition `id` unless it holds it already. A coalition past the limit is an
    /// error, and the list does not grow.
    pub fn add(&mut self, id: u64) -> io::Result<()> {
        let capacity = self.ids.capacity();
        let Err(position) = self.ids.binary_search(&id) else {
            return Ok(());
        };
        if self.ids.len() == capacity {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                format!("the coalitions filled the room for {capacity}"),
            ));
        }

        self.ids.insert(position, id);
        assert_eq!(capacity, self.ids.capacity());
        Ok(())
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

            let coalition = Coalition {
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
            };
            push_without_growing(read, coalition);
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::cost::spin_on_this_thread_for;

    const COALITION_LIMIT: usize = 16;

    fn own_coalition() -> u64 {
        coalition_of(std::process::id().cast_signed()).unwrap()
    }

    #[test]
    fn counts_the_cpu_time_this_process_spends_in_its_coalition() {
        let mut coalitions = Coalitions::new(Timebase::read(), COALITION_LIMIT);
        coalitions.add(own_coalition()).unwrap();
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
        let mut coalitions = Coalitions::new(timebase, COALITION_LIMIT);
        coalitions.add(own_coalition()).unwrap();
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
        let mut coalitions = Coalitions::new(Timebase::read(), COALITION_LIMIT);
        coalitions.add(own_coalition()).unwrap();
        coalitions.add(own_coalition()).unwrap();

        coalitions.read_all();

        assert_eq!(1, coalitions.read.len());
    }

    #[test]
    fn refuses_a_coalition_past_its_limit_without_growing() {
        // Goal: a coalition past the limit is an error, and the list keeps the room it was given.
        // Method: fill a set with room for one coalition, then add a second.
        let mut coalitions = Coalitions::new(Timebase::read(), 1);
        coalitions.add(1).unwrap();
        let capacity = coalitions.ids.capacity();

        let added = coalitions.add(2);

        assert_eq!(io::ErrorKind::QuotaExceeded, added.unwrap_err().kind());
        assert_eq!(capacity, coalitions.ids.capacity());
    }

    #[test]
    fn adds_a_coalition_it_holds_already_at_its_limit() {
        // Goal: a full set still takes a coalition it holds, since holding it again takes no room.
        // Method: fill a set with room for one coalition, then add that one again.
        let mut coalitions = Coalitions::new(Timebase::read(), 1);
        coalitions.add(1).unwrap();

        let added = coalitions.add(1);

        assert!(added.is_ok());
    }

    #[test]
    fn finds_a_coalition_it_holds_whatever_order_the_coalitions_came_in() {
        // Goal: a full set finds a coalition it holds after a lower one was added, since it
        // keeps its ids sorted.
        // Method: fill a set with room for two with 3 and then 1, and add 3 again.
        let mut coalitions = Coalitions::new(Timebase::read(), 2);
        coalitions.add(3).unwrap();
        coalitions.add(1).unwrap();

        let added = coalitions.add(3);

        assert!(added.is_ok());
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
