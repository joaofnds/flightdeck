//! Native exit registration cannot turn a dead or replaced process into an adopted owner.
#[cfg(test)]
#[path = "support/process.rs"]
mod process;

#[cfg(test)]
mod tests {
    use super::process;
    use std::io;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use collector::recording::control::Health;
    use collector::recording::exit_watch::ExitWatch;

    #[test]
    fn exits_on_either_side_of_registration_never_invent_a_success_status() {
        // Goal: registration and process exit races yield either a verified exit or refusal.
        // Method: kill owned native children before, after and concurrently with registration.
        for timing in 0..10 {
            let mut child = process::OwnedChild::spawn(
                Command::new("/bin/sleep")
                    .arg("5")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
            let health = child_health(&child);
            let watch = if timing == 0 {
                child.child.kill().unwrap();
                assert_eq!(child.wait(Duration::from_secs(2)).signal(), Some(9));
                ExitWatch::register(&health)
            } else if timing == 1 {
                let watch = ExitWatch::register(&health);
                child.child.kill().unwrap();
                watch
            } else {
                let barrier = std::sync::Barrier::new(2);
                std::thread::scope(|scope| {
                    let kill = scope.spawn(|| {
                        barrier.wait();
                        child.child.kill().unwrap();
                    });
                    barrier.wait();
                    let watch = ExitWatch::register(&health);
                    kill.join().unwrap();
                    watch
                })
            };
            match watch {
                Ok(watch) => {
                    let status = wait_exit(&watch);
                    assert_eq!(status.signal(), Some(9));
                    assert_eq!(child.wait(Duration::from_secs(2)).signal(), Some(9));
                    eprintln!("registration_exit timing={timing} verified_signal=9");
                }
                Err(error) => {
                    const ESRCH: i32 = 3;
                    assert!(
                        error.kind() == io::ErrorKind::NotFound
                            || error.raw_os_error() == Some(ESRCH),
                        "unexpected exit-watch refusal: {error:?}"
                    );
                    assert_eq!(child.wait(Duration::from_secs(2)).signal(), Some(9));
                    eprintln!("registration_exit timing={timing} native_identity_gone=true");
                }
            }
        }
    }

    fn child_health(child: &process::OwnedChild) -> Health {
        let pid = i32::try_from(child.child.id()).unwrap();
        let mut creation = Err(collector::programs::CreationError::NotRead);
        collector::programs::read_creation(pid, false, &mut creation);
        Health {
            pid,
            creation: creation.unwrap(),
            ..Health::default()
        }
    }

    fn wait_exit(watch: &ExitWatch) -> std::process::ExitStatus {
        for _attempt in 0..2000 {
            if let Some(status) = watch.poll().unwrap() {
                return status;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("native watch did not observe the owned exit");
    }
}
