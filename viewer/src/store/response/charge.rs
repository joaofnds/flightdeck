use persistence::{Error, Result};
use serde_json::Value;

// Typed response formatters construct every container; captured bytes remain string leaves.
// This bounds formatter schema nesting independently of the number of processes or points.
const NESTING_MAX: usize = 64;
const VALUES_MAX: usize = 1024 * 1024;
const BYTES_MAX: usize = 64 * 1024 * 1024;
const NODE_BYTES: usize = 11 * size_of::<(String, Value)>() + 12 * size_of::<usize>() + 32;

/// JSON maps grow from literals in these responses. The pinned `BTreeMap` has eleven slots per
/// node and at least five occupied slots outside its root. The desktop's `IndexMap` backing is
/// also covered by twice the entries plus its hash/index storage; arrays and strings use capacity.
pub(crate) fn json_charge(value: &Value) -> Result<usize> {
    let mut pending = [const { None }; NESTING_MAX];
    let mut depth = 0;
    let mut bytes = 0_usize;
    let mut current = Some(value);
    for _value in 0..VALUES_MAX {
        let Some(value) = current else {
            return Ok(bytes);
        };

        let mut children = None;
        match value {
            Value::String(text) => bytes += text.capacity(),
            Value::Array(values) => {
                bytes += values.capacity() * size_of::<Value>();
                children = Some(Children::Array(values.iter()));
            }
            Value::Object(values) => {
                bytes += object_charge(values.len());
                children = Some(Children::Object(values.iter()));
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        if children.is_some() {
            json_charge_push(&mut pending, &mut depth, &mut children)?;
        }
        current = json_charge_next(&mut pending, &mut depth, &mut bytes);
        if bytes > BYTES_MAX {
            return Err(Error::Bound("Store JSON retained bytes"));
        }
    }

    if current.is_some() {
        return Err(Error::Bound("Store JSON accounting values"));
    }

    Ok(bytes)
}

enum Children<'value> {
    Array(std::slice::Iter<'value, Value>),
    Object(serde_json::map::Iter<'value>),
}

const _: () = assert!(size_of::<[Option<Children<'static>>; NESTING_MAX]>() < 16 * 1024);

impl<'value> Children<'value> {
    fn next_value(&mut self) -> Option<(&'value Value, usize)> {
        match self {
            Self::Array(values) => values.next().map(|value| (value, 0)),
            Self::Object(values) => values.next().map(|(key, value)| (value, key.capacity())),
        }
    }
}

fn object_charge(count: usize) -> usize {
    let nodes = if count == 0 {
        0
    } else {
        if count <= 11 {
            1
        } else {
            1 + count.div_ceil(5)
        }
    };
    let indexed = 2 * count * (size_of::<(String, Value)>() + 3 * size_of::<usize>());

    (nodes * NODE_BYTES).max(indexed)
}

fn json_charge_push<'value>(
    pending: &mut [Option<Children<'value>>; NESTING_MAX],
    depth: &mut usize,
    children: &mut Option<Children<'value>>,
) -> Result<()> {
    let slot = pending
        .get_mut(*depth)
        .ok_or(Error::Bound("Store JSON accounting depth"))?;
    *slot = children.take();
    *depth += 1;

    Ok(())
}

fn json_charge_next<'value>(
    pending: &mut [Option<Children<'value>>; NESTING_MAX],
    depth: &mut usize,
    bytes: &mut usize,
) -> Option<&'value Value> {
    assert!(*depth <= NESTING_MAX);
    for _level in 0..NESTING_MAX {
        if *depth == 0 {
            return None;
        }
        let children = pending[*depth - 1]
            .as_mut()
            .unwrap_or_else(|| unreachable!("active JSON container"));
        if let Some((value, key_bytes)) = children.next_value() {
            *bytes += key_bytes;

            return Some(value);
        }
        *depth -= 1;
        pending[*depth] = None;
    }
    assert_eq!(*depth, 0);

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_arrays_charge_every_value_without_a_sibling_stack() {
        // Goal: breadth does not consume scratch space or hide any retained child capacity.
        // Method: charge 4,097 string siblings, wider than the former pending-values array.
        let values: Vec<_> = (0..4097)
            .map(|_| Value::String(String::from("retained")))
            .collect();
        let bytes = values.capacity() * size_of::<Value>()
            + values
                .iter()
                .map(|value| match value {
                    Value::String(text) => text.capacity(),
                    _ => unreachable!("fixture string"),
                })
                .sum::<usize>();
        let value = Value::Array(values);

        assert_eq!(json_charge(&value).unwrap(), bytes);
    }

    #[test]
    fn excessive_nesting_is_refused_at_the_operation_depth_bound() {
        // Goal: a schema that nests too deeply fails without unbounded scratch or recursion.
        // Method: accept exactly 64 nested containers, then add one more container.
        let mut value = Value::Null;
        for _level in 0..NESTING_MAX {
            value = Value::Array(vec![value]);
        }
        let accepted = json_charge(&value).unwrap();
        value = Value::Array(vec![value]);

        assert!(accepted > 0);
        assert!(matches!(
            json_charge(&value),
            Err(Error::Bound("Store JSON accounting depth"))
        ));
    }

    #[test]
    fn strings_and_arrays_charge_capacity_beyond_their_visible_lengths() {
        // Goal: spare capacity remains charged after changing the traversal representation.
        // Method: keep one short string in a partly empty array with explicit larger capacities.
        let mut text = String::with_capacity(257);
        text.push('x');
        let text_bytes = text.capacity();
        let mut values = Vec::with_capacity(17);
        values.push(Value::String(text));
        let expected = text_bytes + values.capacity() * size_of::<Value>();
        let value = Value::Array(values);

        assert_eq!(json_charge(&value).unwrap(), expected);
    }
}
