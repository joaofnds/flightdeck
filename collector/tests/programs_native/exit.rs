use std::io::Write;

use collector::programs::{CreationError, read_creation};

use super::Fixture;
use crate::sample::Sample;

impl Fixture {
    pub fn exercise_exit(&mut self) {
        let mut baseline = Sample::EMPTY;
        baseline.capture(self.pid());
        baseline.assert_image(&self.ack, &self.image_a, &self.app_a);
        baseline.report("before-owned-exit");
        self.peer.take().unwrap().write_all(b"Q").unwrap();
        let mut racing = Sample::EMPTY;
        let mut accepted = 0_u32;
        for _attempt in 0..128 {
            racing.capture(self.pid());
            if racing.acquisition.candidate {
                racing.assert_image(&self.ack, &self.image_a, &self.app_a);
                accepted += 1;
            } else {
                break;
            }
            std::thread::sleep(std::time::Duration::from_micros(25));
        }
        racing.report("during-owned-exit");
        let mut information = Siginfo([0; 13]);
        // SDK sys/wait.h: P_PID1, WEXITED4, WNOWAIT32, preserving ownership until explicit wait.
        assert_eq!(0, unsafe {
            waitid(1, self.pid().cast_unsigned(), &raw mut information, 4 | 32)
        });
        let mut zombie = Sample::EMPTY;
        zombie.capture_exited(self.pid());
        zombie.report("owned-zombie");
        assert!(!zombie.acquisition.candidate);
        assert_eq!(Ok(self.ack.creation), zombie.acquisition.creation_before);
        assert_eq!(Ok(self.ack.creation), zombie.acquisition.creation_after);
        assert!(zombie.acquisition.readiness.is_err());
        assert!(zombie.acquisition.path.is_none());
        assert!(self.child.as_mut().unwrap().wait().unwrap().success());
        let mut reaped = Err(CreationError::NotRead);
        read_creation(self.pid(), true, &mut reaped);
        if let Ok(creation) = reaped {
            // A freshly reused PID is another creation; never enrich the old owned process.
            assert_ne!(self.ack.creation.unique_id, creation.unique_id);
        } else {
            assert_eq!(Err(CreationError::Gone(3)), reaped);
        }
        eprintln!(
            "exit owned_pid={} accepted_while_exiting={accepted} reaped={reaped:?}",
            self.pid()
        );
    }
}

#[repr(C)]
struct Siginfo([u64; 13]);

const _: () = {
    assert!(size_of::<Siginfo>() == 104);
    assert!(align_of::<Siginfo>() == 8);
};

unsafe extern "C" {
    fn waitid(kind: u32, pid: u32, information: *mut Siginfo, options: i32) -> i32;
}
