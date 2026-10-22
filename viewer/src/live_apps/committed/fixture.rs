use super::*;
use polars::prelude::{AnyValue, Column, IntoColumn, Series};

pub(super) fn refresh(connection: &Engine) {
    let source = Sources {
        processes: "processes",
        events: "events",
    }
    .query();
    let plan = connection.plan(&format!("SELECT * FROM {source}")).unwrap();
    connection.register_lazy("app_rows", plan).unwrap();
}

pub(super) fn append(connection: &Engine, table: &str, addition: &DataFrame) {
    let mut frame = connection.query(&format!("SELECT * FROM {table}")).unwrap();
    frame.vstack_mut(addition).unwrap();
    connection.register(table, frame).unwrap();
    refresh(connection);
}

pub(super) fn insert(connection: &Engine, table: &str, fields: &[(&str, AnyValue<'_>)]) {
    let frame = connection
        .query(&format!("SELECT * FROM {table} LIMIT 0"))
        .unwrap();
    let columns = frame
        .columns()
        .iter()
        .map(|column| {
            let value = fields
                .iter()
                .find(|(name, _value)| *name == column.name().as_str())
                .map_or(AnyValue::Null, |(_name, value)| value.clone());
            Series::from_any_values_and_dtype(column.name().clone(), &[value], column.dtype(), true)
                .unwrap()
                .into_column()
        })
        .collect();
    append(connection, table, &DataFrame::new(1, columns).unwrap());
}

pub(super) fn replace(connection: &Engine, table: &str, fields: &[(&str, AnyValue<'_>)]) {
    let mut frame = connection.query(&format!("SELECT * FROM {table}")).unwrap();
    for (name, value) in fields {
        let original = frame.column(name).unwrap();
        let series = Series::from_any_values_and_dtype(
            (*name).into(),
            std::slice::from_ref(value),
            original.dtype(),
            true,
        )
        .unwrap();
        let column: Column = series.into();
        frame
            .with_column(column.new_from_index(0, frame.height()))
            .unwrap();
    }
    connection.register(table, frame).unwrap();
    refresh(connection);
}

pub(super) fn duplicate(connection: &Engine, table: &str, count: usize) {
    let frame = connection.query(&format!("SELECT * FROM {table}")).unwrap();
    let addition = frame.new_from_index(0, count);
    append(connection, table, &addition);
}

pub(super) fn append_events(connection: &Engine, end: u64) {
    let frame = connection.query("SELECT * FROM events LIMIT 1").unwrap();
    let count = usize::try_from(end - 11).unwrap();
    let mut addition = frame.new_from_index(0, count);
    let numbers: Vec<_> = (11..end).collect();
    addition
        .with_column(Column::new("event".into(), numbers))
        .unwrap();
    append(connection, "events", &addition);
}
