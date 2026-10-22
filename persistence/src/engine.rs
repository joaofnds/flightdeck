//! Native Polars plans and typed materialization for recording queries.

mod admission;
mod cells;
mod scan;
mod sources;
mod startup;

#[cfg(test)]
mod source_tests;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::{Arc, Mutex};

use polars::lazy::frame::InProcessQuery;
use polars::prelude::{DataFrame, IntoLazy, LazyFrame, SchemaRef};
use polars::sql::SQLContext;

use crate::{Error, Result};

pub use admission::COLUMNS_MAX as PARQUET_COLUMNS_MAX;
pub use admission::EXPANDED_BYTES_MAX as PARQUET_EXPANDED_BYTES_MAX;
pub use cells::{Cells, FromCell, Row};
pub use scan::scan;
pub use sources::PARQUET_SOURCES_MAX;
pub use startup::{THREAD_DEFAULTS, initialize_process};

pub struct Engine {
    context: Mutex<SQLContext>,
    execution: Mutex<()>,
    interrupt: Arc<InterruptHandle>,
    sources: Mutex<sources::Sources>,
}

#[derive(Default)]
pub struct InterruptHandle {
    query: Mutex<Option<Arc<InProcessQuery>>>,
}

impl Engine {
    #[must_use]
    pub fn new() -> Self {
        Self {
            context: Mutex::new(SQLContext::new()),
            execution: Mutex::new(()),
            interrupt: Arc::new(InterruptHandle::default()),
            sources: Mutex::new(sources::Sources::new()),
        }
    }

    pub fn register(&self, name: &str, frame: DataFrame) -> Result<()> {
        self.register_lazy(name, frame.lazy())
    }

    pub fn register_lazy(&self, name: &str, frame: LazyFrame) -> Result<()> {
        self.context
            .lock()
            .map_err(|_| Error::Bound("query context poisoned"))?
            .register(name, frame);
        Ok(())
    }

    pub fn unregister(&self, name: &str) -> Result<()> {
        self.context
            .lock()
            .map_err(|_| Error::Bound("query context poisoned"))?
            .unregister(name);
        Ok(())
    }

    /// Returns a stable SQL relation for one admitted immutable file, without Hive inference.
    /// The context retains at most 128 saved scan relations; call again before reusing old SQL.
    pub fn parquet_source(&self, path: &Path) -> Result<String> {
        // Other context operations acquire no source lock, so this order cannot be reversed.
        let mut sources = self
            .sources
            .lock()
            .map_err(|_| Error::Bound("query sources poisoned"))?;
        let mut context = self
            .context
            .lock()
            .map_err(|_| Error::Bound("query context poisoned"))?;

        sources.parquet_source(&mut context, path)
    }

    /// Returns the relation `derive` builds over one admitted immutable file for `kind`.
    /// The relation is derived once and reused while the file's registration is retained,
    /// so `derive` must depend only on the file's contents and `kind`. Refusals are not kept.
    pub fn parquet_relation(
        &self,
        path: &Path,
        kind: &'static str,
        derive: impl FnOnce(&Self, &str) -> Result<String>,
    ) -> Result<String> {
        let source = self.parquet_source(path)?;
        let retained = self
            .sources
            .lock()
            .map_err(|_| Error::Bound("query sources poisoned"))?
            .relation(&source, kind);
        if let Some(relation) = retained {
            return Ok(relation);
        }

        let relation = derive(self, &source)?;

        self.sources
            .lock()
            .map_err(|_| Error::Bound("query sources poisoned"))?
            .relation_retain(&source, kind, &relation);

        Ok(relation)
    }

    pub fn plan(&self, sql: &str) -> Result<LazyFrame> {
        Ok(self
            .context
            .lock()
            .map_err(|_| Error::Bound("query context poisoned"))?
            .execute(sql)?)
    }

    pub fn query(&self, sql: &str) -> Result<DataFrame> {
        self.collect(self.plan(sql)?)
    }

    pub fn schema(&self, source: &str) -> Result<SchemaRef> {
        Ok(self
            .plan(&format!("SELECT * FROM {source}"))?
            .collect_schema()?)
    }

    #[must_use]
    pub fn interrupt_handle(&self) -> Arc<InterruptHandle> {
        Arc::clone(&self.interrupt)
    }

    pub fn collect(&self, plan: LazyFrame) -> Result<DataFrame> {
        let execution = self
            .execution
            .lock()
            .map_err(|_| Error::Bound("query execution poisoned"))?;
        let query = Arc::new(plan.collect_concurrently()?);
        self.interrupt.query_install(Arc::clone(&query))?;

        let result = query.fetch_blocking().map_err(Error::from);
        self.interrupt.query_clear()?;
        drop(execution);

        result
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl InterruptHandle {
    pub fn interrupt(&self) -> Result<()> {
        let query = self
            .query
            .lock()
            .map_err(|_| Error::Bound("query interruption poisoned"))?;

        if let Some(query) = &*query {
            query.cancel();
        }

        Ok(())
    }

    fn query_install(&self, query: Arc<InProcessQuery>) -> Result<()> {
        let mut current = self
            .query
            .lock()
            .map_err(|_| Error::Bound("query interruption poisoned"))?;

        assert!(current.is_none());
        *current = Some(query);

        Ok(())
    }

    fn query_clear(&self) -> Result<()> {
        let mut current = self
            .query
            .lock()
            .map_err(|_| Error::Bound("query interruption poisoned"))?;

        assert!(current.is_some());
        *current = None;

        Ok(())
    }
}
