//! Raw registered candidates. No canonical gate, deletion or lifecycle completion.
#![allow(dead_code)] // Crate-private input and replay inspection seams are exercised by tests.
use super::publication_control::{self, CountedControl};
use super::row_merge::{MergeBatch, MergeStep, RowMerge};
use super::{BatchFact, FileIdentity, PublicationLedger, RegisteredAttempt};
use crate::event_recovery::{Batch, batch, read};
use crate::resources::{ByteBudget, ByteReservation};
use crate::transaction::{normal::NormalState, salvage::RowPlan};
use collector::recording::{
    bootstrap::codec::Phase,
    private::{self, PrivateDirectory},
};
use collector::ring::RingReader;
use std::{io, sync::Arc};

pub(super) struct CandidateHandoff<'store> {
    pub attempt: RegisteredAttempt<'store>,
    pub source: Option<super::normal_proof::SourceProof>,
    pub state: NormalState,
    pub facts: Vec<BatchFact>,
    pub unresolved: u64,
    pub reservation: ByteReservation,
    pub witness: super::inventory::NormalInventory,
}
pub(crate) struct RowCandidates<'store> {
    merge: RowMerge<'store>,
    census: Option<super::inventory::census::io_owner::CensusWriter>,
    census_binding: Option<crate::transaction::census_binding::CensusBinding>,
    state: NormalState,
    intent: Option<NormalState>,
    initial: Option<NormalState>,
    ledger: PublicationLedger,
    budget: Arc<ByteBudget>,
    facts: Vec<BatchFact>,
    scan: usize,
    names: Option<private::PrivateNames>,
    names_candidate: usize,
    name_count: usize,
    dots: usize,
    census_rows: [u64; 2],
    replay: usize,
    pending: Option<MergeBatch>,
    offset: usize,
    candidate: usize,
    ordinal: usize,
    rows: [u64; 2],
    replaying: bool,
    adopted: bool,
    allow_metadata: bool,
    reservation: ByteReservation,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CandidateStep {
    Continue,
    Prepared,
    Unresolved,
}
impl<'store> RegisteredAttempt<'store> {
    pub(crate) fn row_candidates(
        self,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<RowCandidates<'store>> {
        RowCandidates::new(self, reader, budget, ledger)
    }
}
impl super::Store {
    pub(crate) fn resume_row_candidates(
        &self,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<RowCandidates<'_>> {
        ledger.require_budget(budget)?;
        if self
            .staging
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::Acquire,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_err()
        {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let permit = super::StagingPermit(Arc::clone(&self.staging));
        let state = publication_control::read_normal(
            &CountedControl {
                root: &self.directory,
                ledger: Some(ledger),
            },
            self.recording,
        )?;
        if state.salvage.is_some()
            || !matches!(
                state.phase,
                Phase::Registered | Phase::SalvagePlanned | Phase::UnresolvedExtent
            )
            || !matches!(
                state.input.operation,
                crate::transaction::AttemptOperation::Ordinary { .. }
                    | crate::transaction::AttemptOperation::Finite { .. }
            )
        {
            return Err(invalid());
        }
        let mut work = super::PublicationWork::default();
        let result = self.resume_row_original(&state, &mut work);
        ledger.record(&work, &mut super::PublicationWork::default());
        let (directory, placement) = result?;
        let attempt = RegisteredAttempt {
            transaction: state.transaction,
            input: state.input,
            directory,
            resumed: true,
            candidates: false,
            _permit: permit,
            owner: &self.owner,
            root: &self.directory,
            placement,
        };
        RowCandidates::new(attempt, reader, budget, ledger)
    }
    fn resume_row_original(
        &self,
        state: &NormalState,
        work: &mut super::PublicationWork,
    ) -> io::Result<(PrivateDirectory, super::OriginalPlacement)> {
        work.inventory.open_attempts += 1;
        let base = self
            .directory
            .open_directory(super::publication::base_name(&state.input))?;
        work.inventory.open_attempts += 1;
        let object = base
            .open_directory(&collector::recording::identity_text(state.transaction).to_string())?;
        drop(base);
        object.visit_names_counted(6, &mut work.inventory.directory, |name| {
            if [
                "pending",
                "ready",
                "fragment-0",
                "fragment-1",
                "loss",
                "original-census",
            ]
            .contains(&name)
            {
                Ok(())
            } else {
                Err(invalid())
            }
        })?;
        let before = work.inventory.open_attempts;
        let result = if state.input.is_finite() {
            super::inventory::select_finite_original(&object, &mut work.inventory.open_attempts)
        } else {
            super::inventory::select_original_mode(
                &object,
                &mut work.inventory.open_attempts,
                state.row_plan.is_some(),
            )
        };
        work.inventory.reopens += work.inventory.open_attempts - before;
        result?.ok_or_else(invalid)
    }
}
impl<'store> RowCandidates<'store> {
    fn new(
        attempt: RegisteredAttempt<'store>,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<Self> {
        Self::new_optional(attempt, Some(reader), budget, ledger)
    }
    pub(super) fn new_optional(
        mut attempt: RegisteredAttempt<'store>,
        reader: Option<RingReader>,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<Self> {
        ledger.require_budget(budget)?;
        let reservation = budget
            .reserve(8192 * size_of::<BatchFact>())
            .ok_or_else(memory)?;
        let (state, initial) = Self::new_state(&attempt, ledger)?;
        attempt.candidates = state.row_plan.is_some();
        let fixed = state
            .row_plan
            .or_else(|| initial.as_ref().and_then(|pending| pending.row_plan));
        let mut merge = RowMerge::new_optional(attempt, reader, budget, ledger)?;
        if let Some(plan) = fixed {
            merge.fixed(plan);
        }
        Ok(Self {
            merge,
            census: None,
            census_binding: None,
            state,
            intent: None,
            initial,
            ledger: ledger.clone(),
            budget: Arc::clone(budget),
            facts: vec![BatchFact::Unprobed; 8192],
            scan: 0,
            names: None,
            names_candidate: 0,
            name_count: 0,
            dots: 0,
            census_rows: [0; 2],
            replay: 0,
            pending: None,
            offset: 0,
            candidate: 0,
            ordinal: 0,
            rows: [0; 2],
            replaying: false,
            adopted: false,
            allow_metadata: false,
            reservation,
        })
    }
    fn new_state(
        attempt: &RegisteredAttempt<'_>,
        ledger: &PublicationLedger,
    ) -> io::Result<(NormalState, Option<NormalState>)> {
        let state = publication_control::read_normal(
            &CountedControl {
                root: attempt.root,
                ledger: Some(ledger),
            },
            attempt.input.run.recording,
        )?;
        if state.phase == Phase::UnresolvedExtent
            && (!state.input.is_finite() || state.sequence != 0)
        {
            return Err(invalid());
        }

        if state.transaction != attempt.transaction
            || state.input != attempt.input
            || state.salvage.is_some()
            || !matches!(
                state.phase,
                Phase::Registered | Phase::SalvagePlanned | Phase::UnresolvedExtent
            )
        {
            return Err(invalid());
        }
        if !matches!(
            publication_control::read_state(
                &CountedControl {
                    root: attempt.root,
                    ledger: Some(ledger)
                },
                attempt.input.run.recording
            )?,
            collector::recording::bootstrap::codec::NamespaceState::Quiet { .. }
        ) {
            return Err(invalid());
        }
        let initial = publication_control::pending(
            &CountedControl {
                root: attempt.root,
                ledger: Some(ledger),
            },
            &state,
        )?;
        Ok((state, initial))
    }
    pub(super) fn into_publication(self) -> io::Result<CandidateHandoff<'store>> {
        for kind in [
            super::OriginalFile::Ticks,
            super::OriginalFile::Memory,
            super::OriginalFile::Cpus,
            super::OriginalFile::Interfaces,
            super::OriginalFile::GpuStates,
            super::OriginalFile::CoreStates,
            super::OriginalFile::Fans,
            super::OriginalFile::Processes,
            super::OriginalFile::Coalitions,
            super::OriginalFile::AppRanks,
            super::OriginalFile::AppParts,
            super::OriginalFile::Metadata,
        ] {
            if self.merge.inventory.file(kind).is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "original metrics/metadata need independent preservation",
                ));
            }
        }
        let losses = self.losses();
        let header = self.census.as_ref().ok_or_else(invalid)?.header().clone();
        let (attempt, source, mut witness) = self.merge.into_publication()?;
        witness.census_bind_header(&header);
        Ok(CandidateHandoff {
            attempt,
            source,
            state: self.state,
            facts: self.facts,
            unresolved: losses.1,
            witness,
            reservation: self.reservation,
        })
    }
    pub(super) fn mark_unresolved(&mut self) -> io::Result<bool> {
        if !self.state.input.is_finite() || self.state.census.kind != 2 {
            return Ok(false);
        }

        let mut state = self.state.clone();
        state.phase = Phase::UnresolvedExtent;
        self.intent = Some(state);
        self.commit()?;
        Ok(true)
    }
    pub(super) fn publication_metadata(mut self) -> Self {
        self.allow_metadata = true;
        self
    }
    #[cfg(test)]
    pub(crate) fn pending_rows(&self) -> usize {
        self.pending.as_ref().map_or(0, |batch| batch.rows().len())
    }
    pub(crate) fn rows(&self) -> [u64; 2] {
        self.rows
    }
    pub(crate) fn facts(&self) -> &[BatchFact] {
        &self.facts
    }
    pub(crate) fn losses(&self) -> ([u64; 2], u64) {
        (self.merge.losses, self.merge.unresolved)
    }
    pub(crate) fn plan(&self) -> Option<RowPlan> {
        self.state.row_plan
    }
    fn object(&self) -> io::Result<PrivateDirectory> {
        let attempt = self.merge.inventory.owner();
        self.ledger.update(|work| {
            work.inventory.open_attempts += 2;
            work.inventory.reopens += 2;
        });
        let base = attempt
            .root
            .open_directory(super::publication::base_name(&attempt.input))?;
        base.open_directory(&collector::recording::identity_text(attempt.transaction).to_string())
    }
    fn commit(&mut self) -> io::Result<()> {
        let target = self.intent.as_ref().ok_or_else(invalid)?;
        if !publication_control::census_next(&self.state, target) {
            return Err(invalid());
        }
        let mut old = [0; 4096];
        let mut new = [0; 4096];
        self.state.encode(&mut old)?;
        target.encode(&mut new)?;
        let mut syncs = 0;
        self.ledger.update(|work| work.controls += 1);
        let result = publication_control::replace(
            &CountedControl {
                root: self.merge.inventory.owner().root,
                ledger: Some(&self.ledger),
            },
            "foreground",
            &old,
            &new,
            &mut syncs,
        );
        self.ledger.update(|work| work.full_syncs += syncs);
        result?;
        self.state = target.clone();
        if self.state.row_plan.is_some() {
            self.merge.inventory.allow_candidates()?;
        }
        self.intent = None;
        Ok(())
    }
    pub(crate) fn step(&mut self) -> crate::Result<CandidateStep> {
        if self.intent.is_some() {
            self.commit()?;
            return Ok(CandidateStep::Continue);
        }
        if !self.merge.inventory.enumerated() {
            self.merge.step()?;
            return Ok(CandidateStep::Continue);
        }
        if self.census_binding.is_none() {
            self.step_census()?;
            return Ok(CandidateStep::Continue);
        }
        if !self.merge.has_source() && self.state.input.is_finite() {
            if self.state.phase == Phase::Registered {
                self.step_plan()?;
                return Ok(CandidateStep::Continue);
            }

            if self.mark_unresolved()? {
                return Ok(CandidateStep::Unresolved);
            }
            return Err(io::Error::from(io::ErrorKind::WouldBlock).into());
        }
        if self.merge.range().is_none() {
            self.merge.step()?;
            return Ok(CandidateStep::Continue);
        }
        if self.step_plan()? {
            return Ok(CandidateStep::Continue);
        }
        super::inventory::census::io_owner::check_bound(
            self.merge.inventory.owner(),
            &self.state.census,
            &self.ledger,
        )?;
        if self.names_candidate < 2 {
            self.census_names()?;
            return Ok(CandidateStep::Continue);
        }
        if self.scan < 8192 {
            self.census()?;
            return Ok(CandidateStep::Continue);
        }
        if self.replay < 8192 && self.pending.is_none() {
            self.replay_load()?;
            return Ok(CandidateStep::Continue);
        }
        if self.replaying {
            self.step_replay()?;
            return Ok(CandidateStep::Continue);
        }
        if self.pending.is_some() {
            self.write()?;
            return Ok(CandidateStep::Continue);
        }
        self.step_merge()
    }
    fn step_merge(&mut self) -> crate::Result<CandidateStep> {
        let maximum = collector::u64_from_usize(crate::event_observation::ROWS_MAX);
        let remaining = maximum - self.rows[self.candidate];
        // A full candidate transitions before the next physical batch is formed.
        self.merge
            .output_rows_limit(if remaining == 0 { maximum } else { remaining })?;
        match self.merge.step()? {
            MergeStep::Continue => Ok(CandidateStep::Continue),
            MergeStep::Batch => {
                self.pending = self.merge.take_batch();
                self.offset = 0;
                Ok(CandidateStep::Continue)
            }
            MergeStep::Finished if self.rows != [0, 0] || self.state.input.is_finite() => {
                Ok(CandidateStep::Prepared)
            }
            MergeStep::Finished => Err(io::Error::from(io::ErrorKind::WouldBlock).into()),
        }
    }
    fn step_replay(&mut self) -> crate::Result<()> {
        let pending = self.pending.as_ref().ok_or_else(invalid)?;
        let end = (self.offset + 128).min(pending.rows().len());
        let consumed = self.merge.replay_chunk(&pending.rows()[self.offset..end])?;
        if consumed > 0 {
            self.offset += consumed;
        } else if self.merge.replay(&pending.rows()[self.offset])? {
            self.offset += 1;
        }
        if self.offset == pending.rows().len() {
            self.rows[self.candidate] += collector::u64_from_usize(self.offset);
            self.pending = None;
            self.offset = 0;
            self.replaying = false;
            self.ordinal += 1;
            self.replay += 1;
        }
        Ok(())
    }
    fn step_census(&mut self) -> io::Result<()> {
        if self.census.is_none() {
            if self.state.census.kind == 0 {
                self.step_census_admission()?;
            }
            let object = self.object()?;
            let header = self.merge.inventory.census_header(object.device_inode()?)?;
            self.census = Some(super::inventory::census::io_owner::CensusWriter::new(
                header,
                &self.budget,
                &self.ledger,
            )?);
        }
        let retained = self
            .initial
            .as_ref()
            .filter(|state| state.census.kind == 2)
            .map_or(&self.state.census, |state| &state.census);
        if let Some(binding) = self.census.as_mut().ok_or_else(invalid)?.step(
            &*self.merge.inventory,
            self.merge.inventory.owner(),
            retained,
        )? {
            if binding.kind == 1 {
                let mut state = self.state.clone();
                state.census = binding;
                self.intent = Some(state);
            } else {
                self.census_binding = Some(binding);
            }
        }
        Ok(())
    }
    fn step_census_admission(&self) -> io::Result<()> {
        if self.merge.inventory.exceeds_logical_limit() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "ordered original aggregate exceeds logical row bound",
            ));
        }
        if self.merge.inventory.has_overlaps() || self.merge.inventory.has_unordered_extents() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "general overlapping-original merge remains open",
            ));
        }
        Ok(())
    }
    fn step_plan(&mut self) -> crate::Result<bool> {
        if self.state.phase == Phase::Registered && self.state.input.is_finite() {
            let mut target = self.state.clone();
            target.phase = Phase::SalvagePlanned;
            target.census = self.census_binding.ok_or_else(invalid)?;
            self.intent = Some(target);
            return Ok(true);
        }

        if self.state.row_plan.is_none() {
            let range = self.merge.range().ok_or_else(invalid)?;
            let source = self.merge.source_range().ok_or_else(invalid)?;
            if self.merge.inventory.physical_rows() == 0 && source.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "zero survivors require the exact loss proof",
                )
                .into());
            }
            let mut target = self.state.clone();
            target.phase = Phase::SalvagePlanned;
            target.census = self.census_binding.ok_or_else(invalid)?;
            target.row_plan = Some(RowPlan {
                count: 2,
                rows: [0; 2],
                committed: 0,
                first: range.start,
                end: range.end,
                source_first: source.start,
                source_end: source.end,
                layout: target.input.source.layout,
                endpoint: if matches!(self.merge.inventory.index(), super::IndexFact::Valid { .. })
                {
                    1
                } else {
                    2
                },
                split: 0,
            });
            if let Some(initial) = &self.initial {
                if initial.row_plan != target.row_plan {
                    return Err(invalid().into());
                }
            }
            self.intent = Some(target);
            return Ok(true);
        }
        if !self.adopted {
            let mut work = super::PublicationWork::default();
            let result = publication_control::adopt(
                &CountedControl {
                    root: self.merge.inventory.owner().root,
                    ledger: Some(&self.ledger),
                },
                &self.state,
                &mut work,
            );
            self.ledger
                .record(&work, &mut super::PublicationWork::default());
            result?;
            self.adopted = true;
            return Ok(true);
        }
        Ok(false)
    }
    fn census_names(&mut self) -> io::Result<()> {
        let mut work = private::DirectoryWork::default();
        if self.names.is_none() {
            let object = self.object()?;
            self.ledger.update(|work| work.inventory.open_attempts += 1);
            match object.open_directory(&format!("fragment-{}", self.names_candidate)) {
                Ok(directory) => {
                    let result = directory.names(&mut work);
                    self.ledger.update(|total| {
                        total.inventory.directory.open_attempts += work.open_attempts;
                    });
                    work.open_attempts = 0;
                    self.names = Some(result?);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    self.names_candidate += 1;
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
        let result = self.census_names_step(&mut work);
        if result.is_err() {
            self.names = None;
            self.name_count = 0;
            self.dots = 0;
        }
        self.ledger.update(|total| {
            total.inventory.directory.open_attempts += work.open_attempts;
            total.inventory.directory.read_calls += work.read_calls;
            total.inventory.directory.entries += work.entries;
            total.inventory.directory.names += work.names;
        });
        result
    }
    fn census_names_step(&mut self, work: &mut private::DirectoryWork) -> io::Result<()> {
        for _ in 0..128 {
            let mut dot = false;
            let found = self
                .names
                .as_mut()
                .ok_or_else(invalid)?
                .step(work, |name| {
                    dot = name == "." || name == "..";
                    if dot {
                        return if self.dots < 2 {
                            Ok(())
                        } else {
                            Err(invalid())
                        };
                    }
                    if self.allow_metadata && name == "metadata" {
                        return Ok(());
                    }
                    if self.name_count >= 4096 + usize::from(self.allow_metadata) {
                        return Err(invalid());
                    }
                    super::attempt::batch_number(name)?;
                    Ok(())
                })?;
            if !found {
                self.names = None;
                self.names_candidate += 1;
                self.name_count = 0;
                self.dots = 0;
                return Ok(());
            }
            if dot {
                self.dots += 1;
            } else {
                self.name_count += 1;
            }
        }
        Ok(())
    }
    fn census(&mut self) -> crate::Result<()> {
        let candidate = self.scan / 4096;
        let limit = (candidate + 1) * 4096;
        let object = self.object()?;
        self.ledger.update(|work| work.inventory.open_attempts += 1);
        let directory = match object.open_directory(&format!("fragment-{candidate}")) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.facts[self.scan..limit].fill(BatchFact::Missing);
                self.scan = limit;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        drop(object);
        for _ in 0..128 {
            if self.scan == limit {
                return Ok(());
            }
            self.ledger.update(|work| work.inventory.positions += 1);
            self.census_file(&directory)?;
            self.scan += 1;
            if matches!(self.facts[self.scan - 1], BatchFact::Complete { .. }) {
                return Ok(());
            }
        }
        Ok(())
    }
    fn census_file(&mut self, directory: &PrivateDirectory) -> crate::Result<()> {
        let candidate = self.scan / 4096;
        let ordinal = self.scan % 4096;
        self.ledger.update(|work| work.inventory.open_attempts += 1);
        let file = match directory.open_file(&crate::event_recovery::batch_name(ordinal)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.facts[self.scan] = BatchFact::Missing;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        if ordinal > 0 && !matches!(self.facts[self.scan - 1], BatchFact::Complete { .. }) {
            return Err(invalid().into());
        }
        if candidate == 1
            && (self.state.row_plan.ok_or_else(invalid)?.split == 0
                || self.state.row_plan.ok_or_else(invalid)?.count != 2)
        {
            return Err(invalid().into());
        }
        self.ledger
            .update(|work| work.inventory.raw_admissions += 1);
        let identity = FileIdentity::read(&file)?;
        let mut file = file;
        let framed = super::inventory::parquet_framing(&mut file, identity.bytes())?;
        if FileIdentity::read(&file)? != identity {
            return Err(invalid().into());
        }
        if !framed {
            self.facts[self.scan] = BatchFact::Malformed {
                file: identity,
                framed: false,
            };
            return Ok(());
        }
        self.ledger.update(|work| {
            work.inventory.raw_file_clones += 1;
        });
        let mut decoded = 0;
        let result = read::read_original_batch_file(
            file.try_clone()?,
            self.state.input.captured,
            true,
            &self.budget,
            &mut decoded,
        );
        self.ledger
            .update(|work| work.inventory.decoded_bytes += decoded);
        let batch = result?;
        if FileIdentity::read(&file)? != identity {
            return Err(invalid().into());
        }
        let (total, fact) = self.census_file_fact(candidate, ordinal, identity, &batch)?;
        self.census_rows[candidate] = total;
        self.facts[self.scan] = fact;
        Ok(())
    }
    fn census_file_fact(
        &self,
        candidate: usize,
        ordinal: usize,
        identity: FileIdentity,
        batch: &Batch,
    ) -> crate::Result<(u64, BatchFact)> {
        let plan = self.state.row_plan.ok_or_else(invalid)?;
        let interval = plan.interval(candidate);
        if batch.first < interval.start || batch.end > interval.end {
            return Err(invalid().into());
        }
        if ordinal > 0 {
            if let BatchFact::Complete { end, .. } = self.facts[self.scan - 1] {
                if batch.first < end {
                    return Err(invalid().into());
                }
            }
        }
        let total = self.census_rows[candidate]
            .checked_add(batch.rows)
            .ok_or_else(invalid)?;
        if total > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
            || (candidate == 1
                && self.census_rows[0]
                    != collector::u64_from_usize(crate::event_observation::ROWS_MAX))
        {
            return Err(invalid().into());
        }
        let fact = BatchFact::Complete {
            file: identity,
            first: batch.first,
            end: batch.end,
            rows: batch.rows,
        };
        Ok((total, fact))
    }
    fn replay_load(&mut self) -> crate::Result<()> {
        for _ in 0..128 {
            if self.replay == 8192 {
                return Ok(());
            }
            if let BatchFact::Complete {
                file: expected,
                first,
                end,
                rows,
            } = self.facts[self.replay]
            {
                self.candidate = self.replay / 4096;
                self.ordinal = self.replay % 4096;
                self.merge.release_input();
                let object = self.object()?;
                self.ledger.update(|work| {
                    work.inventory.open_attempts += 1;
                    work.inventory.reopens += 1;
                });
                let directory = object.open_directory(&format!("fragment-{}", self.candidate))?;
                self.ledger.update(|work| {
                    work.inventory.open_attempts += 1;
                    work.inventory.reopens += 1;
                });
                let file = directory.open_file(&crate::event_recovery::batch_name(self.ordinal))?;
                if FileIdentity::read(&file)? != expected {
                    return Err(invalid().into());
                }
                let pending = MergeBatch::read(
                    &file,
                    &Batch { first, end, rows },
                    &expected,
                    self.state.input.captured,
                    &self.budget,
                    &self.ledger,
                )?;
                self.sync_existing(&directory, &expected)?;
                self.pending = Some(pending);
                self.replaying = true;
                self.offset = 0;
                return Ok(());
            }
            self.replay += 1;
        }
        self.candidate = usize::from(self.rows[1] > 0);
        self.ordinal = self.facts[self.candidate * 4096..(self.candidate + 1) * 4096]
            .iter()
            .take_while(|fact| matches!(fact, BatchFact::Complete { .. }))
            .count();
        Ok(())
    }
    fn sync_existing(
        &self,
        directory: &PrivateDirectory,
        identity: &FileIdentity,
    ) -> io::Result<()> {
        self.ledger.update(|work| {
            work.inventory.open_attempts += 1;
            work.full_syncs += 1;
        });
        let name = crate::event_recovery::batch_name(self.ordinal);
        let file = directory.open_file(&name)?;
        if FileIdentity::read(&file)? != *identity {
            return Err(invalid());
        }
        private::full_sync(&file)?;
        drop(file);
        self.ledger.update(|work| work.full_syncs += 1);
        directory.full_sync()?;
        self.ledger.update(|work| work.inventory.open_attempts += 1);
        if FileIdentity::read(&directory.open_file(&name)?)? != *identity {
            return Err(invalid());
        }
        Ok(())
    }
    fn write_batch(
        &self,
        directory: &PrivateDirectory,
        name: &str,
        rows: &[batch::Row],
        expected: &mut polars::prelude::DataFrame,
    ) -> crate::Result<FileIdentity> {
        write_batch(
            directory,
            name,
            rows,
            expected,
            self.state.input.captured,
            &self.budget,
            &self.ledger,
        )
    }
    fn write_split(&mut self) -> crate::Result<()> {
        let pending = self.pending.as_ref().ok_or_else(invalid)?;
        if self.candidate != 0 || self.state.row_plan.ok_or_else(invalid)?.count != 2 {
            return Err(invalid().into());
        }
        let mut state = self.state.clone();
        let plan = state.row_plan.as_mut().ok_or_else(invalid)?;
        if plan.split == 0 {
            let next = publication_control::pending(
                &CountedControl {
                    root: self.merge.inventory.owner().root,
                    ledger: Some(&self.ledger),
                },
                &self.state,
            )?;
            plan.split = next
                .and_then(|state| state.row_plan)
                .filter(|plan| plan.split != 0)
                .map_or(pending.rows()[self.offset].event.number, |plan| plan.split);
            if plan.split == 0 || pending.rows()[self.offset].event.number < plan.split {
                return Err(invalid().into());
            }
        } else if pending.rows()[self.offset].event.number < plan.split {
            return Err(invalid().into());
        }
        self.intent = Some(state);
        self.candidate = 1;
        self.ordinal = 0;

        Ok(())
    }
    fn write(&mut self) -> crate::Result<()> {
        let pending = self.pending.as_ref().ok_or_else(invalid)?;
        let maximum = collector::u64_from_usize(crate::event_observation::ROWS_MAX);
        if self.rows[self.candidate] == maximum {
            self.write_split()?;
            return Ok(());
        }
        let count = (pending.rows().len() - self.offset)
            .min(usize::try_from(maximum - self.rows[self.candidate]).map_err(|_| invalid())?);
        if self.ordinal >= 4096 {
            return Err(invalid().into());
        }
        self.merge.release_input();
        let object = self.object()?;
        self.ledger.update(|work| {
            work.canonical.creates += 1;
            work.inventory.open_attempts += 1;
        });
        let mut syncs = 0;
        let result =
            object.create_directory_counted(&format!("fragment-{}", self.candidate), &mut syncs);
        self.ledger.update(|work| work.full_syncs += syncs);
        let directory = result?;
        self.ledger.update(|work| work.full_syncs += 1);
        directory.full_sync()?;
        self.ledger.update(|work| work.full_syncs += 1);
        object.full_sync()?;
        let rows =
            &self.pending.as_ref().ok_or_else(invalid)?.rows()[self.offset..self.offset + count];
        let mut expected = batch::frame(rows, self.state.input.captured)?;
        let name = crate::event_recovery::batch_name(self.ordinal);
        let identity = self.write_batch(&directory, &name, rows, &mut expected)?;
        self.sync_existing(&directory, &identity)?;
        self.facts[self.candidate * 4096 + self.ordinal] = BatchFact::Complete {
            file: identity,
            first: rows[0].event.number,
            end: rows[count - 1].event.number + 1,
            rows: collector::u64_from_usize(count),
        };
        self.rows[self.candidate] += collector::u64_from_usize(count);
        self.ordinal += 1;
        self.offset += count;
        if self.offset == self.pending.as_ref().ok_or_else(invalid)?.rows().len() {
            self.pending = None;
            self.offset = 0;
        }
        Ok(())
    }
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
fn memory() -> io::Error {
    io::Error::from(io::ErrorKind::OutOfMemory)
}

fn remove_torn(
    directory: &PrivateDirectory,
    name: &str,
    ledger: &PublicationLedger,
) -> io::Result<()> {
    ledger.update(|work| work.inventory.open_attempts += 1);
    let mut file = match directory.open_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let identity = FileIdentity::read(&file)?;
    if super::inventory::parquet_framing(&mut file, identity.bytes())? {
        return Ok(());
    }

    ledger.update(|work| work.inventory.open_attempts += 1);
    if FileIdentity::read(&directory.open_file(name)?)? != identity {
        return Err(invalid());
    }
    ledger.update(|work| work.canonical.removals += 1);
    directory.remove_file(name)?;
    ledger.update(|work| work.full_syncs += 1);
    directory.full_sync()
}
pub(super) fn write_batch(
    directory: &PrivateDirectory,
    name: &str,
    rows: &[batch::Row],
    expected: &mut polars::prelude::DataFrame,
    captured: bool,
    budget: &Arc<ByteBudget>,
    ledger: &PublicationLedger,
) -> crate::Result<FileIdentity> {
    let count = rows.len();
    remove_torn(directory, name, ledger)?;
    ledger.update(|work| work.inventory.open_attempts += 1);
    let identity = match directory.open_file(name) {
        Ok(file) => {
            let identity = FileIdentity::read(&file)?;
            let batch = Batch {
                first: rows[0].event.number,
                end: rows[count - 1].event.number + 1,
                rows: collector::u64_from_usize(count),
            };
            ledger.update(|work| work.inventory.raw_admissions += 1);
            let mut decoded = 0;
            ledger.update(|work| work.inventory.raw_file_clones += 1);
            let result = read::read_batch_file_counted(
                file.try_clone()?,
                &batch,
                captured,
                true,
                budget,
                &mut decoded,
            );
            ledger.update(|work| work.inventory.decoded_bytes += decoded);
            let actual = result?;
            if FileIdentity::read(&file)? != identity {
                return Err(invalid().into());
            }
            if !actual.equals_missing(expected) {
                return Err(crate::Error::Bound("conflicting retained candidate batch"));
            }
            identity
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ledger.update(|work| work.canonical.creates += 1);
            let mut file = directory.create_file(name)?;
            crate::encoder::encode_write_event_batch(
                expected,
                &mut super::publication_ledger::LedgerWriter {
                    file: &mut file,
                    ledger,
                },
            )?;
            FileIdentity::read(&file)?
        }
        Err(error) => return Err(error.into()),
    };
    Ok(identity)
}
