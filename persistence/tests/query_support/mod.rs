//! Scratch Polars tables for recording fixtures, read back through the validation boundary.
use std::cell::RefCell;
use std::collections::BTreeMap;

use persistence::engine::Engine;
use persistence::query::TableSource;
use persistence::schema::Table;
use polars::prelude::{Column, DataFrame, IntoColumn, NamedFrom, Series};

pub mod normalized;
mod schema;

use normalized::{Normalized, normalize};

const READINGS_MAX: u64 = 1_048_577;

/// Named typed tables that tests build, change and save like a small SQL database.
pub struct Tables {
    engine: Engine,
    frames: RefCell<BTreeMap<String, DataFrame>>,
}

impl Tables {
    pub fn new() -> Self {
        Self {
            engine: Engine::new(),
            frames: RefCell::new(BTreeMap::new()),
        }
    }

    /// Creates an empty table from a column list such as `publication UBIGINT, pid INTEGER`.
    pub fn create(&self, name: &str, definition: &str) {
        let columns = definition
            .split(',')
            .map(|column| {
                let (field, kind) = column.trim().split_once(' ').unwrap();
                Column::full_null(field.into(), 0, &schema::data_type(kind.trim()))
            })
            .collect();

        let frame = DataFrame::new(0, columns).unwrap();
        self.frames.borrow_mut().insert(name.to_owned(), frame);
    }

    /// Appends `count` rows of `selection`, as `rows` builds them.
    pub fn insert(&self, name: &str, selection: &str, count: u64) {
        let rows = self.rows(selection, count);
        self.append(name, &rows);
    }

    /// Builds `count` rows of `selection`, which may read the row number `i` from 0.
    pub fn rows(&self, selection: &str, count: u64) -> DataFrame {
        assert!(count <= READINGS_MAX);

        let readings = Series::new("i".into(), (0..count).collect::<Vec<u64>>()).into_column();
        self.engine
            .register(
                "readings",
                DataFrame::new(readings.len(), vec![readings]).unwrap(),
            )
            .unwrap();
        self.engine
            .query(&format!("SELECT {selection} FROM readings"))
            .unwrap()
    }

    /// Appends rows by column name. Absent columns become NULL and the others take the
    /// table's types.
    pub fn append(&self, name: &str, rows: &DataFrame) {
        let mut frames = self.frames.borrow_mut();
        let table = frames.get_mut(name).unwrap();
        for column in rows.columns() {
            assert!(
                table.column(column.name()).is_ok(),
                "unknown {}",
                column.name()
            );
        }

        let columns = table
            .columns()
            .iter()
            .map(|column| match rows.column(column.name()) {
                Ok(values) => values.strict_cast(column.dtype()).unwrap(),
                Err(_) => Column::full_null(column.name().clone(), rows.height(), column.dtype()),
            })
            .collect::<Vec<_>>();
        table
            .vstack_mut_owned(DataFrame::new(rows.height(), columns).unwrap())
            .unwrap();
    }

    pub fn add_column(&self, name: &str, field: &str, kind: &str) {
        let data_type = schema::data_type(kind);

        self.change(name, |table| {
            let column = Column::full_null(field.into(), table.height(), &data_type);
            table.with_column(column).unwrap();
        });
    }

    /// Changes one table in place, as an `ALTER TABLE` or `UPDATE` would.
    pub fn change(&self, name: &str, change: impl FnOnce(&mut DataFrame)) {
        let mut frames = self.frames.borrow_mut();

        change(frames.get_mut(name).unwrap());
    }

    pub fn frame(&self, name: &str) -> DataFrame {
        self.frames.borrow()[name].clone()
    }
}

/// Validates and normalizes one staged table, or one saved file, as the viewer does.
pub fn table_source(
    tables: &Tables,
    table: Table,
    source: TableSource<'_>,
) -> persistence::Result<Normalized> {
    let engine = Engine::new();
    if matches!(source, TableSource::Staged) {
        engine.register(table.name(), tables.frame(table.name()))?;
    }

    normalize(&engine, table, source)
}
