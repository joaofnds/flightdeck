//! Borrowed, typed access to machine facts. Saved minutes and the Store's readings decode before
//! this boundary and never become text cells, so both run the same metric reducers.
use persistence::{Error, Result};

use crate::live_raw::Cell;

pub(crate) struct View<'a>(pub(crate) crate::live_raw::View<'a>);

pub(crate) enum Key<'a> {
    Integer(u64),
    Text(&'a str),
}

impl PartialEq for Key<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Key<'_> {}

impl PartialOrd for Key<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Key<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Saved component keys were ordered as decimal text. Keep that reduction order
        // without allocating numeric strings.
        let mut left = [0; 20];
        let mut right = [0; 20];
        self.bytes(&mut left).cmp(other.bytes(&mut right))
    }
}

impl Key<'_> {
    fn bytes<'a>(&'a self, digits: &'a mut [u8; 20]) -> &'a [u8] {
        match self {
            Self::Text(value) => value.as_bytes(),
            Self::Integer(value) => {
                let mut value = *value;
                let mut start = digits.len();
                for _ in 0..20 {
                    start -= 1;
                    digits[start] = u8::try_from(value % 10)
                        .unwrap_or_else(|_| unreachable!("decimal digit"))
                        + b'0';
                    value /= 10;
                    if value == 0 {
                        return &digits[start..];
                    }
                }
                unreachable!("u64 decimal width")
            }
        }
    }
}

/// The reading a reducer reads, and the one before it that its rates take their change from.
pub(crate) struct Sources<'a> {
    pub current: &'a crate::store::Reading<'a>,
    pub previous: Option<&'a crate::store::Reading<'a>>,
}

impl<'a> Sources<'a> {
    pub fn get(
        &self,
        observation: &crate::session::Observation,
        name: &'static str,
    ) -> Result<View<'a>> {
        self.optional(observation, name)?
            .ok_or(Error::Bound("missing required recorded table rows"))
    }

    pub fn optional(
        &self,
        observation: &crate::session::Observation,
        name: &'static str,
    ) -> Result<Option<View<'a>>> {
        let reading = if self.current.tick.publication == observation.publication {
            self.current
        } else {
            self.previous.ok_or(Error::Bound("metric predecessor"))?
        };
        let view = crate::live_raw::View::machine(reading, name)?;
        Ok((view.len() > 0).then_some(View(view)))
    }
}

impl View<'_> {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn rows(&self) -> std::ops::Range<usize> {
        0..self.len()
    }

    pub fn holds(&self, name: &str) -> bool {
        self.0.holds(name)
    }

    pub fn field(&self, row: usize, name: &str) -> Result<Option<&str>> {
        match self.0.cell(row, name)? {
            Cell::Null => Ok(None),
            Cell::Text(text) => Ok(Some(text)),
            Cell::Bool(true) => Ok(Some("true")),
            Cell::Bool(false) => Ok(Some("false")),
            _ => Err(Error::Bound("text metric cell")),
        }
    }

    pub fn integer(&self, row: usize, name: &str) -> Result<Option<u64>> {
        match self.0.cell(row, name)? {
            Cell::Null => Ok(None),
            Cell::Unsigned(value) => Ok(Some(value)),
            _ => Err(Error::Bound("integer metric cell")),
        }
    }

    pub fn key(&self, row: usize, name: &str) -> Result<Option<Key<'_>>> {
        if name == "name" {
            return Ok(self.field(row, name)?.map(Key::Text));
        }

        Ok(self.integer(row, name)?.map(Key::Integer))
    }

    pub fn signed(&self, row: usize, name: &str) -> Result<Option<i64>> {
        match self.0.cell(row, name)? {
            Cell::Null => Ok(None),
            Cell::Signed(value) => Ok(Some(value)),
            _ => Err(Error::Bound("signed metric cell")),
        }
    }

    pub fn number(&self, row: usize, name: &str) -> Result<Option<f64>> {
        self.0.cell(row, name)?.number()
    }

    pub fn disks(&self, snapshot: &mut collector::disks::DiskSnapshot) -> bool {
        self.0.disks(snapshot)
    }
}
