//! Bounded acquisition primitives; callers publish only verified source observations.
use crate::ring::IdentityUnavailable;

mod acquisition;
mod exit;
mod native;
mod path;
mod readiness;
pub mod sweep;

pub use acquisition::{Acquisition, acquire};
pub use exit::{ExitAcquisition, acquire_exit};
pub use native::{read_creation, read_path};
pub use path::{PathError, split_program};
pub use readiness::{Readiness, ReadinessError, acquire_readiness};

#[cfg(test)]
mod acquisition_tests;
#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod path_tests;
#[cfg(test)]
mod readiness_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreationError {
    NotRead,
    Refused(i32),
    Gone(i32),
    Unsupported(i32),
    Syscall(i32),
    Size(i32),
}

fn creation_reason(error: CreationError) -> IdentityUnavailable {
    match error {
        CreationError::Refused(_) => IdentityUnavailable::Refused,
        CreationError::Gone(_) => IdentityUnavailable::Gone,
        CreationError::Unsupported(_) | CreationError::Syscall(_) => {
            IdentityUnavailable::Unsupported
        }
        CreationError::Size(_) | CreationError::NotRead => IdentityUnavailable::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::CreationError;
    use super::native::{CallResult, ProcUniqidentifierinfo, read_creation_result};
    use super::path::read_path_result;
    use crate::ring::ProcessCreation;

    #[test]
    fn accepts_raw_path_bytes_and_excludes_the_terminator() {
        // Goal: valid raw executable bytes never pass through UTF-8 conversion.
        // Method: decode a positive native length with stale errno and compare bytes.
        let mut scratch = [0xff; 4096];
        scratch[..5].copy_from_slice(b"/\xff\\\x01\0");

        assert_eq!(
            Ok(4),
            read_path_result(
                &scratch,
                CallResult {
                    returned: 4,
                    errno: 1
                }
            )
        );
    }

    #[test]
    fn accepts_an_exact_creation_record_despite_stale_errno() {
        // Goal: successful flavor17 does not inherit a prior syscall's errno.
        // Method: decode an exact native record with EPERM left in the errno snapshot.
        let raw = ProcUniqidentifierinfo {
            p_uniqueid: u64::MAX,
            p_idversion: i32::MIN,
            ..ProcUniqidentifierinfo::EMPTY
        };
        let mut output = Err(CreationError::NotRead);

        read_creation_result(
            &raw,
            CallResult {
                returned: 56,
                errno: 1,
            },
            &mut output,
        );

        assert_eq!(
            Ok(ProcessCreation {
                unique_id: u64::MAX,
                pid_version: i32::MIN
            }),
            output
        );
    }
    #[test]
    fn classifies_a_refused_creation_read() {
        // Goal: a refused creation remains distinct from unsupported or missing process data.
        // Method: decode the wrapper's zero-return EPERM contract.
        let raw = ProcUniqidentifierinfo::EMPTY;
        let mut output = Err(CreationError::NotRead);

        read_creation_result(
            &raw,
            CallResult {
                returned: 0,
                errno: 1,
            },
            &mut output,
        );

        assert_eq!(Err(CreationError::Refused(1)), output);
    }

    macro_rules! creation_failures {
        ($($name:ident, $returned:expr, $errno:expr, $expected:expr;)*) => {$(
            #[test]
            fn $name() {
                // Goal: unavailable or wrong-width native records never become a creation.
                // Method: decode one fixed syscall-result case against an initialized output.
                let raw = ProcUniqidentifierinfo::EMPTY;
                let mut output = Ok(ProcessCreation { unique_id: 7, pid_version: 8 });

                read_creation_result(&raw, CallResult { returned: $returned, errno: $errno },
                    &mut output);

                assert_eq!(Err($expected), output);
            }
        )*};
    }

    creation_failures! {
        refuses_a_short_creation_record, 55, 0, CreationError::Size(55);
        refuses_an_oversized_creation_record, 57, 0, CreationError::Size(57);
        refuses_a_negative_creation_return, -1, 14, CreationError::Size(-1);
        classifies_access_denied_creation, 0, 13, CreationError::Refused(13);
        classifies_a_missing_creation, 0, 3, CreationError::Gone(3);
        classifies_unsupported_creation_flavor, 0, 22, CreationError::Unsupported(22);
        preserves_an_unexpected_creation_errno, 0, 14, CreationError::Syscall(14);
        preserves_a_zero_creation_errno, 0, 0, CreationError::Syscall(0);
    }
}
