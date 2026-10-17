use crate::ring::ProgramBytes;

use super::native::CallResult;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    Refused(i32),
    Gone(i32),
    Unsupported(i32),
    Syscall(i32),
    Length(i32),
    TooLong,
    Terminator,
    EmbeddedNul,
    NotAbsolute,
    EmptyBasename,
}

/// Splits borrowed raw bytes; a bundle component must precede the executable basename.
pub fn split_program<'path>(
    path: &'path [u8],
    output: &mut Option<ProgramBytes<'path>>,
) -> Result<(), PathError> {
    *output = None;
    if path.is_empty() {
        return Err(PathError::EmptyBasename);
    }

    if path.len() > 4095 {
        return Err(PathError::TooLong);
    }

    if !path.starts_with(b"/") {
        return Err(PathError::NotAbsolute);
    }

    if path.contains(&0) {
        return Err(PathError::EmbeddedNul);
    }

    let basename_index = path
        .iter()
        .rposition(|byte| *byte == b'/')
        .map_or(0, |index| index + 1);
    if basename_index == path.len() {
        return Err(PathError::EmptyBasename);
    }

    let mut component_end = 0;
    let mut app_path = None;
    for component in path[..basename_index - 1].split(|byte| *byte == b'/') {
        component_end += component.len();
        if component.ends_with(b".app") {
            app_path = Some(&path[..component_end]);
            break;
        }
        component_end += 1;
    }

    *output = Some(ProgramBytes {
        executable_name: &path[basename_index..],
        app_path,
    });
    Ok(())
}

pub(super) fn read_path_result(scratch: &[u8; 4096], call: CallResult) -> Result<usize, PathError> {
    if call.returned == 0 {
        return Err(match call.errno {
            1 | 13 => PathError::Refused(call.errno),
            2 | 3 => PathError::Gone(call.errno),
            22 => PathError::Unsupported(call.errno),
            _ => PathError::Syscall(call.errno),
        });
    }

    let length = usize::try_from(call.returned).map_err(|_| PathError::Length(call.returned))?;
    if length >= scratch.len() {
        return Err(PathError::Length(call.returned));
    }

    if scratch[length] != 0 {
        return Err(PathError::Terminator);
    }

    if scratch[..length].contains(&0) {
        return Err(PathError::EmbeddedNul);
    }

    Ok(length)
}
