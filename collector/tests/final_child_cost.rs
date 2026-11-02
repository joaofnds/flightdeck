//! A child that finishes between paced samples retains native footprint and final CPU.
#[cfg(test)]
mod tests {
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    use collector::clock::Timebase;
    use collector::recording::child::reap_owned;

    #[test]
    fn samples_the_exited_child_before_its_final_wait_releases_native_usage() {
        // Goal: a complete short-lived worker cannot disappear from physical peak accounting.
        // Method: allocate and touch sixteen MiB in an owned child with no live cost sample.
        if std::env::var_os("FLIGHTDECK_FINAL_COST_CHILD").is_some() {
            let mut bytes = vec![0_u8; 16 * 1024 * 1024];
            for byte in bytes.iter_mut().step_by(4096) {
                *byte = 1;
            }
            std::hint::black_box(&bytes);
            return;
        }
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::samples_the_exited_child_before_its_final_wait_releases_native_usage",
            ])
            .env("FLIGHTDECK_FINAL_COST_CHILD", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut owned = Owned(Some(child));
        let timebase = Timebase::read().unwrap();
        for _attempt in 0..2000 {
            if let Some(reaped) = reap_owned(owned.0.as_ref().unwrap(), timebase).unwrap() {
                drop(owned.0.take());
                let sample = reaped.footprint.unwrap();
                assert!(sample.footprint_bytes_max >= 16 * 1024 * 1024);
                assert_eq!(sample.footprint_bytes, 0);
                assert!(reaped.finished.cpu_ns > 0);
                assert!(reaped.finished.successful());
                eprintln!(
                    "final_child_cost native_peak={} cpu_ns={} resident_peak={}",
                    sample.footprint_bytes_max,
                    reaped.finished.cpu_ns,
                    reaped.finished.resident_bytes_max
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("owned child exceeded its two-second final-cost deadline");
    }

    struct Owned(Option<Child>);

    impl Drop for Owned {
        fn drop(&mut self) {
            let Some(child) = &mut self.0 else {
                return;
            };
            if child.try_wait().unwrap().is_some() {
                return;
            }
            child.kill().unwrap();
            for _attempt in 0..2000 {
                if child.try_wait().unwrap().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            eprintln!("owned final-cost child remains pid={}", child.id());
        }
    }
}
