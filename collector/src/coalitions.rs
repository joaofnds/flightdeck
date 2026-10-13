use std::ffi::c_int;
use std::io;

use crate::clock::Timebase;
use crate::kernel::ESRCH;
use crate::processes::{self, Unreadable};
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

pub fn coalition_of(pid: i32) -> io::Result<Option<u64>> {
    assert!(pid >= 0);
    assert!(pid <= processes::PID_MAX);

    let info = unsafe { processes::pid_info::<PidCoalitionInfo>(pid, PROC_PIDCOALITIONINFO) }?;

    match info {
        Ok(info) => Ok(Some(info.coalition_id[COALITION_TYPE_RESOURCE])),
        Err(Unreadable::Refused | Unreadable::Gone) => Ok(None),
    }
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

impl Coalition {
    /// Returns whether a process still belongs to the coalition, which holds while fewer of its
    /// tasks exited than started.
    #[must_use]
    pub fn holds_a_process(&self) -> bool {
        self.tasks_exited < self.tasks_started
    }
}

pub struct Coalitions {
    timebase: Timebase,
    /// The coalitions to read, sorted, with each id once. A coalition leaves once a read finds
    /// no process in it.
    ids: Vec<u64>,
    pub read: Vec<Coalition>,
}

impl Coalitions {
    /// Holds at most `coalition_limit` coalitions, with room for them taken once here.
    #[must_use]
    pub fn new(timebase: Timebase, coalition_limit: usize) -> Self {
        assert!(coalition_limit > 0);

        let coalitions = Self {
            timebase,
            ids: Vec::with_capacity(coalition_limit),
            read: Vec::with_capacity(coalition_limit),
        };
        assert!(coalitions.ids.capacity() >= coalition_limit);
        assert!(coalitions.read.capacity() >= coalitions.ids.capacity());
        coalitions
    }

    /// Adds the coalition `id` unless it holds it already. A coalition past the limit is an
    /// error, and the list does not grow.
    pub fn add(&mut self, id: u64) -> io::Result<()> {
        let capacity = self.ids.capacity();
        assert!(self.ids.len() <= capacity);
        let Err(position) = self.ids.binary_search(&id) else {
            return Ok(());
        };

        if self.ids.len() < capacity {
            self.ids.insert(position, id);
            // The ids stay sorted, with each id once.
            if position > 0 {
                assert!(self.ids[position - 1] < id);
            }
            if position + 1 < self.ids.len() {
                assert!(id < self.ids[position + 1]);
            }
            assert_eq!(capacity, self.ids.capacity());
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                format!("the coalitions filled the room for {capacity}"),
            ))
        }
    }

    pub fn read_all(&mut self) -> io::Result<()> {
        assert!(self.ids.len() <= self.read.capacity());
        assert!(self.ids.windows(2).all(|pair| pair[0] < pair[1]));
        self.read.clear();

        for &id in &self.ids {
            let mut usage = ResourceUsage::default();
            let returned = unsafe {
                coalition_info_resource_usage(id, &raw mut usage, size_of::<ResourceUsage>())
            };
            if read_all_returned(returned, io::Error::last_os_error())? {
                let coalition = Coalition {
                    id,
                    tasks_started: usage.tasks_started,
                    tasks_exited: usage.tasks_exited,
                    cpu_ns: self.timebase.nanos(usage.cpu_time),
                    interrupt_wakeups: usage.interrupt_wakeups,
                    idle_wakeups: usage.platform_idle_wakeups,
                    disk_read_bytes: usage.bytesread,
                    disk_written_bytes: usage.byteswritten,
                    gpu_ns: usage.gpu_time,
                    energy_nj: usage.energy,
                };
                push_without_growing(&mut self.read, coalition);
            }
        }

        read_all_retain(&mut self.ids, &self.read);
        Ok(())
    }
}

/// Keeps among `ids` each coalition whose reading in `read` holds a process. `read` holds a
/// reading for each coalition macOS still has, in the order of `ids`.
///
/// A coalition with no process left stays readable for minutes at least, so holding it would
/// fill the list over a long run. Its last reading is in `read`, and a start that names it
/// again adds it back.
fn read_all_retain(ids: &mut Vec<u64>, read: &[Coalition]) {
    assert!(read.len() <= ids.len());

    let mut readings = read.iter().peekable();
    ids.retain(|&id| {
        readings
            .next_if(|coalition| coalition.id == id)
            .is_some_and(Coalition::holds_a_process)
    });

    assert!(readings.next().is_none());
    assert!(ids.len() <= read.len());
}

/// Reads what a `coalition_info_resource_usage` `returned`, and the `error` it set where it
/// failed, which it does by returning -1: true where it read the coalition, false where macOS
/// no longer has it, and any other failure as the error it is.
fn read_all_returned(returned: c_int, error: io::Error) -> io::Result<bool> {
    assert!(error.raw_os_error().is_some());

    if returned == 0 {
        Ok(true)
    } else {
        if returned == -1 {
            if error.raw_os_error() == Some(ESRCH) {
                Ok(false)
            } else {
                Err(error)
            }
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("coalition_info_resource_usage returned {returned}, neither 0 nor -1"),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::cost::spin_on_this_thread_for;

    const COALITION_LIMIT: usize = 16;

    /// A reading with every count at zero, for a test to fill the counts it needs.
    const READING: Coalition = Coalition {
        id: 1,
        tasks_started: 0,
        tasks_exited: 0,
        cpu_ns: 0,
        interrupt_wakeups: 0,
        idle_wakeups: 0,
        disk_read_bytes: 0,
        disk_written_bytes: 0,
        gpu_ns: 0,
        energy_nj: 0,
    };

    fn own_coalition() -> u64 {
        coalition_of(std::process::id().cast_signed())
            .unwrap()
            .unwrap()
    }

    #[test]
    fn counts_the_cpu_time_this_process_spends_in_its_coalition() {
        // Goal: a coalition's CPU time grows by at least the time one of its processes spends.
        // Method: read this process's coalition before and after spinning 50 ms on one thread.
        let mut coalitions = Coalitions::new(Timebase::read(), COALITION_LIMIT);
        coalitions.add(own_coalition()).unwrap();
        coalitions.read_all().unwrap();
        let before = coalitions.read[0];

        spin_on_this_thread_for(Duration::from_millis(50));
        coalitions.read_all().unwrap();

        let after = coalitions.read[0];
        assert!(
            after.cpu_ns - before.cpu_ns >= 50_000_000,
            "grew {} ns",
            after.cpu_ns - before.cpu_ns
        );
    }

    #[test]
    fn counts_the_energy_this_process_spends_in_its_coalition() {
        // Goal: a coalition's energy grows by at least what one of its processes spends.
        // Method: read this process's coalition and usage before and after spinning 50 ms.
        let timebase = Timebase::read();
        let pid = std::process::id().cast_signed();
        let mut coalitions = Coalitions::new(timebase, COALITION_LIMIT);
        coalitions.add(own_coalition()).unwrap();
        coalitions.read_all().unwrap();
        let before = coalitions.read[0];
        let own_before = processes::read_usage(timebase, pid).unwrap().unwrap();

        spin_on_this_thread_for(Duration::from_millis(50));

        let own_after = processes::read_usage(timebase, pid).unwrap().unwrap();
        coalitions.read_all().unwrap();
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
        // Goal: a coalition added twice, as two of its processes would add it, is read once.
        // Method: add this process's coalition twice, and read every coalition.
        let mut coalitions = Coalitions::new(Timebase::read(), COALITION_LIMIT);
        coalitions.add(own_coalition()).unwrap();
        coalitions.add(own_coalition()).unwrap();

        coalitions.read_all().unwrap();

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

        assert!(added.is_ok(), "{added:?}");
        assert_eq!(vec![1], coalitions.ids);
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

        assert!(added.is_ok(), "{added:?}");
        assert_eq!(vec![1, 3], coalitions.ids);
    }

    #[test]
    fn holds_a_process_until_every_task_it_started_exited() {
        // Goal: a coalition holds a process while fewer of its tasks exited than started, and
        // none once as many exited as started, which is how an ended app's coalition reads.
        // Method: a reading with one task left, and the same reading once that task exited.
        let running = Coalition {
            tasks_started: 4,
            tasks_exited: 3,
            ..READING
        };
        let ended = Coalition {
            tasks_exited: 4,
            ..running
        };

        assert!(running.holds_a_process());
        assert!(!ended.holds_a_process());
    }

    #[test]
    fn keeps_only_the_coalitions_read_holding_a_process() {
        // Goal: after a read, the list keeps a coalition only while its reading holds a process,
        // so neither one macOS no longer has nor one whose processes all exited stays.
        // Method: keep from coalitions 1 to 4 with readings for 2 and 4, holding a process, and
        // 3, holding none, as a read that found 1 gone would give.
        let running = Coalition {
            tasks_started: 1,
            ..READING
        };
        let ended = Coalition {
            id: 3,
            tasks_started: 1,
            tasks_exited: 1,
            ..READING
        };
        let read = [
            Coalition { id: 2, ..running },
            ended,
            Coalition { id: 4, ..running },
        ];
        let mut ids = vec![1, 2, 3, 4];

        read_all_retain(&mut ids, &read);

        assert_eq!(vec![2, 4], ids);
    }

    #[test]
    fn drops_a_coalition_macos_does_not_have() {
        // Goal: a coalition macOS no longer has leaves the list without a reading, since its last
        // reading was taken while it had one.
        // Method: add coalition 0, which macOS reports with ESRCH, and read every coalition.
        let mut coalitions = Coalitions::new(Timebase::read(), COALITION_LIMIT);
        coalitions.add(0).unwrap();

        coalitions.read_all().unwrap();

        assert!(coalitions.read.is_empty());
        assert!(coalitions.ids.is_empty());
    }

    #[test]
    fn reports_a_coalition_read_failing_with_another_errno() {
        // Goal: a coalition read that fails with an errno other than ESRCH is that error, rather
        // than the coalition leaving the list as if macOS no longer had it.
        // Method: add this process's jetsam coalition, which is no resource coalition, so macOS
        // refuses to read its resource usage with EINVAL.
        const COALITION_TYPE_JETSAM: usize = 1;
        const EINVAL: i32 = 22;
        let pid = std::process::id().cast_signed();
        let info = unsafe { processes::pid_info::<PidCoalitionInfo>(pid, PROC_PIDCOALITIONINFO) };
        let jetsam = info.unwrap().ok().unwrap().coalition_id[COALITION_TYPE_JETSAM];
        let mut coalitions = Coalitions::new(Timebase::read(), COALITION_LIMIT);
        coalitions.add(jetsam).unwrap();

        let read = coalitions.read_all();

        assert_eq!(Some(EINVAL), read.unwrap_err().raw_os_error());
    }

    #[test]
    fn refuses_a_coalition_read_that_returned_neither_success_nor_failure() {
        // Goal: coalition_info_resource_usage returns 0 or -1, so any other value is an error
        // rather than a reading taken from whatever errno an earlier call left.
        // Method: read a call that returned 1 with ESRCH left in errno.
        let returned = read_all_returned(1, io::Error::from_raw_os_error(ESRCH));

        assert_eq!(io::ErrorKind::InvalidData, returned.unwrap_err().kind());
    }

    #[test]
    fn finds_a_child_in_its_parents_coalition() {
        // Goal: a child starts in its parent's coalition.
        // Method: spawn a child that sleeps, and find its coalition.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("1")
            .spawn()
            .unwrap();

        let found = coalition_of(child.id().cast_signed()).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();

        assert_eq!(Some(own_coalition()), found);
    }

    #[test]
    fn finds_no_coalition_for_a_reaped_pid() {
        // Goal: a pid whose process exited and was reaped has no coalition.
        // Method: spawn a child, wait for it, and ask for its pid's coalition.
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        child.wait().unwrap();

        assert_eq!(None, coalition_of(child.id().cast_signed()).unwrap());
    }
}
