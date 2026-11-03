//! Event-only ordinary salvage; canonical gates precede exact redundant cleanup.
use super::{
    Arc, AttemptOperation, Body, ByteBudget, ByteReservation, CanonicalKey, CanonicalKind,
    CanonicalLoss, CanonicalMetadata, EndpointEvidence, Lifecycle, Namespace, NamespaceState,
    NormalPublication, Ordering, OriginalPlacement, Phase, PrivateDirectory, PublicationWork,
    RawBase, RawChild, RegisteredAttempt, SlotKind, StagingPermit, Store, Validation, codec,
    insert, invalid, io, private, rows, sync_directory,
};
use crate::event_recovery::{Batch, Manifest as EventManifest, Recovery, batch, read};
use crate::store::row_candidates::{CandidateStep, RowCandidates};
use crate::transaction::Predecessor;
use collector::recording::private::PrivateNames;
use collector::ring::RingReader;

#[derive(PartialEq, Eq)]
enum CleanupNames {
    Unchecked,
    Initial,
    Final,
}
pub struct RowPublication<'store> {
    candidates: Option<RowCandidates<'store>>,
    inner: Option<NormalPublication<'store>>,
    metadata: Vec<CanonicalMetadata>,
    facts: Vec<crate::store::BatchFact>,
    counts: [[u64; 2]; 2],
    scan: usize,
    identity_position: usize,
    source: Option<super::super::normal_proof::SourceProof>,
    proved: Option<bool>,
    transport: bool,
    verified: bool,
    adopted: bool,
    facts_reservation: Option<ByteReservation>,
    comparison_reservation: Option<ByteReservation>,
    written: usize,
    budget: Arc<ByteBudget>,
    ledger: super::super::PublicationLedger,
    recorded: PublicationWork,
    names: Option<PrivateNames>,
    names_state: CleanupNames,
    name_count: usize,
    name_dots: usize,
    candidate_names: usize,
    original: Option<crate::store::row_merge::MergeBatch>,
    original_identity: Option<crate::store::FileIdentity>,
    original_witness: Option<super::super::inventory::NormalInventory>,
    census_reader: Option<super::super::inventory::census::io_owner::CensusReader>,
    cleanup_admission: usize,
    original_row: usize,
    torn_pending: Option<u32>,
    comparison: Option<polars::prelude::DataFrame>,
    comparison_index: usize,
    _scratch: ByteReservation,
}
#[derive(Debug, PartialEq, Eq)]
pub enum RowPublicationStep {
    Continue,
    Idle,
    Unresolved,
}
impl<'store> RegisteredAttempt<'store> {
    pub fn recover_rows(
        self,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<RowPublication<'store>> {
        let candidates = self
            .row_candidates(reader, budget, ledger)?
            .publication_metadata();
        RowPublication::blank(Some(candidates), None, budget, ledger)
    }
}
impl<'store> RowPublication<'store> {
    fn blank(
        candidates: Option<RowCandidates<'store>>,
        inner: Option<NormalPublication<'store>>,
        budget: &Arc<ByteBudget>,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<Self> {
        let scratch = budget
            .reserve(256 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(Self {
            candidates,
            inner,
            metadata: Vec::with_capacity(2),
            facts: Vec::new(),
            counts: [[0; 2]; 2],
            scan: 0,
            identity_position: 0,
            source: None,
            proved: None,
            transport: true,
            verified: false,
            adopted: false,
            facts_reservation: None,
            comparison_reservation: None,
            written: 0,
            budget: Arc::clone(budget),
            ledger: ledger.clone(),
            recorded: PublicationWork::default(),
            names: None,
            names_state: CleanupNames::Unchecked,
            name_count: 0,
            name_dots: 0,
            candidate_names: 0,
            original: None,
            original_identity: None,
            original_witness: None,
            census_reader: None,
            cleanup_admission: 0,
            original_row: 0,
            torn_pending: None,
            comparison: None,
            comparison_index: usize::MAX,
            _scratch: scratch,
        })
    }
    pub fn step(&mut self) -> io::Result<RowPublicationStep> {
        let result = self.step_inner();
        if let Some(inner) = &mut self.inner {
            if let Some(source) = &self.source {
                inner.work.source_reads = source.reads;
                inner.work.source_observations = source.observations.get();
            }
            self.ledger.record(&inner.work, &mut self.recorded);
        }
        result
    }
    fn step_inner(&mut self) -> io::Result<RowPublicationStep> {
        if self.inner.as_ref().is_some_and(|inner| inner.done) {
            return Ok(RowPublicationStep::Idle);
        }
        if let Some(candidates) = &mut self.candidates {
            if candidates.step().map_err(rows::source_error)? != CandidateStep::Prepared {
                return Ok(RowPublicationStep::Continue);
            }
            self.adopt_candidates()?;
            return Ok(RowPublicationStep::Continue);
        }
        if !self.adopted {
            self.adopt()?;
            return Ok(RowPublicationStep::Continue);
        }
        let retired = self
            .inner
            .as_ref()
            .ok_or_else(invalid)?
            .state
            .cleanup
            .region
            == 8;
        if self.original_witness.is_none() && !retired {
            self.load_witness()?;
            return Ok(RowPublicationStep::Continue);
        }
        if !retired {
            self.step_inner_witness()?;
        }
        if self.candidate_names < self.metadata.len() {
            self.census_metadata_names()?;
            return Ok(RowPublicationStep::Continue);
        }
        if self.identity_position < self.facts.len() {
            self.census_metadata_identities()?;
            return Ok(RowPublicationStep::Continue);
        }
        if self.scan < self.facts.len() {
            self.admit_candidate()?;
            return Ok(RowPublicationStep::Continue);
        }
        if self.proved.is_some() && !self.verified {
            self.verify_counts()?;
            self.verified = true;
            return Ok(RowPublicationStep::Continue);
        }
        if self.proved.is_none() {
            self.prove()?;
            return Ok(RowPublicationStep::Continue);
        }
        if self.written < self.metadata.len() {
            self.write_metadata()?;
            return Ok(RowPublicationStep::Continue);
        }
        if let Some(state) = self.step_inner_counts()? {
            self.inner
                .as_mut()
                .ok_or_else(invalid)?
                .commit_state(state)?;
            return Ok(RowPublicationStep::Continue);
        }
        self.step_phase()
    }
    fn step_inner_witness(&self) -> io::Result<()> {
        let inner = self.inner.as_ref().ok_or_else(invalid)?;
        super::super::inventory::census::io_owner::check_bound(
            inner.attempt.as_ref().ok_or_else(invalid)?,
            &inner.state.census,
            &self.ledger,
        )
    }
    fn step_inner_counts(&self) -> io::Result<Option<crate::transaction::normal::NormalState>> {
        let inner = self.inner.as_ref().ok_or_else(invalid)?;
        if inner.state.row_plan.ok_or_else(invalid)?.rows != [0, 0] {
            return Ok(None);
        }
        let mut state = inner.state.clone();
        state.row_plan.as_mut().ok_or_else(invalid)?.rows = [self.counts[0][0], self.counts[1][0]];
        state.original_end = self.metadata[0].original_end;
        state.loss = self.metadata[0].loss.clone();
        state.loss.inherited = state.input.losses.clone();
        state.loss.event_end = state.row_plan.ok_or_else(invalid)?.end;
        state.loss.events_new = self
            .metadata
            .iter()
            .map(|metadata| metadata.loss.events_new)
            .sum();
        Ok(Some(state))
    }
    fn load_witness(&mut self) -> io::Result<()> {
        if self.census_reader.is_none() {
            self.census_reader = Some(
                super::super::inventory::census::io_owner::CensusReader::new(
                    &self.budget,
                    &self.ledger,
                )?,
            );
        }
        let inner = self.inner.as_ref().ok_or_else(invalid)?;
        if let Some(witness) = self.census_reader.as_mut().ok_or_else(invalid)?.step(
            inner.attempt.as_ref().ok_or_else(invalid)?,
            &inner.state.census,
            &self.budget,
        )? {
            self.original_witness = Some(witness);
            self.census_reader = None;
        }
        Ok(())
    }
    fn step_phase(&mut self) -> io::Result<RowPublicationStep> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        match inner.state.phase {
            Phase::SalvagePlanned => inner.acquire_busy()?,
            Phase::Publishing => inner.publish_link()?,
            Phase::Published => {
                inner.row_unresolved = !self.transport || !self.proved.ok_or_else(invalid)?;
                inner.finish_quiet()?;
            }
            Phase::UnresolvedExtent => return Ok(RowPublicationStep::Unresolved),
            Phase::SalvageCommitted if !self.transport && inner.state.cleanup.region != 8 => {
                let mut state = inner.state.clone();
                state.phase = Phase::UnresolvedExtent;
                inner.commit_state(state)?;
            }
            Phase::SalvageCommitted => self.cleanup()?,
            _ => return Err(invalid()),
        }
        Ok(if self.inner.as_ref().ok_or_else(invalid)?.done {
            RowPublicationStep::Idle
        } else {
            RowPublicationStep::Continue
        })
    }
    fn adopt(&mut self) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
        super::super::publication_control::adopt(
            &super::super::publication_control::CountedControl {
                root: attempt.root,
                ledger: Some(&self.ledger),
            },
            &inner.state,
            &mut inner.work,
        )?;
        self.adopted = true;
        Ok(())
    }
    fn adopt_candidates(&mut self) -> io::Result<()> {
        let handoff = self
            .candidates
            .take()
            .ok_or_else(invalid)?
            .into_publication()?;
        if handoff.unresolved != 0 {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let mut source = handoff.source;
        source.reads = 0;
        source.observations.set(0);
        source.operations = 0;
        let reservation = self
            .budget
            .reserve(256 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let mut inner =
            NormalPublication::blank(handoff.attempt, handoff.state, &self.budget, reservation);
        inner.ledger = Some(self.ledger.clone());
        inner.ready = true;
        inner.validation = Validation::Complete;
        self.inner = Some(inner);
        self.adopted = true;
        self.candidate_names = 2;
        self.source = Some(source);
        self.facts = handoff.facts;
        self.original_witness = Some(handoff.witness);
        self.identity_position = self.facts.len();
        self.facts_reservation = Some(handoff.reservation);
        Ok(())
    }
    fn census_metadata_names(&mut self) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        if self.names.is_none() {
            self.name_count = 0;
            self.name_dots = 0;
            let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
            inner.work.inventory.open_attempts += 1;
            let directory = Self::object(attempt, &mut inner.work)?
                .open_directory(&format!("fragment-{}", self.candidate_names))?;
            self.names = Some(directory.names(&mut inner.work.inventory.directory)?);
        }
        let Body::Events(manifest) = &self.metadata[self.candidate_names].body else {
            return Err(invalid());
        };
        for _ in 0..128 {
            let result = self.names.as_mut().ok_or_else(invalid)?.step(
                &mut inner.work.inventory.directory,
                |name| {
                    if matches!(name, "." | "..") {
                        self.name_dots += 1;
                    } else if name != "metadata" {
                        let ordinal = super::super::attempt::batch_number(name)?;
                        if usize::try_from(ordinal).map_err(|_| invalid())?
                            >= manifest.batches.len()
                        {
                            return Err(invalid());
                        }
                    }
                    Ok(())
                },
            );
            match result {
                Ok(false) => {
                    self.names = None;
                    self.candidate_names += 1;
                    return Ok(());
                }
                Ok(true) => {
                    self.name_count += 1;
                    if self.name_count > 4099 || self.name_dots > 2 {
                        return Err(invalid());
                    }
                }
                Err(error) => {
                    self.names = None;
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    fn admit_candidate(&mut self) -> io::Result<()> {
        for _ in 0..128 {
            if self.scan == self.facts.len() {
                return Ok(());
            }
            if let crate::store::BatchFact::Complete {
                file: expected,
                first,
                end,
                rows: count,
            } = self.facts[self.scan]
            {
                let inner = self.inner.as_mut().ok_or_else(invalid)?;
                let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
                let candidate = self.scan / 4096;
                let ordinal = self.scan % 4096;
                inner.work.inventory.open_attempts += 1;
                let directory = Self::object(attempt, &mut inner.work)?
                    .open_directory(&format!("fragment-{candidate}"))?;
                inner.work.inventory.open_attempts += 1;
                let file = directory.open_file(&crate::event_recovery::batch_name(ordinal))?;
                if crate::store::FileIdentity::read(&file)? != expected {
                    return Err(invalid());
                }
                inner.work.inventory.raw_file_clones += 1;
                inner.work.inventory.raw_admissions += 1;
                let _frame = self
                    .budget
                    .reserve(batch::WORKING_BYTES)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
                let frame = read::read_batch_file_counted(
                    file.try_clone()?,
                    &Batch {
                        first,
                        end,
                        rows: count,
                    },
                    attempt.input.captured,
                    true,
                    &self.budget,
                    &mut inner.work.inventory.decoded_bytes,
                )
                .map_err(rows::source_error)?;
                if crate::store::FileIdentity::read(&file)? != expected {
                    return Err(invalid());
                }
                let events = frame
                    .column("event")
                    .map_err(|error| io::Error::other(error.to_string()))?
                    .u64()
                    .map_err(|error| io::Error::other(error.to_string()))?;
                let total = count;
                let old = collector::u64_from_usize(
                    events
                        .into_no_null_iter()
                        .filter(|event| *event < attempt.input.losses.event_inherited_end)
                        .count(),
                );
                inner.work.full_syncs += 1;
                private::full_sync(&file)?;
                sync_directory(&directory, &mut inner.work)?;
                self.counts[candidate][0] += total;
                self.counts[candidate][1] += old;
                self.scan += 1;
                return Ok(());
            }
            self.scan += 1;
        }
        Ok(())
    }
    fn object(
        attempt: &RegisteredAttempt<'_>,
        work: &mut PublicationWork,
    ) -> io::Result<PrivateDirectory> {
        work.inventory.open_attempts += 1;
        let base = attempt.root.open_directory("data")?;
        work.inventory.open_attempts += 1;
        base.open_directory(&collector::recording::identity_text(attempt.transaction).to_string())
    }
    fn verify_counts(&self) -> io::Result<()> {
        let inner = self.inner.as_ref().ok_or_else(invalid)?;
        let hints = inner.state.row_plan.ok_or_else(invalid)?.rows;
        if hints != [0, 0] && hints != [self.counts[0][0], self.counts[1][0]] {
            return Err(invalid());
        }
        for (candidate, metadata) in self.metadata.iter().enumerate() {
            let Body::Events(manifest) = &metadata.body else {
                return Err(invalid());
            };
            let old = &metadata.loss.inherited;
            if self.counts[candidate][0] != manifest.recovery.rows
                || self.counts[candidate][1] + old.events
                    != old.event_inherited_end - old.event_start
                || self.counts[candidate][0] - self.counts[candidate][1] + metadata.loss.events_new
                    != metadata.loss.event_end - old.event_inherited_end
            {
                return Err(invalid());
            }
        }
        if self
            .metadata
            .iter()
            .map(|metadata| metadata.loss.inherited.events)
            .sum::<u64>()
            != inner.state.input.losses.events
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn prove(&mut self) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        let input = &inner.state.input;
        let plan = inner.state.row_plan.ok_or_else(invalid)?;
        let source = self.source.as_mut().ok_or_else(invalid)?;
        let end = source.ordinary_end(input, plan.end, &self.budget);
        inner.work.source_reads = source.reads;
        inner.work.source_observations = source.observations.get();
        let (publication_end, proved) = match end {
            Ok(end) => (end, true),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                (input.losses.publication_inherited_end, false)
            }
            Err(error) => return Err(error),
        };
        let expected_losses = input.losses.events;
        self.metadata.clear();
        for candidate in 0..2 {
            if self.counts[candidate][0] == 0 {
                continue;
            }
            let metadata = self.build_metadata(candidate, publication_end, proved)?;
            self.metadata.push(metadata);
        }
        if self
            .metadata
            .iter()
            .map(|metadata| metadata.loss.inherited.events)
            .sum::<u64>()
            != expected_losses
        {
            return Err(invalid());
        }
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        let input = &inner.state.input;
        inner.links = self
            .metadata
            .iter()
            .map(|metadata| {
                Ok((
                    Namespace::Fragments,
                    CanonicalKey::source(
                        Namespace::Fragments,
                        &input.run,
                        metadata.loss.inherited.event_start,
                    )?,
                ))
            })
            .collect::<io::Result<_>>()?;
        self.proved = Some(proved);
        Ok(())
    }
    fn build_metadata(
        &self,
        candidate: usize,
        publication_end: u64,
        proved: bool,
    ) -> io::Result<CanonicalMetadata> {
        let inner = self.inner.as_ref().ok_or_else(invalid)?;
        let input = &inner.state.input;
        let plan = inner.state.row_plan.ok_or_else(invalid)?;
        let std::ops::Range { start: first, end } = plan.interval(candidate);
        let loss = self.partition_loss(candidate, first, end, publication_end)?;
        let batches = self.metadata_batches(candidate);
        let AttemptOperation::Ordinary {
            window,
            attempted_origin,
        } = input.operation
        else {
            return Err(invalid());
        };
        let metadata = CanonicalMetadata {
            run: input.run,
            transaction: inner.state.transaction,
            kind: CanonicalKind::Fragment,
            base: RawBase::Data,
            child: if candidate == 0 {
                RawChild::Fragment0
            } else {
                RawChild::Fragment1
            },
            window,
            attempted_origin,
            canonical_origin: 0,
            original_end: if proved { publication_end } else { 0 },
            binding: input.binding,
            source_layout: input.source.layout,
            source_origin: input.source.origin_continuous_ns,
            evidence: EndpointEvidence::SurvivingRow,
            lifecycle: Lifecycle::Active,
            captured: input.captured,
            trailing_unknown: !proved,
            loss: loss.clone(),
            body: Body::Events(EventManifest {
                recovery: Recovery {
                    run: input.run,
                    events_first: first,
                    events_end: end,
                    rows: self.counts[candidate][0],
                    losses: loss.totals()?,
                },
                identities: input.captured,
                batches,
            }),
        };
        metadata.validate()?;
        Ok(metadata)
    }
    fn metadata_batches(&self, candidate: usize) -> Vec<Batch> {
        self.facts[candidate * 4096..(candidate + 1) * 4096]
            .iter()
            .filter_map(|fact| match fact {
                crate::store::BatchFact::Complete {
                    first, end, rows, ..
                } => Some(Batch {
                    first: *first,
                    end: *end,
                    rows: *rows,
                }),
                _ => None,
            })
            .collect()
    }
    fn partition_loss(
        &self,
        candidate: usize,
        first: u64,
        end: u64,
        publication_end: u64,
    ) -> io::Result<CanonicalLoss> {
        let inner = self.inner.as_ref().ok_or_else(invalid)?;
        let input = &inner.state.input;
        let plan = inner.state.row_plan.ok_or_else(invalid)?;
        let mut inherited = input.losses.clone();
        inherited.event_start = first;
        inherited.event_inherited_end = inherited.event_inherited_end.clamp(first, end);
        inherited.events = inherited.event_inherited_end - first - self.counts[candidate][1];
        if candidate == 1 {
            inherited.predecessor = Predecessor::Canonical {
                transaction: inner.state.transaction,
                kind: CanonicalKind::Fragment,
                origin: plan.first,
            };
            inherited.publication_start = publication_end;
            inherited.publication_inherited_end = publication_end;
            inherited.publications = 0;
        }
        Ok(CanonicalLoss {
            publications_new: if candidate == 0 {
                publication_end - inherited.publication_inherited_end
            } else {
                0
            },
            events_new: end
                - inherited.event_inherited_end
                - (self.counts[candidate][0] - self.counts[candidate][1]),
            inherited,
            publication_end,
            event_end: end,
            unknown_flags: u32::from(input.losses.unknown.is_some()),
        })
    }
    fn write_metadata(&mut self) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
        inner.work.inventory.open_attempts += 1;
        let directory = Self::object(attempt, &mut inner.work)?
            .open_directory(&format!("fragment-{}", self.written))?;
        super::write_metadata_at(
            attempt,
            &directory,
            &self.metadata[self.written],
            &mut inner.work,
            &inner.state,
            Some(&self.ledger),
        )?;
        self.written += 1;
        Ok(())
    }
    fn cleanup(&mut self) -> io::Result<()> {
        let inner = self.inner.as_ref().ok_or_else(invalid)?;
        if inner.state.cleanup.region != 8 && self.cleanup_admission < 4109 {
            self.cleanup_admission = self.cleanup_admission_pass()?;
            return Ok(());
        }
        if let Some(source) = &self.source {
            source.check(&self.inner.as_ref().ok_or_else(invalid)?.state.input)?;
        }
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        Self::require_quiet(inner, &self.ledger)?;
        if inner.link_next < inner.links.len() {
            Self::cleanup_verify_gate(inner)?;
            inner.link_next += 1;
            return Ok(());
        }
        if inner.state.cleanup.region == 8 {
            return self.clear();
        }
        let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
        let object = Self::object(attempt, &mut inner.work)?;
        let before = inner.work.inventory.open_attempts;
        let original = super::super::inventory::select_original_mode(
            &object,
            &mut inner.work.inventory.open_attempts,
            true,
        )?;
        inner.work.inventory.reopens += inner.work.inventory.open_attempts - before;
        let Some((original, placement)) = original else {
            if inner.state.cleanup.directory != 1 && inner.state.cleanup.region != 8 {
                return Err(invalid());
            }
            return self.clear();
        };
        original.visit_names_counted(
            1,
            &mut inner.work.inventory.directory,
            cleanup_original_name,
        )?;
        inner.work.inventory.open_attempts += 1;
        let events = match original.open_directory("events") {
            Ok(events) => Some(events),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if let Some(events) = events {
            if let Some(witness) = &self.original_witness {
                witness.cleanup_directories(&original, &events)?;
            }
            if self.names_state == CleanupNames::Unchecked {
                return self.cleanup_names(&events);
            }
            if inner.state.cleanup.ordinal < 4096 {
                return self.cleanup_batch(&events);
            }
            if self.names_state != CleanupNames::Final {
                return self.cleanup_final_names(&events);
            }
            return self.cleanup_events_end(&original, events);
        }
        drop(original);
        inner.work.cleanup_directory_attempts += 1;
        inner.work.cleanup_removals += 1;
        object.remove_directory(match placement {
            OriginalPlacement::Pending => "pending",
            OriginalPlacement::Ready => "ready",
        })?;
        sync_directory(&object, &mut inner.work)?;
        self.clear()
    }
    fn cleanup_admission_pass(&self) -> io::Result<usize> {
        let inner = self.inner.as_ref().ok_or_else(invalid)?;
        let next = self
            .original_witness
            .as_ref()
            .ok_or_else(invalid)?
            .census_cleanup_pass(
                inner.attempt.as_ref().ok_or_else(invalid)?,
                &inner.state,
                self.cleanup_admission,
                &self.ledger,
            )?;
        Ok(next)
    }
    fn cleanup_verify_gate(inner: &mut NormalPublication<'_>) -> io::Result<()> {
        inner.select_row_source(inner.link_next)?;
        let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
        let (namespace, key) = inner.links[inner.link_next];
        insert::verify(
            attempt.root,
            &attempt.directory,
            namespace,
            &key,
            &mut inner.work.canonical,
        )?;
        Ok(())
    }
    fn require_quiet(
        inner: &NormalPublication<'_>,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<()> {
        let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
        if super::super::publication_control::read_state(
            &super::super::publication_control::CountedControl {
                root: attempt.root,
                ledger: Some(ledger),
            },
            attempt.input.run.recording,
        )? != (NamespaceState::Quiet {
            sequence: inner.state.sequence.checked_add(1).ok_or_else(invalid)?,
        }) {
            return Err(invalid());
        }
        Ok(())
    }
    fn cleanup_events_end(
        &mut self,
        original: &PrivateDirectory,
        events: PrivateDirectory,
    ) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
        let mut next = inner.state.clone();
        inner.work.inventory.open_attempts += 1;
        if let Some(file) = Self::optional_file(&events, "index")? {
            let mut file = file;
            let identity = crate::store::FileIdentity::read(&file)?;
            let AttemptOperation::Ordinary { window, .. } = attempt.input.operation else {
                return Err(invalid());
            };
            let malformed = self
                .original_witness
                .as_ref()
                .map(|witness| witness.cleanup_index(&file))
                .transpose()?
                .unwrap_or(false);
            let result = crate::event_table::index::Index::read_attempt_file(
                &mut file,
                &crate::WindowIdentity {
                    run: attempt.input.run,
                    window,
                },
                Some(attempt.input.captured),
            );
            let plan = inner.state.row_plan.ok_or_else(invalid)?;
            match result {
                Ok(index) if !malformed && index.first == plan.first && index.end == plan.end => {}
                Err(crate::Error::Bound(_)) if malformed => {}
                Err(error) => return Err(rows::source_error(error)),
                Ok(_) => return Err(invalid()),
            }
            if crate::store::FileIdentity::read(&file)? != identity {
                return Err(invalid());
            }
            inner.work.cleanup_file_attempts += 1;
            inner.work.inventory.open_attempts += 1;
            let current = events.open_file("index")?;
            if crate::store::FileIdentity::read(&current)? != identity {
                return Err(invalid());
            }
            inner.work.cleanup_removals += 1;
            events.remove_file("index")?;
        }
        sync_directory(&events, &mut inner.work)?;
        drop(events);
        inner.work.cleanup_directory_attempts += 1;
        inner.work.cleanup_removals += 1;
        original.remove_directory("events")?;
        sync_directory(original, &mut inner.work)?;
        next.cleanup.directory = 1;
        inner.commit_state(next)?;
        Ok(())
    }
    fn cleanup_names(&mut self, events: &PrivateDirectory) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        if self.names.is_none() {
            self.name_count = 0;
            self.name_dots = 0;
            self.names = Some(events.names(&mut inner.work.inventory.directory)?);
        }
        for _ in 0..128 {
            let result = self.names.as_mut().ok_or_else(invalid)?.step(
                &mut inner.work.inventory.directory,
                |name| {
                    if matches!(name, "." | "..") {
                        self.name_dots += 1;
                        Ok(())
                    } else if name == "index" {
                        Ok(())
                    } else {
                        super::super::attempt::batch_number(name).map(|_| ())
                    }
                },
            );
            match result {
                Ok(false) => {
                    self.names = None;
                    self.names_state = CleanupNames::Initial;
                    return Ok(());
                }
                Ok(true) => {
                    self.name_count += 1;
                    if self.name_count > 4099 || self.name_dots > 2 {
                        return Err(invalid());
                    }
                }
                Err(error) => {
                    self.names = None;
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    fn cleanup_final_names(&mut self, events: &PrivateDirectory) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        if self.names.is_none() {
            self.name_count = 0;
            self.name_dots = 0;
            self.names = Some(events.names(&mut inner.work.inventory.directory)?);
        }
        for _ in 0..128 {
            let next = self.names.as_mut().ok_or_else(invalid)?.step(
                &mut inner.work.inventory.directory,
                |name| {
                    if matches!(name, "." | ".." | "index") {
                        Ok(())
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "new original behind cleanup cursor",
                        ))
                    }
                },
            );
            match next {
                Ok(false) => {
                    self.names = None;
                    self.names_state = CleanupNames::Final;
                    return Ok(());
                }
                Ok(true) => {
                    self.name_count += 1;
                    if self.name_count > 3 {
                        return Err(invalid());
                    }
                }
                Err(error) => {
                    self.names = None;
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    fn optional_file(
        directory: &PrivateDirectory,
        name: &str,
    ) -> io::Result<Option<std::fs::File>> {
        match directory.open_file(name) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
    fn cleanup_batch(&mut self, events: &PrivateDirectory) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        inner.work.cleanup_positions += 1;
        inner.work.inventory.open_attempts += 1;
        let ordinal = inner.state.cleanup.ordinal;
        let name =
            crate::event_recovery::batch_name(usize::try_from(ordinal).map_err(|_| invalid())?);
        if self.original.is_none() {
            inner.work.cleanup_file_attempts += 1;
            inner.work.inventory.open_attempts += 1;
            if let Some(mut file) = Self::optional_file(events, &name)? {
                if let Some(witness) = &self.original_witness {
                    if witness.cleanup_batch(
                        usize::try_from(ordinal).map_err(|_| invalid())?,
                        &mut file,
                    )? {
                        self.torn_pending = Some(ordinal);
                        return self.cleanup_torn(events, &name);
                    }
                }
                self.admit_original(&file)?;
            } else {
                if self.torn_pending != Some(ordinal)
                    && !matches!(inner.state.cleanup.region, 0 | 1)
                {
                    if let Some(witness) = &self.original_witness {
                        witness.cleanup_absent(usize::try_from(ordinal).map_err(|_| invalid())?)?;
                    }
                }
                return self.cleanup_absent(events);
            }
        }
        self.cleanup_compare()?;
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        if self.original_row < self.original.as_ref().ok_or_else(invalid)?.rows().len() {
            return Ok(());
        }
        if let Some(file) = Self::optional_file(events, &name)? {
            if crate::store::FileIdentity::read(&file)?
                != self.original_identity.ok_or_else(invalid)?
            {
                return Err(invalid());
            }
            inner.work.cleanup_file_attempts += 1;
            inner.work.cleanup_removals += 1;
            events.remove_file(&name)?;
        }
        sync_directory(events, &mut inner.work)?;
        let mut state = inner.state.clone();
        state.cleanup.region = 1;
        state.cleanup.ordinal += 1;
        inner.commit_state(state)?;
        self.original = None;
        self.comparison = None;
        self.comparison_reservation = None;
        self.original_identity = None;
        Ok(())
    }
    fn admit_original(&mut self, file: &std::fs::File) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        let identity = crate::store::FileIdentity::read(file)?;
        inner.work.inventory.raw_file_clones += 1;
        inner.work.inventory.raw_admissions += 1;
        let fact = {
            let _frame = self
                .budget
                .reserve(batch::WORKING_BYTES)
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
            read::read_original_batch_file(
                file.try_clone()?,
                inner.state.input.captured,
                true,
                &self.budget,
                &mut inner.work.inventory.decoded_bytes,
            )
            .map_err(rows::source_error)?
        };
        self.original = Some(
            super::super::row_merge::MergeBatch::read(
                file,
                &fact,
                &identity,
                inner.state.input.captured,
                &self.budget,
                &self.ledger,
            )
            .map_err(rows::source_error)?,
        );
        self.original_identity = Some(identity);
        self.original_row = 0;
        Ok(())
    }
    fn cleanup_torn(&mut self, events: &PrivateDirectory, name: &str) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        inner.work.cleanup_file_attempts += 1;
        inner.work.inventory.open_attempts += 1;
        let mut file = events.open_file(name)?;
        if !self
            .original_witness
            .as_ref()
            .ok_or_else(invalid)?
            .cleanup_batch(
                usize::try_from(inner.state.cleanup.ordinal).map_err(|_| invalid())?,
                &mut file,
            )?
        {
            return Err(invalid());
        }
        inner.work.cleanup_removals += 1;
        events.remove_file(name)?;
        sync_directory(events, &mut inner.work)?;
        let mut state = inner.state.clone();
        state.cleanup.region = 1;
        state.cleanup.ordinal += 1;
        inner.commit_state(state)?;
        self.torn_pending = None;
        Ok(())
    }
    fn cleanup_absent(&mut self, events: &PrivateDirectory) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        let mut state = inner.state.clone();
        state.cleanup.region = 1;
        state.cleanup.ordinal += 1;
        for _ in 1..128 {
            if state.cleanup.ordinal == 4096 {
                break;
            }
            inner.work.cleanup_positions += 1;
            inner.work.cleanup_file_attempts += 1;
            inner.work.inventory.open_attempts += 1;
            let name = crate::event_recovery::batch_name(
                usize::try_from(state.cleanup.ordinal).map_err(|_| invalid())?,
            );
            if Self::optional_file(events, &name)?.is_some() {
                break;
            }
            if let Some(witness) = &self.original_witness {
                witness.cleanup_absent(
                    usize::try_from(state.cleanup.ordinal).map_err(|_| invalid())?,
                )?;
            }
            state.cleanup.ordinal += 1;
        }
        sync_directory(events, &mut inner.work)?;
        inner.commit_state(state)?;
        Ok(())
    }
    fn cleanup_compare(&mut self) -> io::Result<()> {
        self.cleanup_comparison_frame()?;
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        let original = self.original.as_ref().ok_or_else(invalid)?;
        let remaining = &original.rows()[self.original_row..];
        let Some(row) = remaining.first() else {
            return Ok(());
        };
        let frame = self.comparison.as_ref().ok_or_else(invalid)?;
        inner.work.proof_operations += 1;
        let position = Self::comparison_position(frame, row.event.number)?;
        let numbers = frame
            .column("event")
            .map_err(|error| io::Error::other(error.to_string()))?
            .u64()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let mut count = 0;
        for (offset, row) in remaining
            .iter()
            .enumerate()
            .take(128.min(frame.height() - position))
        {
            inner.work.proof_operations += 1;
            if numbers.get(position + offset) != Some(row.event.number) {
                break;
            }
            count += 1;
        }
        // The retained original reservation covers owned rows and this bounded Arrow scratch.
        let expected = batch::frame(&remaining[..count], inner.state.input.captured)
            .map_err(rows::source_error)?;
        if count == 0
            || !expected.equals_missing(
                &frame.slice(i64::try_from(position).map_err(|_| invalid())?, count),
            )
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unpreserved/conflicting original row",
            ));
        }
        self.original_row += count;
        Ok(())
    }
    fn cleanup_comparison_frame(&mut self) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
        let original = self.original.as_ref().ok_or_else(invalid)?;
        let Some(row) = original.rows().get(self.original_row) else {
            return Ok(());
        };
        let candidate = usize::from(
            inner.state.row_plan.ok_or_else(invalid)?.split != 0
                && row.event.number >= inner.state.row_plan.ok_or_else(invalid)?.split,
        );
        let Body::Events(manifest) = &self.metadata[candidate].body else {
            return Err(invalid());
        };
        let ordinal = manifest
            .batches
            .partition_point(|fact| fact.end <= row.event.number);
        let fact = manifest.batches.get(ordinal).ok_or_else(invalid)?;
        let key = candidate * 4096 + ordinal;
        if self.comparison_index != key || self.comparison.is_none() {
            self.comparison = None;
            self.comparison_reservation = None;
            self.comparison_reservation = Some(
                self.budget
                    .reserve(batch::WORKING_BYTES)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?,
            );
            inner.work.inventory.open_attempts += 1;
            let directory = Self::object(attempt, &mut inner.work)?
                .open_directory(&format!("fragment-{candidate}"))?;
            inner.work.inventory.open_attempts += 1;
            inner.work.inventory.raw_admissions += 1;
            self.comparison = Some(
                read::read_batch_file_counted(
                    directory.open_file(&crate::event_recovery::batch_name(ordinal))?,
                    fact,
                    attempt.input.captured,
                    true,
                    &self.budget,
                    &mut inner.work.inventory.decoded_bytes,
                )
                .map_err(rows::source_error)?,
            );
            self.comparison_index = key;
        }
        Ok(())
    }
    fn comparison_position(frame: &polars::prelude::DataFrame, event: u64) -> io::Result<usize> {
        let numbers = frame
            .column("event")
            .map_err(|error| io::Error::other(error.to_string()))?
            .u64()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let mut low = 0;
        let mut high = frame.height();
        for _ in 0..16 {
            if low == high {
                break;
            }
            let middle = low + (high - low) / 2;
            if numbers.get(middle).ok_or_else(invalid)? < event {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let position = low;
        if numbers.get(position) != Some(event) {
            return Err(invalid());
        }
        Ok(position)
    }
    fn clear(&mut self) -> io::Result<()> {
        if self.proved != Some(true) {
            return Err(invalid());
        }
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
        let object = Self::object(attempt, &mut inner.work)?;
        sync_directory(&object, &mut inner.work)?;
        inner.work.inventory.open_attempts += 1;
        let base = attempt.root.open_directory("data")?;
        sync_directory(&base, &mut inner.work)?;
        sync_directory(attempt.root, &mut inner.work)?;
        if inner.state.cleanup.region != 8 {
            let mut state = inner.state.clone();
            state.cleanup = crate::transaction::salvage::Cleanup {
                region: 8,
                ..Default::default()
            };
            inner.commit_state(state)?;
            return Ok(());
        }
        super::super::inventory::census::io_owner::retire(attempt, &inner.state, &self.ledger)?;
        let mut old = [0; 4096];
        let mut idle = [0; 4096];
        inner.state.encode(&mut old)?;
        codec::encode_idle(attempt.input.run.recording, SlotKind::Foreground, &mut idle)?;
        inner.work.controls += 1;
        super::super::publication_control::replace(
            &super::super::publication_control::CountedControl {
                root: attempt.root,
                ledger: Some(&self.ledger),
            },
            "foreground",
            &old,
            &idle,
            &mut inner.work.full_syncs,
        )?;
        inner.done = true;
        Ok(())
    }
}
impl Store {
    pub fn resume_row_publication(
        &self,
        reader: Option<RingReader>,
        budget: &Arc<ByteBudget>,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<Option<RowPublication<'_>>> {
        ledger.require_budget(budget)?;
        if !self.protocol_two {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        if self
            .staging
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let permit = StagingPermit(Arc::clone(&self.staging));
        let mut work = PublicationWork::default();
        let result = super::super::publication_control::read_optional_normal(
            &super::super::publication_control::CountedControl {
                root: &self.directory,
                ledger: Some(ledger),
            },
            self.recording,
            &mut work,
        );
        ledger.record(&work, &mut PublicationWork::default());
        let Some(state) = result? else {
            return Ok(None);
        };
        if state.row_plan.is_none() && state.phase != Phase::Registered {
            return Err(invalid());
        }
        let plan = state.row_plan;
        ledger.update(|work| work.inventory.open_attempts += 1);
        let base = self.directory.open_directory("data")?;
        ledger.update(|work| work.inventory.open_attempts += 1);
        let object = base
            .open_directory(&collector::recording::identity_text(state.transaction).to_string())?;
        let present = if let Some(plan) = plan {
            Self::row_metadata_present(&object, &plan, state.phase, ledger)?
        } else {
            false
        };
        if present {
            self.row_publication_reopen(state, permit, &object, reader, budget, ledger)
                .map(Some)
        } else {
            self.row_publication_candidates(state, permit, &object, reader, budget, ledger)
                .map(Some)
        }
    }
    fn row_publication_candidates<'a>(
        &'a self,
        state: crate::transaction::normal::NormalState,
        permit: StagingPermit,
        object: &PrivateDirectory,
        reader: Option<RingReader>,
        budget: &Arc<ByteBudget>,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<RowPublication<'a>> {
        let reader = reader.ok_or_else(|| io::Error::from(io::ErrorKind::WouldBlock))?;
        let mut opens = 0;
        let result = super::super::inventory::select_original_mode(object, &mut opens, true);
        ledger.update(|work| {
            work.inventory.open_attempts += opens;
            work.inventory.reopens += opens;
        });
        let (directory, placement) = result?.ok_or_else(invalid)?;
        let attempt = RegisteredAttempt {
            transaction: state.transaction,
            input: state.input,
            directory,
            resumed: true,
            candidates: true,
            _permit: permit,
            owner: &self.owner,
            root: &self.directory,
            placement,
        };
        let candidates = attempt
            .row_candidates(reader, budget, ledger)?
            .publication_metadata();
        RowPublication::blank(Some(candidates), None, budget, ledger)
    }

    fn row_publication_reopen<'a>(
        &'a self,
        state: crate::transaction::normal::NormalState,
        permit: StagingPermit,
        object: &PrivateDirectory,
        reader: Option<RingReader>,
        budget: &Arc<ByteBudget>,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<RowPublication<'a>> {
        let mut owner = self.row_publication_owner(state, permit, object, budget, ledger)?;
        owner.transport = reader.is_some();
        owner.load_metadata(object)?;
        if let Some(reader) = reader {
            owner.admit_source(reader)?;
        }
        Ok(owner)
    }
    fn row_publication_owner<'a>(
        &'a self,
        state: crate::transaction::normal::NormalState,
        permit: StagingPermit,
        object: &PrivateDirectory,
        budget: &Arc<ByteBudget>,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<RowPublication<'a>> {
        ledger.update(|work| work.inventory.open_attempts += 1);
        let directory = object.open_directory("fragment-0")?;
        let attempt = RegisteredAttempt {
            transaction: state.transaction,
            input: state.input.clone(),
            directory,
            resumed: true,
            candidates: true,
            _permit: permit,
            owner: &self.owner,
            root: &self.directory,
            placement: OriginalPlacement::Pending,
        };
        let reservation = budget
            .reserve(256 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let mut inner = NormalPublication::blank(attempt, state, budget, reservation);
        inner.ledger = Some(ledger.clone());
        inner.ready = true;
        inner.validation = Validation::Complete;
        RowPublication::blank(None, Some(inner), budget, ledger)
    }
    fn row_metadata_present(
        object: &PrivateDirectory,
        plan: &crate::transaction::salvage::RowPlan,
        phase: Phase,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<bool> {
        for candidate in 0..=usize::from(plan.split != 0) {
            ledger.update(|work| work.inventory.open_attempts += 1);
            let directory = match object.open_directory(&format!("fragment-{candidate}")) {
                Ok(directory) => directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            };
            ledger.update(|work| work.inventory.open_attempts += 1);
            let Some(mut file) = RowPublication::optional_file(&directory, "metadata")? else {
                return Ok(false);
            };
            ledger.update(|work| work.canonical.metadata_admissions += 1);
            match CanonicalMetadata::read(&mut file) {
                Ok(_) => {}
                Err(error)
                    if phase == Phase::SalvagePlanned
                        && matches!(
                            error.kind(),
                            io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData
                        ) =>
                {
                    return Ok(false);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }
}
impl RowPublication<'_> {
    fn admit_source(&mut self, reader: RingReader) -> io::Result<()> {
        let reservation = self
            .budget
            .reserve(64 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let input = &self.inner.as_ref().ok_or_else(invalid)?.state.input;
        self.ledger.update(|work| work.source_observations += 1);
        let source = super::super::normal_proof::SourceProof::new(reader, input, reservation)?;
        source.observations.set(0);
        self.source = Some(source);
        Ok(())
    }
    fn validate_metadata(
        state: &crate::transaction::normal::NormalState,
        metadata: &CanonicalMetadata,
        candidate: usize,
        first: u64,
        end: u64,
    ) -> io::Result<()> {
        let input = &state.input;
        let AttemptOperation::Ordinary {
            window,
            attempted_origin,
        } = input.operation
        else {
            return Err(invalid());
        };
        if metadata.window != window
            || metadata.attempted_origin != attempted_origin
            || metadata.canonical_origin != 0
            || metadata.evidence != EndpointEvidence::SurvivingRow
            || metadata.loss.unknown_flags != u32::from(input.losses.unknown.is_some())
            || metadata.loss.inherited.unknown != input.losses.unknown
            || metadata.loss.inherited.event_inherited_end
                != input.losses.event_inherited_end.clamp(first, end)
            || metadata.transaction != state.transaction
            || metadata.run != input.run
            || metadata.binding != input.binding
            || metadata.source_layout != input.source.layout
            || metadata.source_origin != input.source.origin_continuous_ns
            || metadata.kind != CanonicalKind::Fragment
            || metadata.base != RawBase::Data
            || metadata.child
                != if candidate == 0 {
                    RawChild::Fragment0
                } else {
                    RawChild::Fragment1
                }
            || metadata.loss.inherited.event_start != first
            || metadata.loss.event_end != end
            || metadata.loss.inherited.prior_window != input.losses.prior_window
            || metadata.captured != input.captured
            || metadata.lifecycle != Lifecycle::Active
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn validate_transfers(&self) -> io::Result<()> {
        let inner = self.inner.as_ref().ok_or_else(invalid)?;
        let input = &inner.state.input;
        let first = self.metadata.first().ok_or_else(invalid)?;
        let old = &first.loss.inherited;
        if old.publication_start != input.losses.publication_start
            || old.publication_inherited_end != input.losses.publication_inherited_end
            || old.publications != input.losses.publications
        {
            return Err(invalid());
        }
        if let Some(second) = self.metadata.get(1) {
            let old = &second.loss.inherited;
            if old.publication_start != first.loss.publication_end
                || old.publication_inherited_end != first.loss.publication_end
                || old.publications != 0
                || second.loss.publications_new != 0
                || second.loss.publication_end != first.loss.publication_end
                || second.original_end != first.original_end
                || second.trailing_unknown != first.trailing_unknown
            {
                return Err(invalid());
            }
        }
        if inner.state.row_plan.ok_or_else(invalid)?.rows != [0, 0] {
            let mut loss = first.loss.clone();
            loss.inherited = input.losses.clone();
            loss.event_end = inner.state.row_plan.ok_or_else(invalid)?.end;
            loss.events_new = self
                .metadata
                .iter()
                .map(|metadata| metadata.loss.events_new)
                .sum();
            if loss != inner.state.loss || inner.state.original_end != first.original_end {
                return Err(invalid());
            }
        }
        Ok(())
    }
    fn census_metadata_identities(&mut self) -> io::Result<()> {
        for _ in 0..128 {
            if self.identity_position == self.facts.len() {
                break;
            }
            let candidate = self.identity_position / 4096;
            let ordinal = self.identity_position % 4096;
            let fact = if let Some(metadata) = self.metadata.get(candidate) {
                let Body::Events(manifest) = &metadata.body else {
                    return Err(invalid());
                };
                if let Some(batch) = manifest.batches.get(ordinal) {
                    let inner = self.inner.as_mut().ok_or_else(invalid)?;
                    let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
                    inner.work.inventory.open_attempts += 1;
                    let directory = Self::object(attempt, &mut inner.work)?
                        .open_directory(&format!("fragment-{candidate}"))?;
                    inner.work.inventory.open_attempts += 1;
                    let file = directory.open_file(&crate::event_recovery::batch_name(ordinal))?;
                    crate::store::BatchFact::Complete {
                        file: crate::store::FileIdentity::read(&file)?,
                        first: batch.first,
                        end: batch.end,
                        rows: batch.rows,
                    }
                } else {
                    crate::store::BatchFact::Missing
                }
            } else {
                crate::store::BatchFact::Missing
            };
            self.facts[self.identity_position] = fact;
            self.identity_position += 1;
        }
        Ok(())
    }
    fn load_metadata(&mut self, object: &PrivateDirectory) -> io::Result<()> {
        let inner = self.inner.as_mut().ok_or_else(invalid)?;
        let plan = inner.state.row_plan.ok_or_else(invalid)?;
        let input = &inner.state.input;
        self.facts_reservation = Some(
            self.budget
                .reserve(8192 * size_of::<crate::store::BatchFact>())
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?,
        );
        self.facts = vec![crate::store::BatchFact::Missing; 8192];
        for candidate in 0..=usize::from(plan.split != 0) {
            inner.work.inventory.open_attempts += 1;
            let directory = object.open_directory(&format!("fragment-{candidate}"))?;
            inner.work.inventory.open_attempts += 1;
            let mut file = directory.open_file("metadata")?;
            inner.work.canonical.metadata_admissions += 1;
            let metadata = CanonicalMetadata::read(&mut file)?;
            let std::ops::Range { start: first, end } = plan.interval(candidate);
            Self::validate_metadata(&inner.state, &metadata, candidate, first, end)?;
            let expected = if candidate == 0 {
                input.losses.predecessor.clone()
            } else {
                self.metadata[0].reference()
            };
            if metadata.loss.inherited.predecessor != expected {
                return Err(invalid());
            }
            let Body::Events(manifest) = &metadata.body else {
                return Err(invalid());
            };
            if manifest.batches.len() > 4096 {
                return Err(invalid());
            }
            inner.links.push((
                Namespace::Fragments,
                CanonicalKey::source(Namespace::Fragments, &input.run, first)?,
            ));
            self.metadata.push(metadata);
        }
        self.validate_transfers()?;
        self.proved = Some(self.metadata[0].original_end > 0 && !self.metadata[0].trailing_unknown);
        Ok(())
    }
}

impl Drop for RowPublication<'_> {
    fn drop(&mut self) {
        if let Some(inner) = &self.inner {
            self.ledger.record(&inner.work, &mut self.recorded);
        }
    }
}

fn cleanup_original_name(name: &str) -> io::Result<()> {
    if name == "events" {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "original metric/metadata material remains",
        ))
    }
}
