//! Conservative retained-capacity charge for the fixed scalar point shape. Point objects grow
//! monotonically from JSON literals, so an `IndexMap` backing uses at most twice their entries
//! plus a hash index. A `BTreeMap` backing in the CI-pinned Rust 1.98 uses eleven slots and twelve
//! child pointers per node. Charge a full node per entry, which exceeds either backing's spare
//! storage. This is an upper charge, not a heap census; strings/arrays use actual capacities.
use persistence::{Error, Result};
use serde_json::Value;

pub(super) const POINT_BYTES_MAX: usize = 8 * 1024 * 1024;
const PENDING_MAX: usize = 256;
const VALUES_MAX: usize = 2048;
const NODE_BYTES: usize = 11 * size_of::<(String, Value)>() + 12 * size_of::<usize>() + 32;

pub(super) fn charge(point: &Value) -> Result<usize> {
    let mut pending = [None; PENDING_MAX];
    pending[0] = Some(point);
    let mut count = 1;
    let mut bytes = 0_usize;
    for _value in 0..VALUES_MAX {
        if count == 0 {
            return Ok(bytes);
        }

        count -= 1;
        let value = pending[count]
            .take()
            .ok_or(Error::Bound("point accounting stack"))?;
        match value {
            Value::String(text) => bytes += text.capacity(),
            Value::Array(values) => {
                bytes += values.capacity() * size_of::<Value>();
                for value in values {
                    push(&mut pending, &mut count, value)?;
                }
            }
            Value::Object(values) => {
                bytes += values.len() * NODE_BYTES;
                for (key, value) in values {
                    bytes += key.capacity();
                    push(&mut pending, &mut count, value)?;
                }
            }
            _ => {}
        }
        if bytes > POINT_BYTES_MAX {
            return Err(Error::Bound("live scalar point bytes"));
        }
    }
    Err(Error::Bound("live scalar point values"))
}

fn push<'a>(
    pending: &mut [Option<&'a Value>; PENDING_MAX],
    count: &mut usize,
    value: &'a Value,
) -> Result<()> {
    let slot = pending
        .get_mut(*count)
        .ok_or(Error::Bound("point accounting stack"))?;
    *slot = Some(value);
    *count += 1;
    Ok(())
}
