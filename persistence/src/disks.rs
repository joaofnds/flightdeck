//! One bounded optional tick cell using the collector's exact physical-disk word codec.
use collector::disks::{DiskSnapshot, encoding};
use duckdb::{Connection, types::ValueRef};

use crate::{Error, Result};

const HEX: &[u8; 16] = b"0123456789abcdef";

pub const FIELD: &str = "physical_disks";
pub const HEX_BYTES_MAX: usize = encoding::WORDS_MAX * 16;
const _: () = assert!(HEX_BYTES_MAX == 3904);
const _: () = assert!(19 + crate::schema::BATTERY_FIELDS.len() + 1 == 32);

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
    for (index, chunk) in text.as_bytes().chunks_exact(16).enumerate() {
        let mut bytes = [0; 8];
        for (byte, pair) in bytes.iter_mut().zip(chunk.chunks_exact(2)) {
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

pub(crate) fn normalize(connection: &Connection, source: &str) -> Result<String> {
    let mut statement = connection.prepare(&format!("DESCRIBE SELECT * FROM {source}"))?;
    let mut rows = statement.query([])?;
    let mut present = false;
    for count in 0..=32 {
        let Some(row) = rows.next()? else {
            if present {
                normalize_validate(connection, source)?;
                return Ok(source.to_owned());
            }

            if count == 32 {
                return Err(Error::Bound("normalized tick column count"));
            }

            return Ok(format!("(SELECT *, NULL::VARCHAR {FIELD} FROM {source})"));
        };

        if count == 32 {
            return Err(Error::Bound("tick column count"));
        }

        let ValueRef::Text(name) = row.get_ref(0)? else {
            return Err(Error::Bound("tick column metadata"));
        };

        if name.eq_ignore_ascii_case(FIELD.as_bytes()) {
            if name != FIELD.as_bytes() {
                return Err(Error::Bound("physical disk field spelling"));
            }

            if row.get_ref(1)? != ValueRef::Text(b"VARCHAR") {
                return Err(Error::Bound("physical disk storage type"));
            }

            present = true;
        }
    }

    Err(Error::Bound("tick column count"))
}

fn normalize_validate(connection: &Connection, source: &str) -> Result<()> {
    let mut statement = connection.prepare(&format!(
        "SELECT {FIELD}, publication FROM {source} LIMIT 62"
    ))?;
    let mut rows = statement.query([])?;
    let mut snapshot = DiskSnapshot::default();
    for index in 0..=61 {
        let Some(row) = rows.next()? else {
            return Ok(());
        };

        if index == 61 {
            return Err(Error::Bound("physical disk tick rows"));
        }

        match row.get_ref(0)? {
            ValueRef::Null => {}
            ValueRef::Text(text) => {
                if row.get::<_, Option<u64>>(1)?.is_none() {
                    return Err(Error::Bound("physical disk observation"));
                }

                let text = std::str::from_utf8(text)
                    .map_err(|_| Error::Bound("physical disk text encoding"))?;
                decode(text, &mut snapshot)?;
            }
            _ => return Err(Error::Bound("physical disk storage value")),
        }
    }

    Err(Error::Bound("physical disk tick rows"))
}
