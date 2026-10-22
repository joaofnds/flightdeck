use super::*;

fn time(pid: i32, gpu_ns: u64) -> GpuClientTime {
    GpuClientTime { pid, gpu_ns }
}

#[test]
fn gpu_ns_by_pid_gives_each_pid_its_own_time_and_none_without_clients() {
    // Goal: a process's GPU time lands on its own pid, and a pid with no clients reads none.
    // Method: merge rising pids, some with no time, against times for pids absent from them.
    let times = [time(3, 30), time(5, 50), time(9, 90), time(12, 120)];
    let mut gpu_ns = Vec::with_capacity(5);

    gpu_ns_by_pid([1, 3, 4, 9, 20].into_iter(), &times, &mut gpu_ns);

    assert_eq!(gpu_ns, [None, Some(30), None, Some(90), None]);
}

#[test]
fn gpu_ns_by_pid_reads_none_for_a_time_past_what_the_ring_carries() {
    // Goal: a GPU time the ring cannot carry reads as unknown rather than ending the run.
    // Method: merge the largest time the ring carries and one past it, as a saturated sum.
    let ring_max = crate::ring::GPU_NS_MAX;
    let times = [time(3, ring_max), time(5, u64::MAX)];
    let mut gpu_ns = Vec::with_capacity(2);

    gpu_ns_by_pid([3, 5].into_iter(), &times, &mut gpu_ns);

    assert_eq!(gpu_ns, [Some(ring_max), None]);
}

#[test]
fn gpu_ns_by_pid_reads_none_for_every_pid_without_times() {
    // Goal: a tick whose GPU read failed gives every process none, never a stale time.
    // Method: merge pids against no times into a list that held an earlier tick's.
    let mut gpu_ns = Vec::with_capacity(2);
    gpu_ns.push(Some(7));

    gpu_ns_by_pid([1, 2].into_iter(), &[], &mut gpu_ns);

    assert_eq!(gpu_ns, [None, None]);
}

#[test]
fn fact_text_keeps_utf8_text_up_to_its_bound() {
    // Goal: a fact's text holds what the kernel names and refuses what it cannot hold.
    // Method: build texts that are valid, empty, too long, holding a NUL and not UTF-8.
    let longest = [b'a'; FACT_BYTES_MAX];

    assert_eq!(
        FactText::new(b"Apple M5 Pro").unwrap().as_str(),
        "Apple M5 Pro"
    );
    assert_eq!(FactText::new(&longest).unwrap().as_bytes(), longest);
    assert_eq!(FactText::new(b""), None);
    assert_eq!(FactText::new(&[b'a'; FACT_BYTES_MAX + 1]), None);
    assert_eq!(FactText::new(b"a\0b"), None);
    assert_eq!(FactText::new(&[0xff, 0xfe]), None);
}

#[test]
fn reads_the_readings_of_this_mac() {
    // Goal: an Apple silicon Mac gives every system reading and every machine fact.
    // Method: open the sources on this Mac, read one tick, and compare with the kernel.
    let mut system = System::open();

    system.read();
    system.read_gpu();

    let reading = system.reading;
    assert!(
        reading
            .load_average
            .unwrap()
            .iter()
            .all(|average| *average >= 0.0)
    );
    assert!(reading.open_files.unwrap() <= reading.open_files_limit.unwrap());
    assert!(reading.thermal_pressure.unwrap() <= 4);
    assert!(reading.gpu_memory.unwrap().in_use_bytes.is_some());
    assert!(reading.boot_wall_ns.unwrap() > 1_700_000_000_000_000_000);
    assert!(!system.gpu_times.is_empty());
    let machine = system.machine;
    assert!(machine.model.unwrap().as_str().starts_with("Mac"));
    assert!(machine.chip.unwrap().as_str().starts_with("Apple"));
    assert_eq!(machine.architecture.unwrap().as_str(), "arm64");
    assert!(machine.cpu_cores.unwrap() > 0);
    assert!(machine.gpu_cores.unwrap() > 0);
    assert_eq!(
        machine.memory_bytes.unwrap(),
        u64::from_ne_bytes(crate::kernel::sysctl_bytes(c"hw.memsize"))
    );
    let [fastest, slower] = machine.levels.unwrap();
    assert!(!fastest.name.as_str().is_empty());
    assert!(slower.logical_cpus > 0);
}
