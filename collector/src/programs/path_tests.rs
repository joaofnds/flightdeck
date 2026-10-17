use super::native::CallResult;
use super::path::read_path_result;
use super::{PathError, split_program};

macro_rules! accepted_lengths {
    ($($name:ident, $length:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: native strlen bounds preserve every permitted raw byte.
            // Method: decode one listed length with its exact NUL position and stale errno.
            let length: usize = $length;
            let mut scratch = [b'x'; 4096];
            scratch[length] = 0;

            assert_eq!(Ok(length), read_path_result(&scratch, CallResult {
                returned: i32::try_from(length).unwrap(), errno: 14,
            }));
        }
    )*};
}

accepted_lengths! {
    accepts_the_minimum_native_path_length, 1;
    accepts_the_maximum_native_path_length, 4095;
}

macro_rules! refused_lengths {
    ($($name:ident, $length:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: a fixed buffer never makes an out-of-range native length look complete.
            // Method: decode one listed invalid length against a nonterminated buffer.
            let scratch = [b'x'; 4096];

            assert_eq!(Err(PathError::Length($length)), read_path_result(&scratch, CallResult {
                returned: $length, errno: 0,
            }));
        }
    )*};
}

refused_lengths! {
    rejects_a_negative_native_path_length, -1;
    rejects_a_native_path_length_that_fills_the_buffer, 4096;
    rejects_the_largest_i32_native_path_length, i32::MAX;
}

#[test]
fn rejects_a_missing_native_path_terminator() {
    // Goal: a fixed buffer never makes an unterminated path look complete.
    // Method: decode a positive length with no NUL at its reported end.
    let scratch = [b'x'; 4096];

    assert_eq!(
        Err(PathError::Terminator),
        read_path_result(
            &scratch,
            CallResult {
                returned: 3,
                errno: 0,
            }
        )
    );
}

#[test]
fn rejects_an_early_native_path_terminator() {
    // Goal: a terminated path with an embedded NUL cannot hide its incomplete byte range.
    // Method: decode a positive length with both an exact and an early terminator.
    let mut scratch = [b'x'; 4096];
    scratch[3] = 0;
    scratch[1] = 0;
    assert_eq!(
        Err(PathError::EmbeddedNul),
        read_path_result(
            &scratch,
            CallResult {
                returned: 3,
                errno: 0,
            }
        )
    );
}

macro_rules! zero_returns {
    ($($name:ident, $errno:expr, $expected:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: libproc's zero failure return preserves errno independently of numeric reads.
            // Method: decode one listed zero-return error against a fixed scratch buffer.
            let scratch = [0; 4096];

            assert_eq!(Err($expected),
                read_path_result(&scratch, CallResult { returned: 0, errno: $errno }));
        }
    )*};
}

zero_returns! {
    decodes_permission_denied_for_a_zero_path_return, 1, PathError::Refused(1);
    decodes_access_denied_for_a_zero_path_return, 13, PathError::Refused(13);
    decodes_missing_file_for_a_zero_path_return, 2, PathError::Gone(2);
    decodes_missing_process_for_a_zero_path_return, 3, PathError::Gone(3);
    decodes_unsupported_for_a_zero_path_return, 22, PathError::Unsupported(22);
    preserves_unexpected_errno_for_a_zero_path_return, 14, PathError::Syscall(14);
    preserves_zero_errno_for_a_zero_path_return, 0, PathError::Syscall(0);
}

macro_rules! borrowed_programs {
    ($($name:ident, $path:expr, $executable:expr, $app:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: bundle components and raw executable bytes produce borrowed exact slices.
            // Method: split one listed path and validate the resulting executable and app bytes.
            let mut output = None;

            assert_eq!(Ok(()), split_program($path, &mut output));

            let program = output.unwrap();
            assert_eq!($executable, program.executable_name);
            assert_eq!($app, program.app_path);
            assert_eq!(Ok(()), program.validate());
        }
    )*};
}

borrowed_programs! {
    splits_the_outermost_nested_bundle_and_raw_executable,
        b"/A.app/Contents/B.app/\xff\\\x01", b"\xff\\\x01", Some(&b"/A.app"[..]);
    splits_a_repeated_app_suffix_component,
        b"/A.app.app/Contents/run", b"run", Some(&b"/A.app.app"[..]);
    ignores_a_false_app_suffix, b"/A.application/run", b"run", None;
    ignores_an_app_suffix_on_the_executable, b"/bin/tool.app", b"tool.app", None;
    splits_an_executable_without_an_app, b"/x", b"x", None;
}

macro_rules! invalid_paths {
    ($($name:ident, $path:expr, $expected:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: an invalid byte path never leaves a previous candidate in caller storage.
            // Method: replace existing valid output with one listed structural failure.
            let mut output = None;
            split_program(b"/old", &mut output).unwrap();

            assert_eq!(Err($expected), split_program($path, &mut output));

            assert!(output.is_none());
        }
    )*};
}

invalid_paths! {
    refuses_an_empty_borrowed_path_and_clears_output, b"", PathError::EmptyBasename;
    refuses_a_root_only_borrowed_path_and_clears_output, b"/", PathError::EmptyBasename;
    refuses_a_trailing_slash_and_clears_output, b"/a/", PathError::EmptyBasename;
    refuses_a_relative_borrowed_path_and_clears_output, b"relative", PathError::NotAbsolute;
    refuses_a_borrowed_embedded_nul_and_clears_output, b"/a\0b", PathError::EmbeddedNul;
}

#[test]
fn refuses_an_oversized_borrowed_path() {
    // Goal: borrowed paths obey the same fixed maximum byte bound as native path reads.
    // Method: split a 4096-byte path and check that no output appears.
    let mut output = None;
    assert_eq!(
        Err(PathError::TooLong),
        split_program(&[b'x'; 4096], &mut output)
    );
    assert!(output.is_none());
}
