//! A registered recording's windows, found through its catalog. A session lists the windows of one
//! span of the clock and loads another when a read reaches past it, so what it holds and what it
//! reads to open follow that span and never how much the recording has saved.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use persistence::canonical::ReadWork;
use persistence::canonical::catalog::{Catalog, KnownReading};
use persistence::resources::ByteBudget;
use persistence::retention::Retained;
use persistence::{Error, Manifest, Result};

use super::earlier::{LEAD_IN_SECONDS, LOADED_SECONDS};
use super::{ContextRange, NS, Session, TimeBounds};
use crate::cancellation::Operation;

/// The seconds a span holds either side of what a read needs, so a walk along the clock loads a
/// span once in this many seconds and not at every step.
const SPAN_MARGIN_SECONDS: u64 = 900;
/// The passes a read waits through for a publication to finish, each `RETRY_WAIT` apart. A
/// publication's links take a few writes, and one still going after this is reported.
const RETRY_PASSES_MAX: u32 = 2000;
const RETRY_WAIT: std::time::Duration = std::time::Duration::from_millis(5);

const _: () = assert!(SPAN_MARGIN_SECONDS + LOADED_SECONDS + LEAD_IN_SECONDS <= 65_535);

/// The oldest wall clock the retention period still covers, of a recording whose oldest listed
/// reading is `oldest` and whose newest lies at `newest_ns`. An expired reading stays listed until
/// physical removal reaches it, so this lies past `oldest` by how far that reading has expired. A
/// wall clock stepped since then moves this and no reading, since each reading answers for itself
/// when a read asks for it.
pub(crate) fn first_retained_ns(
    root: &Path,
    catalog: &Catalog,
    oldest: &KnownReading,
    newest_ns: u64,
) -> io::Result<u64> {
    let window = match catalog.window_at_or_before(
        &oldest.run,
        oldest.publication,
        &mut ReadWork::default(),
    ) {
        Ok(window) => window,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Err(error),
        // Damage in the oldest window stops no reader of a newer one, and a read of it says so.
        Err(_) => None,
    };
    let expired_by_ns = window
        .and_then(|manifest| manifest.continuous_ns)
        .map_or(0, |bounds| {
            Retained::read(root, [oldest.run.boot]).expired_by_ns(oldest.run.boot, bounds.min)
        });

    Ok(oldest.wall_ns.saturating_add(expired_by_ns).min(newest_ns))
}

/// How a session of a registered recording reaches its windows.
pub(super) struct Registered {
    catalog: Catalog,
    /// The clock range whose windows the session lists, none until a recording has a reading.
    span: Option<Span>,
}

#[derive(Clone, Copy)]
struct Span {
    /// What the read that loaded it asked for.
    needed: ContextRange,
    /// That and a margin either side.
    loaded: ContextRange,
}

/// The recording's oldest and newest seconds with a known clock.
#[derive(Clone, Copy)]
pub(super) struct Ends {
    pub(super) first: u64,
    pub(super) last: u64,
}

impl Registered {
    /// The catalog of `root`.
    pub(super) fn open(
        root: &Path,
        budget: &Arc<ByteBudget>,
        operation: &Operation,
    ) -> Result<Self> {
        let catalog = registered_retry(operation, || {
            Catalog::open(root, budget).map_err(Error::from)
        })?;

        Ok(Self {
            catalog,
            span: None,
        })
    }

    /// The oldest second the retention period still covers and the newest second the recording
    /// holds.
    pub(super) fn ends(&self, root: &Path, operation: &Operation) -> Result<Option<Ends>> {
        let mut work = ReadWork::default();
        let oldest = registered_retry(operation, || {
            self.catalog.oldest_known(&mut work).map_err(Error::from)
        })?;
        let newest = registered_retry(operation, || {
            self.catalog.newest_known(&mut work).map_err(Error::from)
        })?;
        let (Some(oldest), Some(newest)) = (oldest, newest) else {
            return Ok(None);
        };

        let first_ns = registered_retry(operation, || {
            first_retained_ns(root, &self.catalog, &oldest, newest.wall_ns).map_err(Error::from)
        })?;

        Ok(Some(Ends {
            first: first_ns / NS,
            last: newest.wall_ns / NS,
        }))
    }

    /// How many readings have no known clock, none where more of them lie past one page, so a
    /// partial count is never taken for the whole.
    pub(super) fn unknown_clock_ticks(&self, operation: &Operation) -> Result<Option<u64>> {
        let mut work = ReadWork::default();
        registered_retry(operation, || {
            self.catalog
                .unknown_count_within_page(&mut work)
                .map_err(Error::from)
        })
    }

    /// Lists the windows of the readings `span` holds, by run and then publication. A reading takes
    /// its rates only from one a second before it, so the margin around what a read needs holds
    /// every window those rates come from. Only the windows of `needed` are what a read asks for,
    /// so damage in the margin leaves its windows unlisted and stops nothing, and a read that goes
    /// on to reach them finds out then.
    fn windows(
        &self,
        needed: ContextRange,
        span: ContextRange,
        operation: &Operation,
    ) -> Result<Vec<Manifest>> {
        let mut work = ReadWork::default();
        let mut windows = registered_retry(operation, || {
            self.catalog
                .windows_in(needed.first_ns, needed.end_ns, &mut work)
        })?;
        for (first_ns, end_ns) in [
            (span.first_ns, needed.first_ns),
            (needed.end_ns, span.end_ns),
        ] {
            let beside = registered_retry(operation, || {
                self.catalog.windows_in(first_ns, end_ns, &mut work)
            });
            for manifest in windows_beside(beside, operation)? {
                if !windows.contains(&manifest) {
                    windows.push(manifest);
                }
            }
        }
        windows.sort_by_key(|manifest| (manifest.identity.run.run.0, manifest.publication_first));

        Ok(windows)
    }
}

/// The windows a lookup beside a read's own found, none where what it reached does not read. A
/// canceled read and a machine that refuses the work say nothing about what is saved, so they stay
/// errors.
fn windows_beside(found: Result<Vec<Manifest>>, operation: &Operation) -> Result<Vec<Manifest>> {
    operation.check()?;
    match found {
        Err(Error::Io(error))
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof | io::ErrorKind::NotFound
            ) =>
        {
            collector::diagnostics::error(format_args!(
                "saved window beside the read span left out: {error}"
            ));
            Ok(Vec::new())
        }
        result => result,
    }
}

/// The clock range a context ending at `last_second` may read, its older tail included.
fn span_needed(last_second: u64) -> Result<ContextRange> {
    let end_ns = last_second
        .checked_add(1)
        .and_then(|end| end.checked_mul(NS))
        .ok_or(Error::Bound("span end"))?;

    Ok(ContextRange {
        first_ns: last_second.saturating_sub(LOADED_SECONDS + LEAD_IN_SECONDS) * NS,
        end_ns,
    })
}

/// The clock range loaded for a context ending at `last_second`.
fn span_loaded(last_second: u64) -> Result<ContextRange> {
    let needed = span_needed(last_second)?;

    Ok(ContextRange {
        first_ns: needed.first_ns.saturating_sub(SPAN_MARGIN_SECONDS * NS),
        end_ns: needed
            .end_ns
            .checked_add(SPAN_MARGIN_SECONDS * NS)
            .ok_or(Error::Bound("span end"))?,
    })
}

fn registered_retry<T>(operation: &Operation, mut read: impl FnMut() -> Result<T>) -> Result<T> {
    catalog_retry(|| {
        operation.check()?;
        read()
    })
}

/// Runs `read` until no publication stands in its way, since a reader that meets one has learned
/// nothing about what the recording holds.
pub(crate) fn catalog_retry<T>(mut read: impl FnMut() -> Result<T>) -> Result<T> {
    for _pass in 0..RETRY_PASSES_MAX {
        match read() {
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(RETRY_WAIT);
            }
            result => return result,
        }
    }

    Err(io::Error::from(io::ErrorKind::WouldBlock).into())
}

impl Session {
    /// Loads the windows a context ending at `last_second` reads, where the session lists another
    /// span. A list that changes takes with it everything that names a window by its place.
    pub(crate) fn load_span(&mut self, last_second: u64) -> Result<()> {
        let needed = span_needed(last_second)?;
        if self
            .registered
            .span
            .is_some_and(|span| span_covers(span.loaded, needed))
        {
            return Ok(());
        }

        let span = span_loaded(last_second)?;
        let windows = self.registered.windows(needed, span, &self.operation)?;
        self.load_span_install(needed, span, windows)
    }

    /// Whether the session lists the windows a context ending at `last_second` reads.
    pub(crate) fn span_holds(&self, last_second: u64) -> Result<bool> {
        let needed = span_needed(last_second)?;

        Ok(self
            .registered
            .span
            .is_some_and(|span| span_covers(span.loaded, needed)))
    }

    /// Refuses a read of `range` that reaches past the windows the session lists, since finding no
    /// readings there would say the recording has none.
    pub(super) fn span_check(&self, range: ContextRange) -> Result<()> {
        match self.registered.span {
            Some(span) if !span_covers(span.loaded, range) => {
                Err(Error::Bound("read past the session's span"))
            }
            _ => Ok(()),
        }
    }

    fn load_span_install(
        &mut self,
        needed: ContextRange,
        span: ContextRange,
        windows: Vec<Manifest>,
    ) -> Result<()> {
        let selected = self
            .selected
            .take()
            .map(|selected| (self.windows[selected.window].clone(), selected));
        let verified = self.verified_windows();
        self.verified = verified_flags(&windows, &verified.0);
        self.windows = windows;
        self.selected = selected.and_then(|(manifest, mut observation)| {
            let place = self.windows.iter().position(|listed| *listed == manifest)?;
            observation.window = place;
            Some(observation)
        });
        self.forget_page_caches();
        self.earlier_tail.get_mut().take();
        self.registered.span = Some(Span {
            needed,
            loaded: span,
        });

        self.span_bounds()
    }

    /// The wall-clock bounds of each listed window. The recording's ends and its count of readings
    /// with no clock come from the catalog, so a window read here adds to neither.
    pub(super) fn span_bounds(&mut self) -> Result<()> {
        let mut wall_bounds = Vec::with_capacity(self.windows.len());
        for window in 0..self.windows.len() {
            wall_bounds.push(match self.windows[window].wall_ns {
                Some(bounds) => Some(bounds),
                None => self.span_bounds_ticks(window)?,
            });
        }
        self.wall_bounds = wall_bounds;

        Ok(())
    }

    /// The wall-clock bounds of the ticks of a window whose manifest holds none.
    fn span_bounds_ticks(&self, window: usize) -> Result<Option<TimeBounds>> {
        let mut bounds: Option<TimeBounds> = None;
        for tick in &self.reader().minute_ticks(window)? {
            if let Some(wall) = tick.wall_ns {
                bounds = Some(match bounds {
                    Some(bounds) => TimeBounds {
                        min: bounds.min.min(wall),
                        max: bounds.max.max(wall),
                    },
                    None => TimeBounds {
                        min: wall,
                        max: wall,
                    },
                });
            }
        }

        Ok(bounds)
    }

    /// Whether the windows the session's span holds on disk are those it lists.
    pub(crate) fn lists_every_sealed_window(&self) -> Result<bool> {
        let Some(span) = self.registered.span else {
            return Ok(self.registered.ends(&self.root, &self.operation)?.is_none());
        };

        Ok(self
            .registered
            .windows(span.needed, span.loaded, &self.operation)?
            == self.windows)
    }
}

fn span_covers(span: ContextRange, needed: ContextRange) -> bool {
    span.first_ns <= needed.first_ns && needed.end_ns <= span.end_ns
}

/// Marks the windows of `windows` that `verified` holds unchanged.
pub(super) fn verified_flags(windows: &[Manifest], verified: &[Manifest]) -> Vec<AtomicBool> {
    windows
        .iter()
        .map(|manifest| AtomicBool::new(verified.contains(manifest)))
        .collect()
}
