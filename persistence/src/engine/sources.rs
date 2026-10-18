use std::collections::VecDeque;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use polars::sql::SQLContext;

use crate::{Error, Result};

pub const PARQUET_SOURCES_MAX: usize = 128;

pub(super) struct Sources {
    // Hard links share one fingerprint and therefore one registration to retain or evict.
    entries: VecDeque<Source>,
}

struct Source {
    fingerprint: Fingerprint,
    alias: String,
    path: PathBuf,
    // Evicted with its registration, since the relation names the registered alias.
    relation: Option<Relation>,
}

struct Relation {
    kind: &'static str,
    text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl Sources {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(PARQUET_SOURCES_MAX),
        }
    }

    pub fn parquet_source(&mut self, context: &mut SQLContext, path: &Path) -> Result<String> {
        assert!(self.entries.len() <= PARQUET_SOURCES_MAX);
        crate::query::validate_root(path)?;

        let fingerprint = Fingerprint::from(&path.metadata()?);
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.fingerprint == fingerprint)
        {
            let cached = &self.entries[index];
            if cached.path.as_path() != path {
                if cached.fingerprint != Fingerprint::from(&cached.path.metadata()?) {
                    return Err(Error::Bound("changed cached query source"));
                }
            }

            let source = self
                .entries
                .remove(index)
                .ok_or(Error::Bound("query source cache index"))?;
            let alias = source.alias.clone();
            self.entries.push_back(source);

            return Ok(alias);
        }

        let frame = super::scan::scan(path)?;
        if fingerprint != Fingerprint::from(&path.metadata()?) {
            return Err(Error::Bound("changed query source"));
        }

        let alias = fingerprint.alias();

        if self.entries.len() == PARQUET_SOURCES_MAX {
            let evicted = self
                .entries
                .pop_front()
                .ok_or(Error::Bound("query source cache empty"))?;
            context.unregister(&evicted.alias);
        }

        context.register(&alias, frame);
        self.entries.push_back(Source {
            fingerprint,
            alias: alias.clone(),
            path: path.to_owned(),
            relation: None,
        });
        assert!(self.entries.len() <= PARQUET_SOURCES_MAX);

        Ok(alias)
    }

    pub fn relation(&self, alias: &str, kind: &str) -> Option<String> {
        let relation = self
            .entries
            .iter()
            .find(|entry| entry.alias == alias)?
            .relation
            .as_ref()?;
        if relation.kind != kind {
            return None;
        }

        Some(relation.text.clone())
    }

    pub fn relation_retain(&mut self, alias: &str, kind: &'static str, text: &str) {
        // An alias evicted during derivation keeps nothing; its next request derives again.
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.alias == alias) {
            entry.relation = Some(Relation {
                kind,
                text: text.to_owned(),
            });
        }
    }
}

impl From<&Metadata> for Fingerprint {
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

impl Fingerprint {
    fn alias(&self) -> String {
        // The complete tuple gives rebinding a stable name without hashing or SQL path literals.
        format!(
            "flightdeck_parquet_{:016x}_{:016x}_{:016x}_{:016x}_{:016x}_{:016x}_{:016x}",
            self.device,
            self.inode,
            self.length,
            self.modified_seconds,
            self.modified_nanoseconds,
            self.changed_seconds,
            self.changed_nanoseconds,
        )
    }
}
