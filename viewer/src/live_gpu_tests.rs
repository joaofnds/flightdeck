use collector::ring::{IdentityPublication, SupplementalReadings};

/// The tick of the run `gpu_publish` writes at which the GPU power, the GPU memory in use, the
/// GPU memory allocated and the GPU sensor each read their highest, in that order.
const GPU_PEAK_TICKS: [u64; 4] = [31, 32, 33, 34];

/// The wall second of tick 0 of the run `gpu_publish` writes.
const WALL_SECOND_AT_TICK_ZERO: u64 = 1000;

#[test]
fn a_stretch_keeps_each_gpu_reading_the_session_tail_keeps() {
    // Goal: an older stretch keeps the highest of the GPU power, the GPU memory in use
    // and allocated, and the GPU sensor the GPU page draws, with the reading that read the
    // highest, as the session's older tail keeps them for the same seconds, so a peak of the GPU
    // charts drawn from a store is the peak the tail draws. Method: each reading rises at its own
    // pace and reads its highest at its own second of the stretch of 1,030, and the stretch's
    // summaries are set beside the reach of the tail's point.
    let mut fixture = gpu_fixture();
    for tick in 1..=70 {
        gpu_publish(&mut fixture, tick);
    }
    fixture.seal_held();
    let store = crate::store::Store::start(&fixture.root).unwrap();
    // The stretch of 1,030 to 1,039, which the tail's point of 1,030 reaches over.
    let stretch_number = 103;
    let view = crate::store::tests::view_where(&store, |view| {
        view.seconds.len() == 70 && view.stretches.iter().any(|held| held.stretch == stretch_number)
    });
    let stretch = view
        .stretches
        .iter()
        .find(|held| held.stretch == stretch_number)
        .map(|held| held.machine.as_ref())
        .unwrap();
    let tail = LiveSession::open(&fixture.root)
        .unwrap()
        .earlier_context(1070 + CHARTED_SECONDS - 1)
        .unwrap();
    let point = tail["earlier_trends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| {
            point["wall_ns"].as_u64().unwrap() / NANOSECONDS_PER_SECOND / 10 == stretch_number
        })
        .unwrap();

    let kept = |highest: &crate::store::Highest<crate::store::MachinePlace>| {
        let reading = highest.reading.unwrap();
        (reading.value, reading.place)
    };
    let held = [
        ("gpu_watts", kept(&stretch.gpu_watts)),
        (
            "gpu_memory_in_use_bytes",
            kept(&stretch.gpu_memory_in_use_bytes),
        ),
        (
            "gpu_memory_allocated_bytes",
            kept(&stretch.gpu_memory_allocated_bytes),
        ),
        ("gpu_celsius", kept(&stretch.gpu_celsius)),
    ];
    let reached = held.map(|(key, _)| {
        let reach = crate::number_reach(&point["reach"], key).unwrap().unwrap();
        (key, (reach.high, reach.peak))
    });

    assert_eq!(held, reached);
    let peaks = held.map(|(_, kept)| kept.1.second());
    assert_eq!(peaks, GPU_PEAK_TICKS.map(|tick| WALL_SECOND_AT_TICK_ZERO + tick));
}

/// A ring with the system records, which carry the GPU memory macOS reports.
fn gpu_fixture() -> RingFixture {
    RingFixture::started_with("gpu", &one_of_each(120), Registration::start_with_system)
}

/// Publishes tick `tick` of a run whose GPU energy, GPU memory in use, GPU memory allocated and
/// GPU sensor each rise at their own pace and read their highest at their own tick of
/// `GPU_PEAK_TICKS`.
fn gpu_publish(fixture: &mut RingFixture, tick: u64) {
    const MIB: u64 = 1024 * 1024;
    let [watts_peak, in_use_peak, allocated_peak, sensor_peak] = GPU_PEAK_TICKS;
    let energy_nj = |pace: u64, peak: u64| {
        let rise = if tick >= peak { 49 * pace } else { 0 };
        pace * tick + rise
    };
    let gauge = |base: u64, peak: u64| {
        let rise = if tick == peak { 1024 * MIB } else { 0 };
        base + tick * MIB + rise
    };
    let sensor_rise = if tick == sensor_peak { 20.0 } else { 0.0 };
    let sensor = 50.0 + f32::from(u8::try_from(tick).unwrap()) / 10.0 + sensor_rise;
    let reading = collector::system::SystemReading {
        gpu_memory: Some(collector::gpu_memory::GpuMemoryReading {
            in_use_bytes: Some(gauge(4096 * MIB, in_use_peak)),
            allocated_bytes: Some(gauge(8192 * MIB, allocated_peak)),
        }),
        ..collector::system::SystemReading::default()
    };
    let system = collector::ring::SystemPublication {
        reading: &reading,
        machine: &collector::system::MachineFacts::default(),
        clock: None,
        process_gpu_ns: &[],
    };
    fixture
        .writer
        .publish_with_disks(
            &Readings {
                run_tick: tick,
                cpus: &[collector::machine::CPUTicks::default()],
                memory: &collector::machine::Memory::default(),
                interfaces: &[],
                ioreport: Some(collector::ioreport::IOReportReadings {
                    gpu_energy_nj: energy_nj(2_000_000_000, watts_peak),
                    gpu_active_ns: &[],
                    cores: &[],
                }),
                smc: Some(collector::ring::SmcReadings {
                    power_watts: 5.0,
                    soc_celsius: 40.0,
                    cpu_celsius: None,
                    gpu_celsius: Some(sensor),
                    fan_rpm: &[],
                }),
                processes: &[],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[],
                dropped: Dropped::default(),
            },
            Some(&ObservationTimes {
                wall_ns: (WALL_SECOND_AT_TICK_ZERO + tick) * NANOSECONDS_PER_SECOND,
                continuous_ns: tick * NANOSECONDS_PER_SECOND,
                awake_ns: tick * NANOSECONDS_PER_SECOND,
            }),
            &IdentityPublication {
                processes: &[],
                events: &[],
            },
            &SupplementalReadings {
                battery: None,
                disks: None,
                system: Some(&system),
            },
        )
        .unwrap();
}
