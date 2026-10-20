//! One bounded optional tick cell using the collector's exact physical-disk word codec.
use crate::engine::{Engine, Row};
use collector::disks::{DiskSnapshot, encoding};
use polars::prelude::{AnyValue, DataType};

use crate::schema::TICK_COLUMNS_MAX;
use crate::{Error, Result};

const HEX: &[u8; 16] = b"0123456789abcdef";

pub const FIELD: &str = "physical_disks";
pub const HEX_BYTES_MAX: usize = encoding::WORDS_MAX * 16;
const _: () = assert!(HEX_BYTES_MAX == 3904);

pub fn encode(snapshot: &DiskSnapshot) -> Result<String> {
    let mut words = [0; encoding::WORDS_MAX];
    let count = encoding::encode(snapshot, &mut words)
        .map_err(|_| Error::Bound("physical disk snapshot"))?;
    let mut text = String::with_capacity(count * 16);
    for word in &words[..count] {
        for byte in word.to_le_bytes() {
            text.push(char::from(HEX[usize::from(byte >> 4)]));
            text.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }

    assert!(text.len() <= HEX_BYTES_MAX);
    Ok(text)
}

pub fn decode(text: &str, snapshot: &mut DiskSnapshot) -> Result<()> {
    *snapshot = DiskSnapshot::default();
    if text.len() > HEX_BYTES_MAX {
        return Err(Error::Bound("physical disk text size"));
    }

    if !text.len().is_multiple_of(16) {
        return Err(Error::Bound("physical disk text extent"));
    }

    let mut words = [0; encoding::WORDS_MAX];
    for (index, chunk) in text.as_bytes().as_chunks::<16>().0.iter().enumerate() {
        let mut bytes = [0; 8];
        for (byte, pair) in bytes.iter_mut().zip(chunk.as_chunks::<2>().0) {
            *byte = decode_digit(pair[0])? * 16 + decode_digit(pair[1])?;
        }
        words[index] = u64::from_le_bytes(bytes);
    }

    if encoding::decode(&words[..text.len() / 16], snapshot).is_err() {
        *snapshot = DiskSnapshot::default();
        return Err(Error::Bound("physical disk codec"));
    }

    Ok(())
}

fn decode_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(Error::Bound("physical disk hex digit")),
    }
}

pub(crate) fn normalize(connection: &Engine, source: &str) -> Result<String> {
    let schema = connection.schema(source)?;
    if schema.len() > TICK_COLUMNS_MAX {
        return Err(Error::Bound("tick column count"));
    }

    let mut present = false;
    for (name, data_type) in schema.iter() {
        if name.eq_ignore_ascii_case(FIELD) {
            if name.as_str() != FIELD {
                return Err(Error::Bound("physical disk field spelling"));
            }

            if *data_type != DataType::String {
                return Err(Error::Bound("physical disk storage type"));
            }

            present = true;
        }
    }

    if present {
        normalize_validate(connection, source)?;
        return Ok(source.to_owned());
    }

    if schema.len() + 1 > TICK_COLUMNS_MAX {
        return Err(Error::Bound("normalized tick column count"));
    }

    Ok(format!("(SELECT *, NULL::VARCHAR {FIELD} FROM {source})"))
}

fn normalize_validate(connection: &Engine, source: &str) -> Result<()> {
    let frame = connection.query(&format!(
        "SELECT {FIELD}, publication FROM {source} LIMIT 62"
    ))?;
    if frame.height() > 61 {
        return Err(Error::Bound("physical disk tick rows"));
    }

    let mut snapshot = DiskSnapshot::default();
    for index in 0..frame.height() {
        let row = Row::new(&frame, index)?;
        let value = row.value(0)?;
        let text = match &value {
            AnyValue::Null => continue,
            AnyValue::String(text) => *text,
            AnyValue::StringOwned(text) => text.as_str(),
            _ => return Err(Error::Bound("physical disk storage value")),
        };
        if row.get::<Option<u64>>(1)?.is_none() {
            return Err(Error::Bound("physical disk observation"));
        }

        decode(text, &mut snapshot)?;
    }

    Ok(())
}
