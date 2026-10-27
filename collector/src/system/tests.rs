use super::*;

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
fn read_boot_wall_ns_gives_the_boot_time_in_nanoseconds() {
    // Goal: the boot time's seconds and microseconds add up to nanoseconds since 1970.
    // Method: read 1,760,000,000 s and 250,000 us.
    let boot_time = Timeval {
        tv_sec: 1_760_000_000,
        tv_usec: 250_000,
    };

    assert_eq!(
        1_760_000_000_250_000_000,
        read_boot_wall_ns_of(boot_time).unwrap()
    );
}

#[test]
fn read_boot_wall_ns_refuses_a_second_or_more_of_microseconds() {
    // Goal: microseconds that make up a whole second are an error, never a boot time
    // shifted by it.
    // Method: read 1,000,000 us.
    let boot_time = Timeval {
        tv_sec: 1_760_000_000,
        tv_usec: 1_000_000,
    };

    let read = read_boot_wall_ns_of(boot_time);

    assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
}

#[test]
fn read_boot_wall_ns_refuses_a_boot_before_1970() {
    // Goal: a boot time before 1970 is an error, never a time near the u64's top.
    // Method: read -1 s.
    let boot_time = Timeval {
        tv_sec: -1,
        tv_usec: 0,
    };

    let read = read_boot_wall_ns_of(boot_time);

    assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
}

#[test]
fn read_boot_wall_ns_refuses_negative_microseconds() {
    // Goal: microseconds below zero are an error, never a time near the u64's top.
    // Method: read -1 us.
    let boot_time = Timeval {
        tv_sec: 1_760_000_000,
        tv_usec: -1,
    };

    let read = read_boot_wall_ns_of(boot_time);

    assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
}

#[test]
fn read_boot_wall_ns_refuses_seconds_past_what_nanoseconds_hold() {
    // Goal: a boot time whose nanoseconds pass a u64 is an error, never a wrapped time.
    // Method: read the most seconds an i64 holds.
    let boot_time = Timeval {
        tv_sec: i64::MAX,
        tv_usec: 0,
    };

    let read = read_boot_wall_ns_of(boot_time);

    assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
}

/// The tests whose subject is what this Mac gives, which only an Apple silicon Mac runs. CI's
/// virtual Mac has no power manager, no Apple GPU driver and one core cluster, so its check
/// skips this module.
mod this_mac {
    use super::*;

    #[test]
    fn reads_the_readings_of_this_mac() {
        // Goal: an Apple silicon Mac gives every system reading and every machine fact.
        // Method: open the sources on this Mac, read one tick, and compare with the kernel.
        let mut system = System::open();

        system.read();

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
}
