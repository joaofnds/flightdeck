use std::io::Read;

use polars_parquet_format::thrift::protocol::{
    ReadThrift, TCompactInputProtocol, TFieldIdentifier, TInputProtocol, TListIdentifier,
    TMapIdentifier, TMessageIdentifier, TSetIdentifier, TStructIdentifier, TType,
};
use polars_parquet_format::thrift::{Error, ProtocolError, ProtocolErrorKind, Result};

const CONTAINER_ITEMS_MAX: u32 = 16_384;
const STRUCT_DEPTH_MAX: usize = 16;
const VECTOR_BYTES_MAX: usize = 8 * 1024 * 1024;
const CONTEXT_DEPTH_MAX: usize = 48;

const _: () = {
    assert!(STRUCT_DEPTH_MAX < CONTEXT_DEPTH_MAX);
};

pub(super) struct BoundedProtocol<R: Read> {
    native: TCompactInputProtocol<R>,
    vector_bytes_left: usize,
    struct_depth: usize,
    contexts: [Context; CONTEXT_DEPTH_MAX],
    context_depth: usize,
}

#[derive(Clone, Copy)]
enum Context {
    Empty,
    Struct(Option<TType>),
    Sequence {
        kind: TType,
        element_type: TType,
        remaining: u32,
    },
    Map {
        key_type: TType,
        value_type: TType,
        remaining: u32,
        key_next: bool,
    },
}

impl<R: Read> BoundedProtocol<R> {
    pub(super) fn new(reader: R, byte_budget: usize) -> Self {
        Self {
            native: TCompactInputProtocol::new(reader, byte_budget),
            vector_bytes_left: VECTOR_BYTES_MAX,
            struct_depth: 0,
            contexts: [Context::Empty; CONTEXT_DEPTH_MAX],
            context_depth: 0,
        }
    }

    fn read_value(&mut self, kind: TType) -> Result<()> {
        if self.context_depth == 0 {
            return Ok(());
        }

        match &mut self.contexts[self.context_depth - 1] {
            Context::Struct(field_type) => {
                if field_type.take() != Some(kind) {
                    return Err(wire_type());
                }
            }
            Context::Sequence {
                element_type,
                remaining,
                ..
            } => {
                if *element_type != kind {
                    return Err(wire_type());
                }

                *remaining = remaining.checked_sub(1).ok_or_else(wire_type)?;
            }
            Context::Map {
                key_type,
                value_type,
                remaining,
                key_next,
            } => {
                let expected = if *key_next { *key_type } else { *value_type };
                if expected != kind {
                    return Err(wire_type());
                }

                if *remaining == 0 {
                    return Err(wire_type());
                }

                if *key_next {
                    *key_next = false;
                } else {
                    *remaining -= 1;
                    *key_next = true;
                }
            }
            Context::Empty => return Err(wire_type()),
        }

        Ok(())
    }

    fn push_context(&mut self, context: Context) -> Result<()> {
        assert!(self.context_depth <= CONTEXT_DEPTH_MAX);
        if self.context_depth == CONTEXT_DEPTH_MAX {
            return Err(limit());
        }

        self.contexts[self.context_depth] = context;
        self.context_depth += 1;

        Ok(())
    }

    fn pop_context(&mut self, expected: TType) -> Result<()> {
        assert!(self.context_depth <= CONTEXT_DEPTH_MAX);
        if self.context_depth == 0 {
            return Err(wire_type());
        }

        let kind = match self.contexts[self.context_depth - 1] {
            Context::Struct(Some(TType::Stop)) => TType::Struct,
            Context::Sequence {
                kind, remaining: 0, ..
            } => kind,
            Context::Map {
                remaining: 0,
                key_next: true,
                ..
            } => TType::Map,
            _ => return Err(wire_type()),
        };
        if kind != expected {
            return Err(wire_type());
        }

        self.context_depth -= 1;
        self.contexts[self.context_depth] = Context::Empty;

        Ok(())
    }
}

macro_rules! forward {
    ($($name:ident -> $kind:ty),+ $(,)?) => {
        $(fn $name(&mut self) -> Result<$kind> {
            self.native.$name()
        })+
    };
}

macro_rules! read_scalar {
    ($($name:ident -> $kind:ty, $wire:ident);+ $(;)?) => {
        $(fn $name(&mut self) -> Result<$kind> {
            self.read_value(TType::$wire)?;
            self.native.$name()
        })+
    };
}

impl<R: Read> TInputProtocol for BoundedProtocol<R> {
    forward!(
        read_message_begin -> TMessageIdentifier,
        read_message_end -> (),
        read_field_end -> (),
        read_byte -> u8,
    );

    read_scalar!(
        read_bool -> bool, Bool;
        read_bytes -> Vec<u8>, String;
        read_i8 -> i8, I08;
        read_i16 -> i16, I16;
        read_i32 -> i32, I32;
        read_i64 -> i64, I64;
        read_double -> f64, Double;
        read_string -> String, String;
    );

    fn read_field_begin(&mut self) -> Result<TFieldIdentifier> {
        let field = self.native.read_field_begin()?;
        if let Some(identifier) = field.id {
            if !(1..=1024).contains(&identifier) {
                return Err(limit());
            }
        }

        if self.context_depth > 0 {
            let Context::Struct(next_type) = &mut self.contexts[self.context_depth - 1] else {
                return Err(wire_type());
            };

            if next_type.is_some() {
                return Err(wire_type());
            }

            *next_type = Some(field.field_type);
        }

        Ok(field)
    }

    fn read_struct_begin(&mut self) -> Result<Option<TStructIdentifier>> {
        if self.struct_depth >= STRUCT_DEPTH_MAX {
            return Err(limit());
        }

        self.read_value(TType::Struct)?;
        self.push_context(Context::Struct(None))?;
        self.struct_depth += 1;

        self.native.read_struct_begin()
    }

    fn read_struct_end(&mut self) -> Result<()> {
        if self.struct_depth == 0 {
            return Err(limit());
        }

        self.pop_context(TType::Struct)?;
        self.struct_depth -= 1;

        self.native.read_struct_end()
    }

    fn read_list_begin(&mut self) -> Result<TListIdentifier> {
        self.read_value(TType::List)?;
        let list = self.native.read_list_begin()?;
        if list.size > CONTAINER_ITEMS_MAX {
            return Err(limit());
        }

        self.push_context(Context::Sequence {
            kind: TType::List,
            element_type: list.element_type,
            remaining: list.size,
        })?;

        Ok(list)
    }

    fn read_list_end(&mut self) -> Result<()> {
        self.pop_context(TType::List)?;
        self.native.read_list_end()
    }

    fn read_set_begin(&mut self) -> Result<TSetIdentifier> {
        self.read_value(TType::Set)?;
        let set = self.native.read_set_begin()?;
        if set.size > CONTAINER_ITEMS_MAX {
            return Err(limit());
        }

        self.push_context(Context::Sequence {
            kind: TType::Set,
            element_type: set.element_type,
            remaining: set.size,
        })?;

        Ok(set)
    }

    fn read_set_end(&mut self) -> Result<()> {
        self.pop_context(TType::Set)?;
        self.native.read_set_end()
    }

    fn read_map_begin(&mut self) -> Result<TMapIdentifier> {
        self.read_value(TType::Map)?;
        let map = self.native.read_map_begin()?;
        if map.size > CONTAINER_ITEMS_MAX {
            return Err(limit());
        }

        let (key_type, value_type) = if map.size == 0 {
            (TType::Stop, TType::Stop)
        } else {
            (
                map.key_type.ok_or_else(wire_type)?,
                map.value_type.ok_or_else(wire_type)?,
            )
        };

        self.push_context(Context::Map {
            key_type,
            value_type,
            remaining: map.size,
            key_next: true,
        })?;

        Ok(map)
    }

    fn read_map_end(&mut self) -> Result<()> {
        self.pop_context(TType::Map)?;
        self.native.read_map_end()
    }

    fn read_list<P: ReadThrift>(&mut self) -> Result<Vec<P>> {
        let list = self.read_list_begin()?;
        let count = usize::try_from(list.size).map_err(|_| limit())?;
        let bytes = count.checked_mul(size_of::<P>()).ok_or_else(limit)?;

        self.vector_bytes_left = self
            .vector_bytes_left
            .checked_sub(bytes)
            .ok_or_else(limit)?;

        let mut values = Vec::new();
        values.try_reserve_exact(count)?;

        for _index in 0..count {
            values.push(P::read_from_in_protocol(self)?);
        }

        self.read_list_end()?;

        Ok(values)
    }
}

fn wire_type() -> Error {
    Error::Protocol(ProtocolError {
        kind: ProtocolErrorKind::InvalidData,
        message: "recording Parquet metadata wire type".to_owned(),
    })
}

fn limit() -> Error {
    Error::Protocol(ProtocolError {
        kind: ProtocolErrorKind::SizeLimit,
        message: "recording Parquet metadata limit".to_owned(),
    })
}
