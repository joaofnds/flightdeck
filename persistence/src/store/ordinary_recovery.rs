//! Source-backed reconstruction of a complete Registered ordinary payload, before metadata.
use super::inventory::OriginalInventory;
use super::normal_proof::SourceProof;
use super::publication_control::{self, CountedControl};
use super::{
    NormalPublication, OriginalFile, PublicationLedger, PublicationStep, PublicationWork,
    RegisteredAttempt, StagingPermit, Store,
};
use crate::canonical::rows;
use crate::encoder::metric_projection::{ColumnGroups, MetricFrame, MetricTick};
use crate::engine::AdmittedTable;
use crate::resources::{ByteBudget, ByteReservation};
use crate::transaction::{AttemptOperation, normal::NormalState};
use collector::recording::bootstrap::codec::Phase;
use collector::ring::RingReader;
#[path = "ordinary_comparison.rs"]
mod comparison;
use comparison::Comparison;
#[path = "ordinary_order.rs"]
mod order;
use std::io;
use std::sync::{Arc, atomic::Ordering};

const CHUNK_ROWS: usize = 64;
const _: () = assert!(CHUNK_ROWS * (crate::engine::PARQUET_COLUMNS_MAX + 61) <= 16384);

pub struct OrdinaryRecovery<'store> {
    inventory: Option<OriginalInventory<'store, 'store>>,
    source: Option<SourceProof>,
    publication: Option<NormalPublication<'store>>,
    state: NormalState,
    budget: Arc<ByteBudget>,
    ledger: PublicationLedger,
    work: PublicationWork,
    recorded: PublicationWork,
    publication_recorded: PublicationWork,
    observations: Vec<crate::Observation>,
    groups: ColumnGroups,
    selected: usize,
    table: usize,
    input: Option<AdmittedTable>,
    tick: Option<MetricTick>,
    expected: Option<MetricFrame>,
    expected_first: usize,
    matched: usize,
    offset: u64,
    table_rows: u64,
    order_offset: u64,
    previous_key: Option<order::FileKey>,
    tables: Vec<crate::TableManifest>,
    column: usize,
    matches: Vec<(usize, usize)>,
    chunk_count: u32,
    _reservation: ByteReservation,
}
impl Store {
    /// Consumes the occupied ordinary registration without returning raw writable authority.
    /// Requires the exact registered source until complete proof metadata is durable.
    pub fn resume_ordinary_proof(
        &self,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<OrdinaryRecovery<'_>> {
        ledger.require_budget(budget)?;

        if self
            .staging
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }

        let permit = StagingPermit(Arc::clone(&self.staging));
        let reservation = budget
            .reserve(64 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let counted = CountedControl {
            root: &self.directory,
            ledger: Some(ledger),
        };
        let state = publication_control::read_normal(&counted, self.recording)?;
        if state.phase != Phase::Registered
            || state.census.kind != 0
            || state.salvage.is_some()
            || state.row_plan.is_some()
            || !matches!(state.input.operation, AttemptOperation::Ordinary { .. })
        {
            return Err(invalid());
        }

        publication_control::visible(&counted, &state)?;
        let source_reservation = budget
            .reserve(64 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        ledger.update(|work| work.source_observations += 1);
        let source = SourceProof::new(reader, &state.input, source_reservation)?;
        resume_ordinary_proof_owner(self, state, source, permit, budget, ledger, reservation)
    }
}
impl<'store> OrdinaryRecovery<'store> {
    fn new(
        inventory: OriginalInventory<'store, 'store>,
        source: SourceProof,
        state: NormalState,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
        work: &PublicationWork,
        reservation: ByteReservation,
    ) -> Self {
        Self {
            inventory: Some(inventory),
            source: Some(source),
            publication: None,
            state,
            budget: Arc::clone(budget),
            ledger: ledger.clone(),
            work: *work,
            recorded: PublicationWork::default(),
            publication_recorded: PublicationWork::default(),
            observations: Vec::with_capacity(61),
            groups: ColumnGroups::default(),
            selected: 0,
            table: 0,
            input: None,
            tick: None,
            expected: None,
            expected_first: 0,
            matched: 0,
            offset: 0,
            table_rows: 0,
            order_offset: 0,
            previous_key: None,
            tables: Vec::with_capacity(12),
            column: 0,
            matches: Vec::with_capacity(CHUNK_ROWS),
            chunk_count: 0,
            _reservation: reservation,
        }
    }
    #[must_use]
    pub fn work(&self) -> PublicationWork {
        self.ledger.project(&self.snapshot(), &self.recorded)
    }
    fn snapshot(&self) -> PublicationWork {
        let mut work = self.work;
        if let Some(inventory) = &self.inventory {
            work.inventory = inventory.work();
        }
        if let Some(source) = &self.source {
            work.source_reads = source.reads;
            // Constructor's initial binding observation is already in the caller ledger.
            work.source_observations = source.observations.get().saturating_sub(1);
            work.proof_operations += source.operations;
        }
        work
    }
    /// Whether the originals have proved themselves and their publication has begun, from which
    /// point it needs every one of them.
    #[must_use]
    pub(crate) fn publishing(&self) -> bool {
        self.publication.is_some()
    }
    /// One census continuation, source tick, or bounded comparison chunk per call.
    pub fn step(&mut self) -> io::Result<PublicationStep> {
        let result = self.step_inner();
        let work = self.snapshot();
        self.ledger.record(&work, &mut self.recorded);
        result
    }
    fn step_inner(&mut self) -> io::Result<PublicationStep> {
        if let Some(publication) = &mut self.publication {
            let result = publication.step();
            self.ledger
                .record(&publication.work(), &mut self.publication_recorded);
            return result;
        }

        publication_control::visible(
            &CountedControl {
                root: self.inventory.as_ref().ok_or_else(invalid)?.owner().root,
                ledger: Some(&self.ledger),
            },
            &self.state,
        )?;
        let inventory = self.inventory.as_mut().ok_or_else(invalid)?;
        if !inventory.enumerated() {
            inventory.step().map_err(rows::source_error)?;
            return Ok(PublicationStep::Continue);
        }

        inventory.release_decoder_workspace()?;
        if self.observations.is_empty() {
            self.step_ticks()?;
        } else if self.selected < self.observations.len() && self.table == 0 {
            let tick = self.read_tick(self.selected)?;
            self.groups.include(&tick.window);
            self.selected += 1;
        } else if self.table == 0 {
            self.table = 1;
            self.selected = 0;
        } else if self.table <= 9 {
            if let Some(input) = &self.input
                && self.order_offset < input.row_count()
            {
                let (count, key) = order::validate_chunk(
                    input,
                    crate::schema::TABLES[self.table - 1],
                    self.order_offset,
                    self.previous_key.as_ref(),
                    &self.ledger,
                    &mut self.work,
                )?;
                self.order_offset += u64::from(count);
                self.previous_key = Some(key);
            } else {
                self.step_table()?;
            }
        } else if self.table <= 11 {
            self.step_derived()?;
        } else {
            self.finish()?;
        }
        Ok(PublicationStep::Continue)
    }
    fn step_ticks(&mut self) -> io::Result<()> {
        let inventory = self.inventory.as_mut().ok_or_else(invalid)?;
        if inventory.file(OriginalFile::Ticks).is_none() {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }

        let file = inventory.reopen_file(OriginalFile::Ticks)?;
        self.work.canonical.raw_admissions += 1;
        let limits = crate::engine::DecodeLimits {
            compressed_bytes: 8 * 1024 * 1024,
            expanded_bytes: 8 * 1024 * 1024,
            rows: 61,
            row_groups: 1,
            columns: crate::schema::TICK_COLUMNS_MAX,
        };
        let table = AdmittedTable::from_file_streaming_limited_counted(
            file,
            &self.budget,
            &limits,
            &self.ledger,
        )
        .map_err(rows::admission_error)?;
        let count = u32::try_from(table.row_count()).map_err(|_| invalid())?;
        let _workspace = table
            .reserve_slice_work(0, count)
            .map_err(rows::admission_error)?;
        self.work.proof_operations += 1;
        let frame = table
            .read_slice_counted(
                &["publication", "wall_ns", "continuous_ns", "awake_ns"],
                0,
                count,
                &self.ledger,
            )
            .map_err(rows::read_error)?;
        self.work.proof_operations += collector::u64_from_usize(frame.height() * 5);
        self.observations = step_ticks_observations(&frame)?;
        Ok(())
    }
    fn read_tick(&mut self, selected: usize) -> io::Result<MetricTick> {
        let AttemptOperation::Ordinary { window, .. } = self.state.input.operation else {
            return Err(invalid());
        };
        let identity = crate::WindowIdentity {
            run: self.state.input.run,
            window,
        };
        let publication = self.observations[selected].publication;
        let source = self.source.as_mut().ok_or_else(invalid)?;
        let mut tick = source.metric_tick(
            &self.state.input,
            &identity,
            publication,
            &self.budget,
            &self.ledger,
        )?;
        let observation = tick.window.observations().next().ok_or_else(invalid)?;
        if *observation != self.observations[selected] {
            return Err(invalid());
        }

        let staged = tick.window.ticks.first().ok_or_else(invalid)?;
        if self
            .state
            .input
            .source
            .window(staged.run_tick, observation.continuous_ns)?
            != window
        {
            return Err(invalid());
        }

        tick.window.projection_groups = Some(self.groups);
        Ok(tick)
    }
    fn step_table(&mut self) -> io::Result<()> {
        let table = crate::schema::TABLES[self.table - 1];
        if self.input.is_none() {
            self.work.canonical.raw_admissions += 1;
            let inventory = self.inventory.as_mut().ok_or_else(invalid)?;
            let kind = TABLE_FILES[self.table - 1];
            if inventory.file(kind).is_none() {
                return Err(io::Error::from(io::ErrorKind::Unsupported));
            }

            let file = inventory.reopen_file(kind)?;
            self.input = Some(
                AdmittedTable::from_file_streaming_counted(file, &self.budget, &self.ledger)
                    .map_err(step_table_error)?,
            );
            return Ok(());
        }

        if self.selected == self.observations.len() {
            let input = self.input.as_ref().ok_or_else(invalid)?;
            if self.table_rows != input.row_count() {
                return Err(invalid());
            }

            self.tables.push(crate::TableManifest {
                name: table.name(),
                rows: input.row_count(),
            });
            self.input = None;
            self.table += 1;
            self.selected = 0;
            self.table_rows = 0;
            self.order_offset = 0;
            self.previous_key = None;
            return Ok(());
        }

        if self.tick.is_none() {
            self.tick = Some(self.read_tick(self.selected)?);
            return Ok(());
        }

        if self.step_table_expected(table)? {
            return Ok(());
        }

        let input = self.input.as_ref().ok_or_else(invalid)?;
        if self.offset == input.row_count() {
            let expected = &self.expected.as_ref().ok_or_else(invalid)?.frame;
            self.work.proof_operations += collector::u64_from_usize(expected.width());
            comparison::validate_schema(input, expected)?;
            if self.matched != self.tick.as_ref().ok_or_else(invalid)?.rows(table) {
                return Err(invalid());
            }

            self.table_rows += collector::u64_from_usize(self.matched);
            self.tick = None;
            self.expected = None;
            self.matched = 0;
            self.offset = 0;
            self.selected += 1;
            return Ok(());
        }

        self.step_table_comparison()
    }
    fn step_table_expected(&mut self, table: crate::schema::Table) -> io::Result<bool> {
        let tick = self.tick.as_ref().ok_or_else(invalid)?;
        let expected_rows = tick.rows(table);
        let needed = self.expected.is_none()
            || (self.matched < expected_rows
                && self.matched
                    == self.expected_first
                        + self.expected.as_ref().ok_or_else(invalid)?.frame.height());
        if needed {
            self.expected = None;
            let count = if matches!(table, crate::schema::Table::Processes) {
                expected_rows.saturating_sub(self.matched).min(CHUNK_ROWS)
            } else {
                expected_rows.saturating_sub(self.matched)
            };
            self.work.proof_operations += 1;
            let expected = tick
                .frame(table, self.matched, count, &self.budget)
                .map_err(rows::source_error)?;
            self.expected_first = self.matched;
            self.expected = Some(expected);
        }
        Ok(needed)
    }
    fn step_table_comparison(&mut self) -> io::Result<()> {
        let comparison = Comparison {
            input: self.input.as_ref().ok_or_else(invalid)?,
            expected: &self.expected.as_ref().ok_or_else(invalid)?.frame,
            observations: &self.observations,
            publication: self.observations[self.selected].publication,
            offset: self.offset,
            matched: self.matched,
            expected_first: self.expected_first,
            ledger: &self.ledger,
        };
        if self.column == 0 {
            self.work.proof_operations += 1;
            let count = comparison.count()?;
            self.work.proof_operations += 1;
            let full_work = comparison
                .input
                .slice_read_work_bytes(self.offset, count)
                .map_err(rows::admission_error)?;
            if full_work <= self.budget.maximum() - self.budget.used() {
                let matched = comparison.full(count, &mut self.work)?;
                self.offset += u64::from(count);
                self.matched = matched;
            } else {
                let matches = comparison.select(count, &mut self.work)?;
                self.matches = matches;
                self.chunk_count = count;
                self.column = 1;
            }
        } else if self.column <= comparison.expected.width() {
            comparison.column(
                self.column - 1,
                self.chunk_count,
                &self.matches,
                &mut self.work,
            )?;
            self.column += 1;
        } else {
            self.offset += u64::from(self.chunk_count);
            self.matched += self.matches.len();
            self.matches.clear();
            self.column = 0;
        }
        Ok(())
    }
    fn step_derived(&mut self) -> io::Result<()> {
        let kind = TABLE_FILES[self.table - 1];
        let name = crate::schema::DERIVED_TABLES[self.table - 10];
        let inventory = self.inventory.as_mut().ok_or_else(invalid)?;
        if inventory.file(kind).is_some() {
            self.work.canonical.raw_admissions += 1;
            let file = inventory.reopen_file(kind)?;
            let table =
                AdmittedTable::from_file_streaming_counted(file, &self.budget, &self.ledger)
                    .map_err(rows::admission_error)?;
            self.tables.push(crate::TableManifest {
                name,
                rows: table.row_count(),
            });
        }
        self.table += 1;
        Ok(())
    }
    fn manifest(&self) -> io::Result<crate::Manifest> {
        let inventory = self.inventory.as_ref().ok_or_else(invalid)?;
        let index = inventory.ordinary_index()?;
        let first = self.observations.first().ok_or_else(invalid)?.publication;
        let end = self
            .observations
            .last()
            .ok_or_else(invalid)?
            .publication
            .checked_add(1)
            .ok_or_else(invalid)?;
        let publications = end
            .checked_sub(self.state.input.losses.publication_start)
            .and_then(|count| count.checked_sub(collector::u64_from_usize(self.observations.len())))
            .ok_or_else(invalid)?;
        let events = index
            .end
            .checked_sub(self.state.input.losses.event_start)
            .and_then(|count| count.checked_sub(index.rows))
            .ok_or_else(invalid)?;
        let mut tables = self.tables.clone();
        tables.insert(
            9,
            crate::TableManifest {
                name: "events",
                rows: index.rows,
            },
        );
        Ok(crate::Manifest {
            identity: index.identity,
            publication_first: first,
            publication_end: end,
            events_first: self.state.input.losses.event_start,
            events_end: index.end,
            wall_ns: crate::metadata::clock_bounds(
                self.observations.iter().map(|value| value.wall_ns),
            ),
            continuous_ns: crate::metadata::clock_bounds(
                self.observations.iter().map(|value| value.continuous_ns),
            ),
            awake_ns: crate::metadata::clock_bounds(
                self.observations.iter().map(|value| value.awake_ns),
            ),
            losses: crate::Losses {
                publications,
                events,
                unknown: self.state.input.losses.unknown.is_some(),
            },
            data_directory: format!(
                "data/{}/ready",
                collector::recording::identity_text(self.state.transaction)
            ),
            tables,
            event_batches: Some(u32::try_from(index.batches.len()).map_err(|_| invalid())?),
        })
    }
    fn finish(&mut self) -> io::Result<()> {
        let manifest = self.manifest()?;
        // Ledger owns all predecessor work before the consuming publication owner starts.
        let work = self.snapshot();
        self.ledger.record(&work, &mut self.recorded);
        self.work = work;
        let source = self.source.as_ref().ok_or_else(invalid)?;
        self.publication_recorded.source_reads = source.reads;
        self.publication_recorded.source_observations = source.observations.get();
        self.publication_recorded.proof_operations = source.operations;
        let publication = NormalPublication::recovered(
            &mut self.inventory,
            &manifest,
            &mut self.source,
            &self.budget,
            &self.ledger,
        )?;
        self.publication = Some(publication);
        Ok(())
    }
}
impl Drop for OrdinaryRecovery<'_> {
    fn drop(&mut self) {
        let work = self.snapshot();
        self.ledger.record(&work, &mut self.recorded);
        if let Some(publication) = &self.publication {
            self.ledger
                .record(&publication.work(), &mut self.publication_recorded);
        }
    }
}
const TABLE_FILES: [OriginalFile; 11] = [
    OriginalFile::Ticks,
    OriginalFile::Memory,
    OriginalFile::Cpus,
    OriginalFile::Interfaces,
    OriginalFile::GpuStates,
    OriginalFile::CoreStates,
    OriginalFile::Fans,
    OriginalFile::Processes,
    OriginalFile::Coalitions,
    OriginalFile::AppRanks,
    OriginalFile::AppParts,
];
fn resume_ordinary_proof_owner_directory(
    store: &Store,
    state: &NormalState,
    work: &mut PublicationWork,
) -> io::Result<(
    collector::recording::private::PrivateDirectory,
    super::OriginalPlacement,
)> {
    work.inventory.open_attempts += 1;
    let base = store.directory.open_directory("data")?;
    work.inventory.open_attempts += 1;
    let object =
        base.open_directory(&collector::recording::identity_text(state.transaction).to_string())?;
    drop(base);
    object.visit_names_counted(3, &mut work.inventory.directory, |name| {
        if matches!(name, "pending" | "ready") {
            Ok(())
        } else {
            Err(invalid())
        }
    })?;
    super::inventory::select_original_counted(&object, &mut work.inventory.open_attempts)?
        .ok_or_else(invalid)
}
fn value_error(error: impl std::fmt::Display) -> io::Error {
    crate::metadata::invalid(error)
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

fn step_table_error(error: crate::engine::AdmissionRefusal) -> io::Error {
    match error {
        crate::engine::AdmissionRefusal::Bound("parquet file framing") => {
            io::Error::from(io::ErrorKind::Unsupported)
        }
        error => rows::admission_error(error),
    }
}

fn resume_ordinary_proof_owner<'store>(
    store: &'store Store,
    state: NormalState,
    source: SourceProof,
    permit: StagingPermit,
    budget: &Arc<ByteBudget>,
    ledger: &PublicationLedger,
    reservation: ByteReservation,
) -> io::Result<OrdinaryRecovery<'store>> {
    let mut work = PublicationWork::default();
    let result = resume_ordinary_proof_owner_directory(store, &state, &mut work);
    ledger.record(&work, &mut PublicationWork::default());
    let (directory, placement) = result?;
    let attempt = RegisteredAttempt {
        transaction: state.transaction,
        input: state.input.clone(),
        directory,
        resumed: true,
        candidates: false,
        _permit: permit,
        owner: &store.owner,
        root: &store.directory,
        placement,
    };
    let mut work = PublicationWork::default();
    let result = OriginalInventory::owned_counted(attempt, budget, &mut work.inventory);
    if result.is_err() {
        ledger.record(&work, &mut PublicationWork::default());
    }
    let inventory = result?;
    Ok(OrdinaryRecovery::new(
        inventory,
        source,
        state,
        budget,
        ledger,
        &work,
        reservation,
    ))
}

fn step_ticks_observations(
    frame: &polars::prelude::DataFrame,
) -> io::Result<Vec<crate::Observation>> {
    if frame.height() == 0 || frame.height() > 61 {
        return Err(invalid());
    }

    let mut observations = Vec::with_capacity(61);
    let mut previous = 0;
    for row in 0..frame.height() {
        let publication = frame
            .column("publication")
            .map_err(value_error)?
            .u64()
            .map_err(value_error)?
            .get(row)
            .ok_or_else(invalid)?;
        if publication <= previous {
            return Err(invalid());
        }

        observations.push(crate::Observation {
            publication,
            wall_ns: frame
                .column("wall_ns")
                .map_err(value_error)?
                .u64()
                .map_err(value_error)?
                .get(row),
            continuous_ns: frame
                .column("continuous_ns")
                .map_err(value_error)?
                .u64()
                .map_err(value_error)?
                .get(row),
            awake_ns: frame
                .column("awake_ns")
                .map_err(value_error)?
                .u64()
                .map_err(value_error)?
                .get(row),
        });
        previous = publication;
    }
    Ok(observations)
}
