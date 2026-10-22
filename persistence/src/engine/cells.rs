use polars::prelude::{AnyValue, ChunkedArray, DataFrame, PolarsPhysicalType};

use crate::{Error, Result};

pub struct Row<'a> {
    frame: &'a DataFrame,
    index: usize,
}

pub trait FromCell: Sized {
    fn from_cell(value: AnyValue<'_>) -> Result<Self>;
}

impl<'a> Row<'a> {
    pub fn new(frame: &'a DataFrame, index: usize) -> Result<Self> {
        if index >= frame.height() {
            return Err(Error::Bound("query row index"));
        }

        Ok(Self { frame, index })
    }

    pub fn value(&self, index: usize) -> Result<AnyValue<'a>> {
        let column = self
            .frame
            .columns()
            .get(index)
            .ok_or(Error::Bound("query column index"))?;
        Ok(column.get(self.index)?)
    }

    pub fn get<T: FromCell>(&self, index: usize) -> Result<T> {
        T::from_cell(self.value(index)?)
    }
}

/// One column of a frame, read in the type the frame holds it in. The type is checked once for
/// the column, where `Row` converts each cell it reads, so a read of many rows reads them far
/// faster through these.
pub struct Cells<'a, T: PolarsPhysicalType> {
    cells: &'a ChunkedArray<T>,
}

impl<'a, T: PolarsPhysicalType> Cells<'a, T> {
    /// The cells of column `index` of `frame`, which must hold them as `T`. A column of
    /// another type is refused whole, even one whose values `T` could hold, since a table
    /// holding it was not written by the writer its reader was built for.
    pub fn new(frame: &'a DataFrame, index: usize) -> Result<Self> {
        let column = frame
            .columns()
            .get(index)
            .ok_or(Error::Bound("query column index"))?;
        // The engine refuses only a column of another type here, which `Row` reports as a
        // bound too.
        let cells = column
            .as_materialized_series()
            .unpack::<T>()
            .map_err(|_| Error::Bound("query column type"))?;

        Ok(Self { cells })
    }

    /// The cell of row `index`, which must hold a value.
    pub fn get(&self, index: usize) -> Result<T::Physical<'a>> {
        self.optional(index)?.ok_or(Error::Bound("query null cell"))
    }

    /// The cell of row `index`, or `None` where it holds no value.
    pub fn optional(&self, index: usize) -> Result<Option<T::Physical<'a>>> {
        // The engine panics on a row past the end.
        if index >= self.cells.len() {
            return Err(Error::Bound("query row index"));
        }

        Ok(self.cells.get(index))
    }
}

macro_rules! integer_cell {
    ($($kind:ty),+ $(,)?) => {
        $(impl FromCell for $kind {
            fn from_cell(value: AnyValue<'_>) -> Result<Self> {
                Self::try_from(integer(&value)?).map_err(|_| Error::Bound("query integer range"))
            }
        })+
    };
}

integer_cell!(u8, u16, u32, u64, i32, i64);

fn integer(value: &AnyValue<'_>) -> Result<i128> {
    match value {
        AnyValue::UInt8(value) => Ok(i128::from(*value)),
        AnyValue::UInt16(value) => Ok(i128::from(*value)),
        AnyValue::UInt32(value) => Ok(i128::from(*value)),
        AnyValue::UInt64(value) => Ok(i128::from(*value)),
        AnyValue::UInt128(value) => {
            i128::try_from(*value).map_err(|_| Error::Bound("query integer range"))
        }
        AnyValue::Int8(value) => Ok(i128::from(*value)),
        AnyValue::Int16(value) => Ok(i128::from(*value)),
        AnyValue::Int32(value) => Ok(i128::from(*value)),
        AnyValue::Int64(value) => Ok(i128::from(*value)),
        AnyValue::Int128(value) => Ok(*value),
        _ => Err(Error::Bound("query integer type")),
    }
}

impl FromCell for f32 {
    fn from_cell(value: AnyValue<'_>) -> Result<Self> {
        match value {
            AnyValue::Float32(value) => Ok(value),
            _ => Err(Error::Bound("query float32 type")),
        }
    }
}

impl FromCell for f64 {
    fn from_cell(value: AnyValue<'_>) -> Result<Self> {
        match value {
            AnyValue::Float32(value) => Ok(Self::from(value)),
            AnyValue::Float64(value) => Ok(value),
            _ => Err(Error::Bound("query float64 type")),
        }
    }
}

impl FromCell for bool {
    fn from_cell(value: AnyValue<'_>) -> Result<Self> {
        match value {
            AnyValue::Boolean(value) => Ok(value),
            _ => Err(Error::Bound("query boolean type")),
        }
    }
}

impl FromCell for String {
    fn from_cell(value: AnyValue<'_>) -> Result<Self> {
        match value {
            AnyValue::String(value) => Ok(value.to_owned()),
            AnyValue::StringOwned(value) => Ok(value.to_string()),
            _ => Err(Error::Bound("query string type")),
        }
    }
}

impl FromCell for Vec<u8> {
    fn from_cell(value: AnyValue<'_>) -> Result<Self> {
        match value {
            AnyValue::Binary(value) => Ok(value.to_owned()),
            AnyValue::BinaryOwned(value) => Ok(value),
            _ => Err(Error::Bound("query binary type")),
        }
    }
}

impl<T: FromCell> FromCell for Option<T> {
    fn from_cell(value: AnyValue<'_>) -> Result<Self> {
        match value {
            AnyValue::Null => Ok(None),
            value => Ok(Some(T::from_cell(value)?)),
        }
    }
}
