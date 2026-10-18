//! Only the trusted native client calls this ABI, serially, with valid live pointers.
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::{Cancellation, LiveSession, Session};

const RESPONSE_BYTES_MAX: usize = 8 * 1024 * 1024;

pub struct Handle {
    session: persistence::Result<ViewedSession>,
}

enum ViewedSession {
    Saved(Box<Session>),
    Live(Box<LiveSession>),
}

impl ViewedSession {
    fn begin(&mut self, epoch: u64) -> persistence::Result<()> {
        match self {
            Self::Saved(session) => session.begin(epoch),
            Self::Live(session) => session.begin(epoch),
        }
    }

    fn catalog(&self) -> Value {
        match self {
            Self::Saved(session) => session.catalog(),
            Self::Live(session) => session.catalog(),
        }
    }

    fn select(&mut self, second: u64, alternative: usize) -> persistence::Result<Value> {
        match self {
            Self::Saved(session) => session.select(second, alternative),
            Self::Live(session) => session.select(second, alternative),
        }
    }

    fn page(&self, name: &str, offset: u64) -> persistence::Result<Value> {
        match self {
            Self::Saved(session) => session.page(name, offset),
            Self::Live(session) => session.page(name, offset),
        }
    }
}

#[repr(C)]
pub struct Bytes {
    pointer: *mut u8,
    length: usize,
}

#[repr(C)]
pub struct AppKey {
    kind: u32,
    pointer: *const u8,
    length: usize,
}

/// Opens an opaque session; errors are returned by its catalog request.
/// # Safety
/// `path` must address `length` readable bytes, UTF-8, with length at most 4096.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_open(path: *const u8, length: usize) -> *mut Handle {
    unsafe { open_input(path, length, Arc::new(Cancellation::default()), 0, false) }
}

unsafe fn open_input(
    path: *const u8,
    length: usize,
    control: Arc<Cancellation>,
    epoch: u64,
    live: bool,
) -> *mut Handle {
    let session = if length > 4096 {
        Err(persistence::Error::Bound("recording path bytes"))
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(path, length) };
        match std::str::from_utf8(bytes) {
            Ok(path) => {
                if live {
                    LiveSession::open_with(Path::new(path), control, epoch)
                        .map(Box::new)
                        .map(ViewedSession::Live)
                } else {
                    Session::open_with(Path::new(path), control, epoch)
                        .map(Box::new)
                        .map(ViewedSession::Saved)
                }
            }
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
    unsafe { open_input(path, length, control, epoch, false) }
}

/// # Safety
/// Path and control requirements match `fd_open_controlled`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_open_live_controlled(
    path: *const u8,
    length: usize,
    control: *const Arc<Cancellation>,
    epoch: u64,
) -> *mut Handle {
    let control = Arc::clone(unsafe { &*control });
    unsafe { open_input(path, length, control, epoch, true) }
}

/// # Safety
/// `handle` must be exclusively owned by the serial native worker during this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_live_poll(handle: *mut Handle) -> Bytes {
    let handle = unsafe { &mut *handle };
    response(match &mut handle.session {
        Ok(ViewedSession::Live(session)) => session
            .poll()
            .map(|changed| json!(changed))
            .map_err(|error| error.to_string()),
        Ok(ViewedSession::Saved(_)) => Err("session has no live source".to_owned()),
        Err(error) => Err(error.to_string()),
    })
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
            .map(ViewedSession::catalog)
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
/// `handle` must be a live session result, exclusively borrowed for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_follow(handle: *mut Handle) -> Bytes {
    let handle = unsafe { &mut *handle };
    response(match &mut handle.session {
        Ok(ViewedSession::Live(session)) => {
            session.follow_latest().map_err(|error| error.to_string())
        }
        Ok(ViewedSession::Saved(_)) => Err("session has no live source".to_owned()),
        Err(error) => Err(error.to_string()),
    })
}

/// # Safety
/// `handle` must be a live session result, exclusively borrowed for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_apps(handle: *mut Handle, offset: u32) -> Bytes {
    let handle = unsafe { &*handle };
    response(match &handle.session {
        Ok(ViewedSession::Live(session)) => {
            session.apps_page(offset).map_err(|error| error.to_string())
        }
        Ok(ViewedSession::Saved(_)) => Err("session has no live App source".to_owned()),
        Err(error) => Err(error.to_string()),
    })
}

/// # Safety
/// `handle` must be a live session result, exclusively borrowed for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_cpu_detail(handle: *mut Handle) -> Bytes {
    let handle = unsafe { &*handle };
    response(match &handle.session {
        Ok(ViewedSession::Live(session)) => session.cpu_detail().map_err(|error| error.to_string()),
        Ok(ViewedSession::Saved(session)) => {
            session.cpu_detail().map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    })
}

/// # Safety
/// `handle` must be a live session result, exclusively borrowed for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_cpu_history(handle: *mut Handle, cpu_offset: u32) -> Bytes {
    let handle = unsafe { &mut *handle };
    response(match &mut handle.session {
        Ok(ViewedSession::Live(session)) => session
            .cpu_history(cpu_offset)
            .map_err(|error| error.to_string()),
        Ok(ViewedSession::Saved(session)) => session
            .cpu_history(cpu_offset)
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    })
}

/// # Safety
/// `handle` is exclusively borrowed. `key` addresses a live `AppKey` whose pointer
/// addresses its readable length of bytes for the call; no key bytes are retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_app_detail(
    handle: *mut Handle,
    key: *const AppKey,
    offset: u32,
) -> Bytes {
    unsafe {
        app_query(handle, key, |session, kind, bytes| {
            session.app_detail(kind, bytes, offset)
        })
    }
}

/// # Safety
/// `handle` is exclusively borrowed. `key` addresses a live `AppKey` whose pointer
/// addresses its readable length of bytes for the call; no key bytes are retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fd_app_history(handle: *mut Handle, key: *const AppKey) -> Bytes {
    unsafe { app_query(handle, key, LiveSession::app_history) }
}

unsafe fn app_query(
    handle: *mut Handle,
    key: *const AppKey,
    query: impl FnOnce(
        &LiveSession,
        crate::live_app_response::KeyKind,
        &[u8],
    ) -> persistence::Result<Value>,
) -> Bytes {
    let key = unsafe { &*key };
    if key.length == 0 {
        return response(Err("empty App key".to_owned()));
    }
    if key.length > collector::ring::PROGRAM_PATH_BYTES_MAX {
        return response(Err("App key exceeds captured-byte bound".to_owned()));
    }
    let kind = match key.kind {
        0 => crate::live_app_response::KeyKind::Bundle,
        1 => crate::live_app_response::KeyKind::Executable,
        _ => return response(Err("unknown App key kind".to_owned())),
    };
    let bytes = unsafe { std::slice::from_raw_parts(key.pointer, key.length) };
    let handle = unsafe { &*handle };

    response(match &handle.session {
        Ok(ViewedSession::Live(session)) => {
            query(session, kind, bytes).map_err(|error| error.to_string())
        }
        Ok(ViewedSession::Saved(_)) => Err("session has no live App source".to_owned()),
        Err(error) => Err(error.to_string()),
    })
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
        Ok(value) => {
            let mut envelope = json!({"error": null});
            envelope["value"] = value;

            envelope
        }
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn wraps_a_full_context_without_cloning_each_owned_element() {
        // Goal: ABI envelope construction does not allocate a copy of every owned element.
        // Method: encode 900 heap strings, count calls only during response, then decode it.
        let elements: Vec<Value> = (0..900)
            .map(|index| Value::String(format!("reading-{index}")))
            .collect();
        let payload = Value::Array(elements);
        let expected = json!({"value":payload,"error":null});

        let (bytes, calls) = crate::allocation_tests::allocations_in(|| response(Ok(payload)));
        let decoded = serde_json::from_slice::<Value>(unsafe {
            std::slice::from_raw_parts(bytes.pointer, bytes.length)
        });
        unsafe { fd_bytes_free(bytes) };

        assert_eq!(decoded.unwrap(), expected);
        assert!(calls <= 128, "owned response made {calls} allocations");
        eprintln!("owned ABI response: 900 elements, {calls} allocation calls");
    }

    pub(crate) fn response_value(value: Value) -> Value {
        let bytes = response(Ok(value));
        let decoded = serde_json::from_slice::<Value>(unsafe {
            std::slice::from_raw_parts(bytes.pointer, bytes.length)
        });
        unsafe { fd_bytes_free(bytes) };
        decoded.unwrap()
    }

    #[test]
    fn refuses_an_envelope_one_byte_over_the_literal_eight_mib_limit() {
        // Goal: oversized results return an explicit error instead of an oversized ABI buffer.
        // Method: pass a string whose full JSON envelope exceeds the literal cap by one byte.
        let value = Value::String("x".repeat(8 * 1024 * 1024 - 24));

        let bytes = response(Ok(value));
        let decoded = serde_json::from_slice::<Value>(unsafe {
            std::slice::from_raw_parts(bytes.pointer, bytes.length)
        });
        unsafe { fd_bytes_free(bytes) };

        assert_eq!(
            decoded.unwrap(),
            json!({
                "value":null, "error":"response exceeds 8 MiB"
            })
        );
    }

    #[test]
    fn delivers_an_envelope_at_the_literal_eight_mib_limit() {
        // Goal: the response guard permits a valid result exactly at its byte boundary.
        // Method: decode and free the actual ABI buffer for an exactly fitting JSON envelope.
        let value = Value::String("x".repeat(8 * 1024 * 1024 - 25));

        let bytes = response(Ok(value));
        let length = bytes.length;
        let decoded = serde_json::from_slice::<Value>(unsafe {
            std::slice::from_raw_parts(bytes.pointer, bytes.length)
        });
        unsafe { fd_bytes_free(bytes) };

        let decoded = decoded.unwrap();
        assert_eq!(length, 8 * 1024 * 1024);
        assert!(decoded["error"].is_null());
        assert_eq!(
            decoded["value"].as_str().unwrap().len(),
            8 * 1024 * 1024 - 25
        );
    }
}
