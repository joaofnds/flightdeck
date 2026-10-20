use std::ffi::c_int;
use std::mem::{align_of, size_of};
use std::os::unix::ffi::OsStrExt;
use std::process::{Child, Command};

use super::{
    Acquisition, CreationError, PathError, acquire, read_creation, read_path, split_program,
};

#[test]
fn acquires_this_process_with_real_numeric_data_without_allocating() {
    // Goal: actual native acquisition preserves numeric reads with zero Rust heap activity.
    // Method: warm the calls, repeat inside the existing allocator counter and inspect output.
    let pid = i32::try_from(std::process::id()).unwrap();
    let timebase = crate::clock::Timebase::read().unwrap();
    let expected_path = std::env::current_exe().unwrap().canonicalize().unwrap();
    let mut scratch = [0; 4096];
    let mut output = Acquisition::EMPTY;
    let mut numeric = None;
    acquire(pid, false, &mut scratch, &mut output, || {
        numeric = Some(crate::processes::read(timebase, pid));
    });

    let allocations = crate::recording::discovery::tests::allocator_calls_in(|| {
        acquire(pid, false, &mut scratch, &mut output, || {
            numeric = Some(crate::processes::read(timebase, pid));
        });
    });

    assert_eq!(0, allocations);
    assert!(output.candidate);
    assert_eq!(Ok(()), output.readiness);
    assert_eq!(output.creation_before, output.creation_after);
    assert_eq!(pid, numeric.unwrap().unwrap().unwrap().pid);
    let length = output.path.unwrap().unwrap();
    assert_eq!(expected_path.as_os_str().as_bytes(), &scratch[..length]);
    let mut program = None;
    split_program(&scratch[..length], &mut program).unwrap();
    assert_eq!(Ok(()), program.unwrap().validate());
    eprintln!(
        "self pid={pid} before={:?} after={:?} path_length={length} rust_heap_calls={allocations}",
        output.creation_before, output.creation_after
    );
}

#[test]
fn reads_owned_child_creation_alive_exited_unreaped_and_reaped() {
    // Goal: the native flavor17 nonzero argument retains a zombie's real creation only.
    // Method: own a child, kill it, wait without reaping, then explicitly reap and reread.
    let mut child = OwnedChild {
        child: Command::new("/bin/sleep").arg("30").spawn().unwrap(),
    };
    let pid = i32::try_from(child.child.id()).unwrap();
    let mut scratch = [0; 4096];
    let mut alive = Acquisition::EMPTY;
    let mut numeric_calls = 0;
    acquire(pid, false, &mut scratch, &mut alive, || numeric_calls += 1);
    assert!(alive.candidate);
    assert_eq!(1, numeric_calls);
    child.child.kill().unwrap();
    wait_exited_unreaped(child.child.id());

    let mut creation = Err(CreationError::NotRead);
    read_creation(pid, false, &mut creation);
    assert_eq!(Err(CreationError::Gone(3)), creation);
    read_creation(pid, true, &mut creation);
    assert_eq!(alive.creation_before, creation);
    assert_eq!(Err(PathError::Gone(3)), read_path(pid, &mut scratch));
    let mut exited = Acquisition::EMPTY;
    acquire(pid, true, &mut scratch, &mut exited, || numeric_calls += 1);
    assert!(!exited.candidate);
    assert_eq!(alive.creation_before, exited.creation_before);
    assert_eq!(alive.creation_before, exited.creation_after);
    assert!(exited.readiness.is_err());
    assert!(exited.path.is_none());
    assert_eq!(2, numeric_calls);
    eprintln!(
        "child pid={pid} alive={:?} zombie={creation:?} zombie_gate={:?}",
        alive.creation_before, exited.readiness
    );

    assert!(!child.child.wait().unwrap().success());
    read_creation(pid, true, &mut creation);
    assert_eq!(Err(CreationError::Gone(3)), creation);
    eprintln!("child pid={pid} reaped={creation:?}");
}

struct OwnedChild {
    child: Child,
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                if let Err(error) = self.child.kill() {
                    eprintln!("owned child cleanup kill: {error}");
                }
                if let Err(error) = self.child.wait() {
                    eprintln!("owned child cleanup wait: {error}");
                }
            }
            Err(error) => eprintln!("owned child cleanup status: {error}"),
        }
    }
}

fn wait_exited_unreaped(pid: u32) {
    let mut information = Siginfo([0; 13]);
    // SDK sys/wait.h: P_PID=1, WEXITED=4, WNOWAIT=32. No sleep or reaping establishes readiness.
    let returned = unsafe { waitid(1, pid, &raw mut information, 4 | 32) };
    assert_eq!(0, returned, "waitid: {}", std::io::Error::last_os_error());
}

// Room for SDK siginfo_t, matching the existing processes test's opaque waitid buffer.
#[repr(C)]
struct Siginfo([u64; 13]);

const _: () = {
    assert!(size_of::<Siginfo>() == 104);
    assert!(align_of::<Siginfo>() == 8);
};

unsafe extern "C" {
    fn waitid(idtype: u32, id: u32, information: *mut Siginfo, options: c_int) -> c_int;
}

#[test]
fn associates_actual_final_usage_with_owned_zombie_creation_without_allocating() {
    // Goal: final zombie usage has verified creation despite unavailable path and readiness.
    // Method: own a child, wait without reaping, and count heap calls around native bracketing.
    let timebase = crate::clock::Timebase::read().unwrap();
    let mut child = OwnedChild {
        child: Command::new("/bin/sleep").arg("30").spawn().unwrap(),
    };
    let pid = i32::try_from(child.child.id()).unwrap();
    let mut expected = Err(CreationError::NotRead);
    read_creation(pid, false, &mut expected);
    let expected = expected.unwrap();
    let mut alive = super::ExitAcquisition::EMPTY;
    super::acquire_exit(pid, &mut alive, || {
        let mut exited = true;
        crate::processes::read_exit_usage(timebase, pid, &mut exited)
            .unwrap()
            .unwrap();
        Some(exited)
    });
    assert_eq!(Some(false), alive.exited);
    assert_eq!(
        Err(crate::ring::IdentityUnavailable::Changed),
        alive.creation()
    );
    child.child.kill().unwrap();
    wait_exited_unreaped(child.child.id());
    let mut acquired = super::ExitAcquisition::EMPTY;
    let mut numeric = None;

    let allocations = crate::recording::discovery::tests::allocator_calls_in(|| {
        super::acquire_exit(pid, &mut acquired, || {
            let mut exited = false;
            numeric = Some(crate::processes::read_exit_usage(
                timebase,
                pid,
                &mut exited,
            ));
            Some(exited)
        });
    });

    assert_eq!(0, allocations);
    assert_eq!(Ok(expected), acquired.creation());
    let usage = numeric.unwrap().unwrap().unwrap();
    assert!(usage.awake_ns_at_start > 0);
    eprintln!("owned final pid={pid} creation={expected:?} usage={usage:?} heap={allocations}");
    assert!(!child.child.wait().unwrap().success());
}
