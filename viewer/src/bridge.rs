//! Only the trusted native client calls this ABI, serially, with valid live pointers.
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::{Cancellation, Session};

const RESPONSE_BYTES_MAX: usize = 8 * 1024 * 1024;

pub struct Handle {
    session: persistence::Result<Session>,
}

#[repr(C)]
pub struct Bytes {
    pointer: *mut u8,
    length: usize,
}

/// Opens an opaque session; errors are returned by its catalog request.
/// # Safety
/// `path` must address `length` readable bytes, UTF-8, with length at most 4096.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_open(path: *const u8, length: usize) -> *mut Handle {
    unsafe { open_input(path, length, Arc::new(Cancellation::default()), 0) }
}

unsafe fn open_input(
    path: *const u8,
    length: usize,
    control: Arc<Cancellation>,
    epoch: u64,
) -> *mut Handle {
    let session = if length > 4096 {
        Err(persistence::Error::Bound("recording path bytes"))
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(path, length) };
        match std::str::from_utf8(bytes) {
            Ok(path) => Session::open_with(Path::new(path), control, epoch),
            Err(_) => Err(persistence::Error::Bound("path encoding")),
        }
    };
    Box::into_raw(Box::new(Handle { session }))
}

#[unsafe(no_mangle)]
pub extern "C" fn fd_control_new() -> *mut Arc<Cancellation> {
    Box::into_raw(Box::new(Arc::new(Cancellation::default())))
}

/// # Safety
/// `control` must remain live across this call and may be shared between native threads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_control_epoch(control: *const Arc<Cancellation>) -> u64 {
    unsafe { &*control }.epoch()
}

/// # Safety
/// `control` must remain live across this call and may be shared between native threads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_control_cancel(control: *const Arc<Cancellation>) -> u32 {
    match unsafe { &*control }.cancel() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("flightdeck cancellation: {error}");
            1
        }
    }
}

/// # Safety
/// Consume the original control once, after its native callers have stopped using it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_control_free(control: *mut Arc<Cancellation>) {
    drop(unsafe { Box::from_raw(control) });
}

/// # Safety
/// Path rules match `fd_open`; `control` must be live and epoch captured at submission.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_open_controlled(
    path: *const u8,
    length: usize,
    control: *const Arc<Cancellation>,
    epoch: u64,
) -> *mut Handle {
    let control = Arc::clone(unsafe { &*control });
    unsafe { open_input(path, length, control, epoch) }
}

/// # Safety
/// `handle` must be exclusively owned by the serial native worker during this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_prepare(handle: *mut Handle, epoch: u64) -> Bytes {
    let handle = unsafe { &mut *handle };
    response(match &mut handle.session {
        Ok(session) => session
            .begin(epoch)
            .map(|()| json!(true))
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    })
}

/// # Safety
/// `handle` must be a live `fd_open` result, exclusively borrowed for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_catalog(handle: *mut Handle) -> Bytes {
    let handle = unsafe { &mut *handle };
    response(
        handle
            .session
            .as_ref()
            .map(Session::catalog)
            .map_err(ToString::to_string),
    )
}

/// # Safety
/// `handle` must be a live `fd_open` result, exclusively borrowed for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_select(handle: *mut Handle, second: u64, alternative: u32) -> Bytes {
    let handle = unsafe { &mut *handle };
    let result = match &mut handle.session {
        Ok(session) => usize::try_from(alternative)
            .map_err(|error| error.to_string())
            .and_then(|index| {
                session
                    .select(second, index)
                    .map_err(|error| error.to_string())
            }),
        Err(error) => Err(error.to_string()),
    };
    response(result)
}

/// # Safety
/// `handle` must be a live `fd_open` result, exclusively borrowed for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_page(handle: *mut Handle, table: u32, offset: u64) -> Bytes {
    let handle = unsafe { &mut *handle };
    let result = match &handle.session {
        Ok(session) => usize::try_from(table)
            .map_err(|error| error.to_string())
            .and_then(|index| {
                persistence::schema::TABLES
                    .get(index)
                    .ok_or_else(|| "unknown table".to_owned())
            })
            .and_then(|entry| {
                session
                    .page(entry.0.name(), offset)
                    .map_err(|error| error.to_string())
            }),
        Err(error) => Err(error.to_string()),
    };
    response(result)
}

/// # Safety
/// Consume each non-null handle once, after its last request completes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_close(handle: *mut Handle) {
    drop(unsafe { Box::from_raw(handle) });
}

/// # Safety
/// Consume each response once, with its original pointer and length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_bytes_free(bytes: Bytes) {
    let slice = std::ptr::slice_from_raw_parts_mut(bytes.pointer, bytes.length);
    drop(unsafe { Box::from_raw(slice) });
}

fn response(result: Result<Value, String>) -> Bytes {
    let envelope = match result {
        Ok(value) => json!({"value": value, "error": null}),
        Err(error) => json!({"value": null, "error": error}),
    };
    let mut bytes = envelope.to_string().into_bytes();
    if bytes.len() > RESPONSE_BYTES_MAX {
        bytes = b"{\"value\":null,\"error\":\"response exceeds 8 MiB\"}".to_vec();
    }
    let mut bytes = bytes.into_boxed_slice();
    let result = Bytes {
        pointer: bytes.as_mut_ptr(),
        length: bytes.len(),
    };
    std::mem::forget(bytes);
    result
}
