//! A table after the validation boundary, materialized for test queries.
use persistence::engine::Engine;
use persistence::query::TableSource;
use persistence::schema::Table;
use polars::prelude::DataFrame;

/// A table after validation and normalization, queryable as `normalized`.
pub struct Normalized {
    engine: Engine,
}

impl Normalized {
    pub fn query(&self, sql: &str) -> DataFrame {
        self.engine.query(sql).unwrap()
    }
}

/// Validates and normalizes one table as the viewer does, reading a staged table from `engine`.
pub fn normalize(
    engine: &Engine,
    table: Table,
    source: TableSource<'_>,
) -> persistence::Result<Normalized> {
    let normalized = persistence::query::table_source(engine, table, source)?;
    let frame = engine.query(&format!("SELECT * FROM {normalized}"))?;

    let normalized = Engine::new();
    normalized.register("normalized", frame)?;
    Ok(Normalized { engine: normalized })
}
