use polars::prelude::{AnyValue, DataFrame};

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
