use super::*;
use crate::recording::discovery::tests::allocator_calls_in;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Failure {
    NullSample,
    MissingChannels,
    InvalidChannels,
    ChannelLimit,
    CoreLimit,
    ChannelsChanged,
    StatesChanged,
    NegativeResidency,
    NanosecondsOverflow,
    NegativeEnergy,
    EnergyOverflow,
}

pub(crate) const FAILURES: [Failure; 11] = [
    Failure::NullSample,
    Failure::MissingChannels,
    Failure::InvalidChannels,
    Failure::ChannelLimit,
    Failure::CoreLimit,
    Failure::ChannelsChanged,
    Failure::StatesChanged,
    Failure::NegativeResidency,
    Failure::NanosecondsOverflow,
    Failure::NegativeEnergy,
    Failure::EnergyOverflow,
];

pub(crate) struct Samples {
    answers: [Option<Owned>; 4],
    selected: usize,
}

impl Samples {
    pub(crate) fn sample(&self) -> CFTypeRef {
        self.answers[self.selected]
            .as_ref()
            .map_or(ptr::null(), |sample| unsafe { CFRetain(sample.0) })
    }
}

pub(crate) fn fixture(failure: Option<Failure>) -> IOReport {
    let mut answers = std::array::from_fn(|index| Some(one_core_sample(index)));
    let timebase = match failure {
        Some(Failure::NanosecondsOverflow) => Timebase::OVERFLOWING_TICKS,
        _ => Timebase::NANOSECOND_TICKS,
    };
    if matches!(failure, Some(Failure::EnergyOverflow)) {
        for sample in answers.iter().flatten() {
            with_unit(first_channel(sample, Kind::GPUEnergy), UNIT_UJ);
        }
    }
    let mut reader = Reader::new(timebase, 1).unwrap();
    reader.learn(answers[0].as_ref().unwrap()).unwrap();
    if let Some(failure) = failure {
        damage_sample(&mut answers[1], failure);
    }
    IOReport {
        valid: false,
        subscription: Subscription::Fixture(Samples {
            answers,
            selected: 0,
        }),
        reader,
    }
}

fn one_core_sample(sequence: usize) -> Owned {
    let sample = m5_pro_sample();
    let channels = ChannelsKey::new().unwrap().channels_of(&sample).unwrap();
    for index in (3..unsafe { CFArrayGetCount(channels) }).rev() {
        assert!(
            Kind::of(unsafe { CFArrayGetValueAtIndex(channels, index) }) == Some(Kind::CoreStates)
        );
        unsafe { CFArrayRemoveValueAtIndex(channels, index) };
    }
    for channel in each(channels).unwrap() {
        let value = i64::try_from(sequence).unwrap() * 100;
        if Kind::of(channel) == Some(Kind::GPUEnergy) {
            set_raw(channel, 32, &value.to_ne_bytes());
        } else {
            for index in 0..unsafe { IOReportStateGetCount(channel) } {
                set_raw(
                    channel,
                    usize::try_from(index).unwrap() * 64 + 48,
                    &value.to_ne_bytes(),
                );
            }
        }
    }
    sample
}

// RawElements contains 64-byte IOReport elements. Energy starts at byte 32; a state
// element's residency starts at byte 48. The fixture's native decoder verifies these bytes.
fn set_raw(channel: CFTypeRef, offset: usize, value: &[u8]) {
    unsafe extern "C" {
        fn CFDataGetMutableBytePtr(data: CFTypeRef) -> *mut u8;
    }
    let key = string(c"RawElements").unwrap();
    let raw = unsafe { CFDictionaryGetValue(channel, key.0) };
    assert!(!raw.is_null());
    let size = usize::try_from(unsafe { CFDataGetLength(raw) }).unwrap();
    assert!(offset + value.len() <= size);
    let bytes = unsafe { CFDataGetMutableBytePtr(raw) };
    assert!(!bytes.is_null());
    unsafe { ptr::copy_nonoverlapping(value.as_ptr(), bytes.add(offset), value.len()) };
}

fn damage_sample(answer: &mut Option<Owned>, failure: Failure) {
    let sample = answer.as_ref().unwrap();
    let key = ChannelsKey::new().unwrap();
    let channels = key.channels_of(sample).unwrap();
    let core = first_channel(sample, Kind::CoreStates);
    let energy = first_channel(sample, Kind::GPUEnergy);
    match failure {
        Failure::NullSample => *answer = None,
        Failure::MissingChannels => {
            *answer = Some(
                Owned::new(
                    unsafe { CFDictionaryCreateMutable(ptr::null(), 0, ptr::null(), ptr::null()) },
                    "empty dictionary",
                )
                .unwrap(),
            );
        }
        Failure::InvalidChannels => unsafe { CFDictionarySetValue(sample.0, key.0.0, core) },
        Failure::ChannelLimit => {
            for _index in 3..=CHANNELS_MAX {
                unsafe { CFArrayAppendValue(channels, core) };
            }
        }
        Failure::CoreLimit => unsafe { CFArrayAppendValue(channels, core) },
        Failure::ChannelsChanged => unsafe { CFArrayRemoveValueAtIndex(channels, 2) },
        Failure::StatesChanged => {
            let raw_key = string(c"LegendChannel").unwrap();
            let gpu = first_channel(sample, Kind::GPUStates);
            let raw = unsafe { CFDictionaryGetValue(gpu, raw_key.0) };
            unsafe { CFDictionarySetValue(core, raw_key.0, raw) };
        }
        Failure::NegativeResidency => set_raw(core, 3 * 64 + 48, &(-1_i64).to_ne_bytes()),
        Failure::NanosecondsOverflow => {
            set_raw(core, 3 * 64 + 48, &i64::MAX.to_ne_bytes());
        }
        Failure::NegativeEnergy => set_raw(energy, 32, &(-1_i64).to_ne_bytes()),
        Failure::EnergyOverflow => set_raw(energy, 32, &i64::MAX.to_ne_bytes()),
    }
}

pub(crate) fn set_read(ioreport: &mut IOReport, sequence: u8) {
    let Subscription::Fixture(samples) = &mut ioreport.subscription else {
        panic!("expected native-answer fixture");
    };
    samples.selected = usize::from(sequence);
}

pub(crate) fn assert_released(ioreport: &IOReport) {
    let Subscription::Fixture(samples) = &ioreport.subscription else {
        panic!("expected native-answer fixture");
    };
    for sample in samples.answers.iter().flatten() {
        assert_eq!(unsafe { CFGetRetainCount(sample.0) }, 1);
    }
}

fn expected(failure: Failure) -> ReadError {
    match failure {
        Failure::NullSample => ReadError::NullSample,
        Failure::MissingChannels => ReadError::MissingChannels,
        Failure::InvalidChannels => ReadError::InvalidChannels,
        Failure::ChannelLimit => ReadError::ChannelLimit(CHANNELS_MAX + 1),
        Failure::CoreLimit => ReadError::CoreLimit(1),
        Failure::ChannelsChanged => ReadError::ChannelsChanged {
            found: [1, 1, 0],
            expected: [1, 1, 1],
        },
        Failure::StatesChanged => ReadError::StatesChanged {
            found: 16,
            expected: 17,
        },
        Failure::NegativeResidency => ReadError::NegativeResidency(-1),
        Failure::NanosecondsOverflow => {
            ReadError::NanosecondsOverflow(u64::try_from(i64::MAX).unwrap())
        }
        Failure::NegativeEnergy => ReadError::NegativeEnergy(-1),
        Failure::EnergyOverflow => ReadError::EnergyOverflow {
            units: u64::try_from(i64::MAX).unwrap(),
            nj_per_unit: 1_000,
        },
    }
}

pub(crate) fn check_failure(failure: Failure) {
    let mut ioreport = fixture(Some(failure));
    assert!(ioreport.reading().is_none());
    ioreport.read().unwrap();
    let storage = ioreport.reader.cores.as_ptr();
    let capacity = ioreport.reader.gpu.active_ns.capacity();
    set_read(&mut ioreport, 1);

    crate::diagnostics::error(format_args!("IOReport diagnostic warm-up"));
    let calls = allocator_calls_in(|| {
        for _attempt in 0..16 {
            let error = ioreport.read().unwrap_err();
            assert_eq!(error, expected(failure));
            crate::diagnostics::error(format_args!("collector: IOReport read failed: {error:?}"));
            assert!(ioreport.reading().is_none());
            assert_released(&ioreport);
        }
    });

    assert_eq!(calls, 0);
    assert_eq!(ioreport.reader.cores.as_ptr(), storage);
    assert_eq!(ioreport.reader.gpu.active_ns.capacity(), capacity);
    if matches!(failure, Failure::NegativeResidency) {
        assert_eq!(ioreport.reader.gpu_energy_nj, 100);
        assert_eq!(ioreport.reader.gpu.active_ns, [100; 15]);
        assert_eq!(ioreport.reader.cores[0].active_ns, [100]);
    }
    set_read(&mut ioreport, 2);
    ioreport.read().unwrap();
    let recovered = match failure {
        Failure::EnergyOverflow => 200_000,
        _ => 200,
    };
    assert_eq!(ioreport.reading().unwrap().gpu_energy_nj, recovered);
    assert_released(&ioreport);
    println!("IOReport {failure:?}: repeated_attempts=16 rust_allocator_calls={calls}");
}

macro_rules! failure_test {
    ($name:ident, $failure:ident) => {
        #[test]
        fn $name() {
            // Goal: returned native/decode failures hide all partial state and retain resources.
            // Method: repeat a preloaded bad answer, count Rust allocations, then recover.
            crate::tests::with_diagnostics(
                concat!("ioreport::tests::read_tests::", stringify!($name)),
                || check_failure(Failure::$failure),
            );
        }
    };
}

failure_test!(isolates_null_sample_without_allocating, NullSample);
failure_test!(
    isolates_missing_channels_without_allocating,
    MissingChannels
);
failure_test!(
    isolates_invalid_channels_without_allocating,
    InvalidChannels
);
failure_test!(isolates_channel_limit_without_allocating, ChannelLimit);
failure_test!(isolates_core_limit_without_allocating, CoreLimit);
failure_test!(
    isolates_changed_channels_without_allocating,
    ChannelsChanged
);
failure_test!(isolates_changed_states_without_allocating, StatesChanged);
failure_test!(
    isolates_negative_residency_without_allocating,
    NegativeResidency
);
failure_test!(
    isolates_time_overflow_without_allocating,
    NanosecondsOverflow
);
failure_test!(isolates_negative_energy_without_allocating, NegativeEnergy);
failure_test!(isolates_energy_overflow_without_allocating, EnergyOverflow);

#[test]
fn reports_negative_native_channel_count_without_allocating() {
    // Goal: the native count guard keeps even an impossible CF array count fixed-size.
    // Method: exercise the count validation directly, since CF cannot build a negative array.
    crate::tests::with_diagnostics(
        concat!(
            "ioreport::tests::read_tests::",
            "reports_negative_native_channel_count_without_allocating"
        ),
        check_negative_native_channel_count,
    );
}

pub(crate) fn check_negative_native_channel_count() {
    crate::diagnostics::error(format_args!("IOReport diagnostic warm-up"));
    let calls = allocator_calls_in(|| {
        let error = each_channel_count_check(-1).unwrap_err();
        assert_eq!(error, ReadError::ChannelCount(-1));
        crate::diagnostics::error(format_args!("collector: IOReport read failed: {error:?}"));
    });
    assert_eq!(calls, 0);
}
