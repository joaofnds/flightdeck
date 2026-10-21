//! The observations whose sealed windows keep their Apps' parts, which the histories of every
//! App take in place of a replay of each window's minute.
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::sealed::{Kept, observed_in};
use super::{BTreeSet, ContextRange, Restore, Session};
use crate::app_attribution_history::cache::Observed;
use crate::live_app_history::{ResidentFile, ResidentPin};
use crate::live_app_response::Header;
use persistence::schema::APP_PARTS;
use persistence::{Result, WindowIdentity};

/// A window of a range whose seal kept its Apps' parts, and the facts of each of its
/// observations in the range, of which it holds one at least.
pub(super) struct KeptWindow {
    pub window: usize,
    path: PathBuf,
    pub observations: Vec<KeptObservation>,
}

pub(super) struct KeptObservation {
    pub header: Header,
    pub plotted: bool,
}

/// The facts each sealed file of Apps' parts keeps. A sealed file never changes and each
/// second's histories read the same files again, so the Session keeps them.
pub(in crate::session) struct KeptHeaders {
    files: BTreeMap<PathBuf, KeptHeadersFile>,
}

struct KeptHeadersFile {
    source: String,
    headers: Option<Vec<Header>>,
}

impl KeptHeaders {
    pub(in crate::session) fn new() -> Self {
        Self {
            files: BTreeMap::new(),
        }
    }
}

const KEPT_FILES_MAX: usize = 64;

impl Session {
    /// Gives `output` each window of `range` whose parts this build reads and that keeps an
    /// observation in `range`, with the facts of those observations, each counted as plotted
    /// in `restore`. Returns every window whose parts this build reads, which needs no replay.
    pub(super) fn attribution_kept(
        &self,
        range: &ContextRange,
        restore: &mut Restore,
        output: &mut Vec<KeptWindow>,
    ) -> Result<BTreeSet<usize>> {
        let mut kept_windows = BTreeSet::new();
        self.visit_derived(
            *range,
            APP_PARTS,
            |kept| Ok((kept.path.to_owned(), self.attribution_kept_headers(kept)?)),
            |window, (path, headers)| {
                let Some(headers) = headers else {
                    return Ok(());
                };
                let mut observations = Vec::with_capacity(headers.len());
                for header in headers {
                    if observed_in(*range, &header.selected) {
                        restore.plot()?;
                        observations.push(KeptObservation {
                            header,
                            plotted: true,
                        });
                    }
                }
                kept_windows.insert(window);
                // Each window kept holds an observation counted as plotted, which bounds them.
                if !observations.is_empty() {
                    assert!(output.len() < super::OBSERVATIONS_MAX);
                    output.push(KeptWindow {
                        window,
                        path,
                        observations,
                    });
                }
                Ok(())
            },
        )?;

        Ok(kept_windows)
    }

    /// The facts of each observation the parts of `kept` keep, in publication order, or none
    /// where this build does not read its parts.
    fn attribution_kept_headers(&self, kept: &Kept<'_>) -> Result<Option<Vec<Header>>> {
        let source = self.connection.parquet_source(kept.path)?;
        if let Some(file) = self.app_history_kept_headers.borrow().files.get(kept.path) {
            // A file replaced since its facts were read registers as another source.
            if file.source == source {
                return Ok(file.headers.clone());
            }
        }

        let headers = self.kept_parts_headers(kept, &source)?;

        let mut cache = self.app_history_kept_headers.borrow_mut();
        if cache.files.len() == KEPT_FILES_MAX {
            cache.files.clear();
        }
        cache.files.insert(
            kept.path.to_owned(),
            KeptHeadersFile {
                source,
                headers: headers.clone(),
            },
        );
        Ok(headers)
    }

    /// Gives `consume` the facts and the Apps of each plotted observation of `kept`, from the
    /// App rows of its window's parts, after `pin` takes the window's commitment and table.
    pub(super) fn visit_attribution_kept_apps(
        &self,
        kept: &KeptWindow,
        pin: &mut impl FnMut(ResidentPin) -> Result<()>,
        consume: &mut impl FnMut(Observed<'_>, [Option<WindowIdentity>; 2]) -> Result<()>,
    ) -> Result<()> {
        let plotted_headers: Vec<&Header> = kept
            .observations
            .iter()
            .filter(|observation| observation.plotted)
            .map(|observation| &observation.header)
            .collect();
        if plotted_headers.is_empty() {
            return Ok(());
        }

        self.validate_commitment(kept.window)?;
        let source = self.connection.parquet_source(&kept.path)?;
        pin(ResidentPin {
            manifest: self.windows[kept.window].clone(),
            files: vec![ResidentFile {
                path: kept.path.clone(),
                alias: source.clone(),
            }],
        })?;
        let windows = [Some(self.windows[kept.window].identity), None];
        self.visit_kept_parts_apps(&source, &plotted_headers, |header, apps| {
            consume(Observed::Sealed { header, apps }, windows)
        })?;

        self.validate_commitment(kept.window)
    }
}
