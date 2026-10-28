use super::*;
use crate::recording::discovery::tests::allocator_calls_in;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Failure {
    Native,
    Short,
    Missing,
    Refused,
}

impl Failure {
    pub(super) fn answer(self, question: &Message) -> Result<Message, ReadError> {
        let (returned, size, result) = match self {
            Self::Native => (-71, 0, SUCCESS),
            Self::Short => (kernel::KERN_SUCCESS, 1, SUCCESS),
            Self::Missing => (kernel::KERN_SUCCESS, size_of::<Message>(), KEY_NOT_FOUND),
            Self::Refused => (kernel::KERN_SUCCESS, size_of::<Message>(), 135),
        };
        ask_answered(
            question,
            returned,
            size,
            Message {
                result,
                ..Message::default()
            },
        )
    }

    fn expected(self) -> ReadError {
        match self {
            Self::Native => ReadError::Native(-71),
            Self::Short => ReadError::AnswerSize(1),
            Self::Missing => ReadError::MissingKey {
                key: Key::fan_speed(1).0,
                command: READ_BYTES,
                index: 0,
            },
            Self::Refused => ReadError::Controller(135),
        }
    }
}

pub(crate) fn set_read(smc: &mut Smc, sequence: u8, failure: Option<Failure>) {
    let Controller::Fake(fake) = &mut smc.controller else {
        panic!("expected native-answer fixture");
    };
    fake.failure = failure;
    for answer in &mut fake.answers {
        if answer.info.is(FLOAT) {
            let value = 10.0 * f32::from(sequence);
            answer.bytes[..4].copy_from_slice(&value.to_ne_bytes());
        }
    }
}

pub(crate) fn check_failure(failure: Failure) {
    let mut smc = m5_pro();
    assert!(smc.reading().is_none());
    smc.read().unwrap();
    let capacity = smc.fan_rpm.capacity();
    let storage = smc.fan_rpm.as_ptr();
    set_read(&mut smc, 1, Some(failure));

    crate::diagnostics::error(format_args!("SMC diagnostic warm-up"));
    let calls = allocator_calls_in(|| {
        for _attempt in 0..16 {
            let error = smc.read().unwrap_err();
            assert_eq!(error, failure.expected());
            crate::diagnostics::error(format_args!("collector: SMC read failed: {error:?}"));
            assert!(smc.reading().is_none());
        }
    });

    assert_eq!(calls, 0);
    assert_eq!(smc.fan_rpm.capacity(), capacity);
    assert_eq!(smc.fan_rpm.as_ptr(), storage);
    assert_eq!(smc.power_watts.to_bits(), 10.0_f32.to_bits());
    assert_eq!(smc.fan_rpm, [10.0]);
    set_read(&mut smc, 2, None);
    smc.read().unwrap();
    assert_eq!(
        smc.reading().unwrap().power_watts.to_bits(),
        20.0_f32.to_bits()
    );
    println!("SMC {failure:?}: repeated_attempts=16 rust_allocator_calls={calls}");
}

macro_rules! failure_test {
    ($name:ident, $failure:ident) => {
        #[test]
        fn $name() {
            // Goal: every returned error hides partial state without allocating or reopening.
            // Method: repeat native-answer failures after one success, then read fresh answers.
            crate::tests::with_diagnostics(
                concat!("smc::tests::read_tests::", stringify!($name)),
                || check_failure(Failure::$failure),
            );
        }
    };
}

failure_test!(isolates_native_failure_without_allocating, Native);
failure_test!(isolates_short_answer_without_allocating, Short);
failure_test!(isolates_missing_key_without_allocating, Missing);
failure_test!(isolates_controller_refusal_without_allocating, Refused);
