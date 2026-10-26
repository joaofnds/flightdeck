//! Borrowed, typed access to machine facts. Saved tables decode at this boundary; live
//! facts never become text cells. Both inputs run the same metric reducers and presentation.
use persistence::{Error, Result};

use crate::data::Table;
use crate::live_raw::{Cell, Raw};

pub(crate) enum View<'a> {
    Saved(&'a Table),
    Native(crate::live_raw::View<'a>),
}

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
        // without allocating numeric strings in the native adapter.
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

pub(crate) enum Sources<'a> {
    Saved(&'a crate::metrics::Tables),
    Native {
        current: &'a Raw,
        previous: Option<&'a Raw>,
    },
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
        match self {
            Self::Saved(tables) => Ok(tables
                .get(&(observation.window, name))
                .and_then(|table| table.get(&observation.publication))
                .map(View::Saved)),
            Self::Native { current, previous } => {
                let raw = if current.machine.ticks[0].publication == observation.publication {
                    *current
                } else {
                    previous.ok_or(Error::Bound("native metric predecessor"))?
                };
                let view = raw.view(name)?;
                Ok(Some(View::Native(view)))
            }
        }
    }

    pub fn holds(&self, observation: &crate::session::Observation, name: &str) -> bool {
        match self {
            Self::Saved(tables) => tables.contains_key(&(observation.window, name)),
            Self::Native { .. } => true,
        }
    }
}

impl View<'_> {
    pub fn len(&self) -> usize {
        match self {
            Self::Saved(table) => table.rows.len(),
            Self::Native(view) => view.len(),
        }
    }

    pub fn rows(&self) -> std::ops::Range<usize> {
        0..self.len()
    }

    pub fn holds(&self, name: &str) -> bool {
        match self {
            Self::Saved(table) => table.columns.iter().any(|column| column == name),
            Self::Native(view) => view.holds(name),
        }
    }

    pub fn field(&self, row: usize, name: &str) -> Result<Option<&str>> {
        match self {
            Self::Saved(table) => table.field(&table.rows[row], name),
            Self::Native(view) => match view.cell(row, name)? {
                Cell::Null => Ok(None),
                Cell::Text(text) => Ok(Some(text)),
                Cell::Bool(true) => Ok(Some("true")),
                Cell::Bool(false) => Ok(Some("false")),
                _ => Err(Error::Bound("text metric cell")),
            },
        }
    }

    pub fn integer(&self, row: usize, name: &str) -> Result<Option<u64>> {
        match self {
            Self::Saved(table) => table.integer(&table.rows[row], name),
            Self::Native(view) => match view.cell(row, name)? {
                Cell::Null => Ok(None),
                Cell::Unsigned(value) => Ok(Some(value)),
                _ => Err(Error::Bound("integer metric cell")),
            },
        }
    }

    pub fn key(&self, row: usize, name: &str) -> Result<Option<Key<'_>>> {
        match self {
            Self::Saved(table) => Ok(table.field(&table.rows[row], name)?.map(Key::Text)),
            Self::Native(_) => {
                if name == "name" {
                    return Ok(self.field(row, name)?.map(Key::Text));
                }
                Ok(self.integer(row, name)?.map(Key::Integer))
            }
        }
    }

    pub fn signed(&self, row: usize, name: &str) -> Result<Option<i64>> {
        match self {
            Self::Saved(table) => table
                .field(&table.rows[row], name)?
                .map(str::parse)
                .transpose()
                .map_err(|_| Error::Bound("signed metric cell")),
            Self::Native(view) => match view.cell(row, name)? {
                Cell::Null => Ok(None),
                Cell::Signed(value) => Ok(Some(value)),
                _ => Err(Error::Bound("signed metric cell")),
            },
        }
    }

    pub fn number(&self, row: usize, name: &str) -> Result<Option<f64>> {
        match self {
            Self::Saved(table) => table.number(&table.rows[row], name),
            Self::Native(view) => view.cell(row, name)?.number(),
        }
    }

    pub fn fan_rpm(&self, row: usize) -> Result<Option<f64>> {
        match self {
            Self::Saved(table) => table
                .field(&table.rows[row], "rpm")?
                .map(str::parse::<f64>)
                .transpose()
                .map_err(|_| Error::Bound("number")),
            Self::Native(view) => view.cell(row, "rpm")?.number(),
        }
    }

    pub fn disks(&self, row: usize, snapshot: &mut collector::disks::DiskSnapshot) -> Result<bool> {
        match self {
            Self::Saved(table) => {
                if !self.holds(persistence::disks::FIELD) {
                    return Ok(false);
                }
                let Some(text) = table.field(&table.rows[row], persistence::disks::FIELD)? else {
                    return Ok(false);
                };
                persistence::disks::decode(text, snapshot)?;
                Ok(true)
            }
            Self::Native(view) => Ok(view.disks(snapshot)),
        }
    }
}
