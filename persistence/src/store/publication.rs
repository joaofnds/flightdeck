//! Consuming, bounded normal publication of a retained registered original.
use super::inventory::{InventoryStep, OriginalInventory, OriginalPlacement};
use super::{RegisteredAttempt, StagingPermit, Store};
use crate::canonical::metadata::Body;
use crate::canonical::{
    CanonicalKey, CanonicalLoss, CanonicalMetadata, EndpointEvidence, Lifecycle, Namespace,
    RawBase, RawChild, ReadWork,
};
use crate::canonical::{insert, rows};
use crate::resources::{ByteBudget, ByteReservation};
use crate::transaction::normal::NormalState;
use crate::transaction::{AttemptInput, AttemptOperation, CanonicalKind};
use collector::recording::bootstrap::codec::{self, NamespaceState, Phase, SlotKind};
use collector::recording::private::{self, PrivateDirectory};
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[path = "loss_only.rs"]
mod loss_only;
pub use loss_only::LossPublication;
#[path = "row_publication.rs"]
mod row_publication;
pub use row_publication::{RowPublication, RowPublicationStep};

pub struct NormalPublication<'store> {
    inventory: Option<OriginalInventory<'store, 'store>>,
    attempt: Option<RegisteredAttempt<'store>>,
    state: NormalState,
    metadata: Option<CanonicalMetadata>,
    frontier: Option<crate::canonical::SourceFrontierRead<'store, 'static>>,
    witness: Option<super::inventory::NormalInventory>,
    budget: Arc<ByteBudget>,
    _reservation: ByteReservation,
    tables_next: usize,
    batches_next: usize,
    batches: Vec<crate::event_recovery::Batch>,
    ticks: Option<Vec<rows::TickSpan>>,
    inherited_rows: u64,
    rows: u64,
    validation: Validation,
    ready: bool,
    links: Vec<(Namespace, CanonicalKey)>,
    link_next: usize,
    done: bool,
    row_unresolved: bool,
    draft: Option<CanonicalMetadata>,
    proof: Option<super::normal_proof::SourceProof>,
    work: PublicationWork,
    ledger: Option<super::PublicationLedger>,
    accounted: Option<PublicationWork>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Validation {
    Raw,
    Metadata,
    Complete,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct PublicationWork {
    pub raw_read_operations: u64,
    pub raw_read_requested_bytes: u64,
    pub raw_write_operations: u64,
    pub raw_write_requested_bytes: u64,
    pub census_write_operations: u64,
    pub census_write_requested_bytes: u64,
    pub decode_admission_bytes: u64,
    pub canonical: ReadWork,
    pub inventory: super::InventoryWork,
    pub links: u64,
    pub controls: u64,
    pub control_reads: u64,
    pub full_syncs: u64,
    pub source_reads: u64,
    pub source_observations: u64,
    pub proof_operations: u64,
    pub proof_publication_next: u64,
    pub proof_event_next: u64,
    pub cleanup_positions: u64,
    pub cleanup_file_attempts: u64,
    pub cleanup_directory_attempts: u64,
    pub cleanup_removals: u64,
    pub control_file_opens: u64,
    pub control_file_creates: u64,
    pub control_read_operations: u64,
    pub control_read_bytes: u64,
    pub control_write_operations: u64,
    pub control_write_requested_bytes: u64,
    pub control_rename_attempts: u64,
    pub control_remove_attempts: u64,
    pub control_file_sync_attempts: u64,
    pub control_directory_sync_attempts: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationStep {
    Continue,
    Idle,
}

impl RegisteredAttempt<'_> {
    fn ordinary_draft(&self, manifest: &crate::Manifest) -> io::Result<CanonicalMetadata> {
        let AttemptOperation::Ordinary {
            window,
            attempted_origin,
        } = self.input.operation
        else {
            return Err(invalid());
        };
        let loss = final_loss(
            &self.input,
            manifest.publication_end,
            manifest.events_end,
            &manifest.losses,
        )?;
        let metadata = CanonicalMetadata {
            run: self.input.run,
            transaction: self.transaction,
            kind: CanonicalKind::Ordinary,
            base: RawBase::Data,
            child: RawChild::Ready,
            window,
            attempted_origin,
            canonical_origin: manifest.publication_first,
            original_end: manifest.publication_end,
            binding: self.input.binding,
            source_layout: self.input.source.layout,
            source_origin: self.input.source.origin_continuous_ns,
            evidence: EndpointEvidence::ReadyManifest,
            lifecycle: Lifecycle::Active,
            captured: self.input.captured,
            trailing_unknown: false,
            loss,
            body: Body::Ordinary(manifest.clone()),
        };
        metadata.validate()?;
        Ok(metadata)
    }
    fn terminal_draft(
        &self,
        recovery: &crate::event_recovery::Recovery,
        batches: &[crate::event_recovery::Batch],
    ) -> io::Result<CanonicalMetadata> {
        let AttemptOperation::Terminal { event_end } = self.input.operation else {
            return Err(invalid());
        };
        if event_end != recovery.events_end || batches.len() > 4096 {
            return Err(invalid());
        }

        let publication_end = self.input.losses.publication_inherited_end;
        let loss = final_loss(&self.input, publication_end, event_end, &recovery.losses)?;
        let metadata = CanonicalMetadata {
            run: self.input.run,
            transaction: self.transaction,
            kind: CanonicalKind::Terminal,
            base: RawBase::Recovery,
            child: RawChild::Ready,
            window: 0,
            attempted_origin: 0,
            canonical_origin: 0,
            original_end: 0,
            binding: self.input.binding,
            source_layout: self.input.source.layout,
            source_origin: self.input.source.origin_continuous_ns,
            evidence: EndpointEvidence::FixedSource,
            lifecycle: Lifecycle::AcquiredCompletion,
            captured: self.input.captured,
            trailing_unknown: false,
            loss,
            body: Body::Events(crate::event_recovery::Manifest {
                recovery: *recovery,
                identities: self.input.captured,
                batches: batches.to_vec(),
            }),
        };
        metadata.validate()?;
        Ok(metadata)
    }
}
impl<'store> RegisteredAttempt<'store> {
    /// Proves the source complement before issuing complete immutable metadata.
    /// Writable raw leases must end before this consuming handoff.
    /// ```compile_fail,E0505
    /// use collector::ring::RingReader;
    /// use persistence::{Store, Manifest, resources::ByteBudget, transaction::AttemptInput};
    /// use std::{io::Write, sync::Arc};
    /// fn prove(store: &Store, input: &AttemptInput, mut reader: RingReader,
    ///          manifest: &Manifest, budget: &Arc<ByteBudget>) {
    ///     let attempt = store.register_attempt(input, &mut reader, budget).unwrap();
    ///     let mut file = attempt.create_file("ticks.parquet").unwrap();
    ///     let proof = attempt.prove_ordinary(manifest, reader, budget).unwrap();
    ///     file.write_all(b"late write").unwrap();
    /// }
    /// ```
    pub fn prove_ordinary(
        self,
        manifest: &crate::Manifest,
        reader: collector::ring::RingReader,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<NormalProof<'store>> {
        let reservation = budget
            .reserve(256 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let draft = self.ordinary_draft(manifest)?;
        self.begin_proof(draft, reader, budget, reservation)
    }
    pub fn prove_terminal(
        self,
        recovery: &crate::event_recovery::Recovery,
        batches: &[crate::event_recovery::Batch],
        reader: collector::ring::RingReader,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<NormalProof<'store>> {
        let reservation = budget
            .reserve(256 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let draft = self.terminal_draft(recovery, batches)?;
        self.begin_proof(draft, reader, budget, reservation)
    }
    fn begin_proof(
        self,
        draft: CanonicalMetadata,
        reader: collector::ring::RingReader,
        budget: &Arc<ByteBudget>,
        reservation: ByteReservation,
    ) -> io::Result<NormalProof<'store>> {
        let scratch = budget
            .reserve(64 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let proof = super::normal_proof::SourceProof::new(reader, &self.input, scratch)?;
        let state = NormalState::registered(self.transaction, self.input.clone());
        let mut inner = NormalPublication::new(self, state, budget, Some(reservation), None)?;
        inner.draft = Some(draft);
        inner.proof = Some(proof);
        Ok(NormalProof { inner })
    }
    /// Consumes append authority, including the lifetime of every writable raw handle.
    /// ```compile_fail
    /// use collector::ring::RingReader;
    /// use persistence::{Store, resources::ByteBudget, transaction::AttemptInput};
    /// use std::{io::Write, sync::Arc};
    /// fn ready(store: &Store, input: &AttemptInput, reader: &mut RingReader,
    ///          budget: &Arc<ByteBudget>) {
    ///     let attempt = store.register_attempt(input, reader, budget).unwrap();
    ///     let mut file = attempt.create_file("ticks.parquet").unwrap();
    ///     let publisher = attempt.normal_publication(budget).unwrap();
    ///     file.write_all(b"late write").unwrap();
    /// }
    /// ```
    pub fn normal_publication(
        self,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<NormalPublication<'store>> {
        let state = NormalState::registered(self.transaction, self.input.clone());
        NormalPublication::new(self, state, budget, None, None)
    }
}
pub struct NormalProof<'store> {
    inner: NormalPublication<'store>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofStep {
    Continue,
    Complete,
}
impl<'store> NormalProof<'store> {
    pub fn step(&mut self) -> io::Result<ProofStep> {
        if self.inner.validation != Validation::Complete {
            self.inner.step()?;
        }
        Ok(if self.inner.validation == Validation::Complete {
            ProofStep::Complete
        } else {
            ProofStep::Continue
        })
    }
    pub fn work(&self) -> PublicationWork {
        self.inner.work()
    }
    pub fn into_publication(self) -> io::Result<NormalPublication<'store>> {
        if self.inner.validation != Validation::Complete || self.inner.proof.is_some() {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }

        Ok(self.inner)
    }
}
impl Store {
    /// Reopens exact retained authority without needing transport or creating another attempt.
    pub fn resume_publication(
        &self,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<NormalPublication<'_>> {
        self.resume_publication_inner(budget, None)
    }
    /// Source-free normal resume with caller-retained accounting, including construction errors.
    pub fn resume_publication_counted(
        &self,
        budget: &Arc<ByteBudget>,
        ledger: &super::PublicationLedger,
    ) -> io::Result<NormalPublication<'_>> {
        ledger.require_budget(budget)?;
        self.resume_publication_inner(budget, Some(ledger))
    }
    fn resume_publication_inner(
        &self,
        budget: &Arc<ByteBudget>,
        ledger: Option<&super::PublicationLedger>,
    ) -> io::Result<NormalPublication<'_>> {
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
        let reservation = budget
            .reserve(256 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let state = super::publication_control::read_normal(
            &super::publication_control::CountedControl {
                root: &self.directory,
                ledger,
            },
            self.recording,
        )?;
        if state.census.kind != 0
            || state.row_plan.is_some()
            || state.salvage.is_some()
            || state.input.is_finite()
        {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }

        let (directory, placement) = if let Some(ledger) = ledger {
            let mut work = PublicationWork::default();
            let result = resume_publication_directory(self, &state, &mut work);
            ledger.record(&work, &mut PublicationWork::default());
            result?
        } else if state.phase == Phase::Registered {
            self.register_attempt_directory(&state.input, state.transaction, true)?
        } else {
            let base = self.directory.open_directory(base_name(&state.input))?;
            let object = base.open_directory(
                &collector::recording::identity_text(state.transaction).to_string(),
            )?;
            super::inventory::select_original(&object)?.ok_or_else(invalid)?
        };
        let attempt = RegisteredAttempt {
            transaction: state.transaction,
            input: state.input.clone(),
            directory,
            resumed: true,
            candidates: false,
            _permit: permit,
            owner: &self.owner,
            root: &self.directory,
            placement,
        };
        let mut owner =
            NormalPublication::new(attempt, state, budget, Some(reservation), ledger.cloned())?;
        if ledger.is_some() {
            owner.accounted = Some(PublicationWork::default());
        }
        Ok(owner)
    }
}
fn resume_publication_directory(
    store: &Store,
    state: &NormalState,
    work: &mut PublicationWork,
) -> io::Result<(PrivateDirectory, OriginalPlacement)> {
    work.inventory.open_attempts += 1;
    let base = store.directory.open_directory(base_name(&state.input))?;
    work.inventory.open_attempts += 1;
    let object =
        base.open_directory(&collector::recording::identity_text(state.transaction).to_string())?;
    drop(base);
    super::inventory::select_original_counted(&object, &mut work.inventory.open_attempts)?
        .ok_or_else(invalid)
}
impl<'store> NormalPublication<'store> {
    pub(super) fn recovered(
        retained: &mut Option<OriginalInventory<'store, 'store>>,
        manifest: &crate::Manifest,
        source: &mut Option<super::normal_proof::SourceProof>,
        budget: &Arc<ByteBudget>,
        ledger: &super::PublicationLedger,
    ) -> io::Result<Self> {
        let inventory = retained.as_ref().ok_or_else(invalid)?;
        let metadata = inventory.owner().ordinary_draft(manifest)?;
        inventory.validate_normal(&metadata)?;
        let batches = inventory.normal_batches(&metadata)?;
        let attempt = inventory.owner();
        let state = NormalState::registered(attempt.transaction, attempt.input.clone());
        normal_namespace(attempt, &state, Some(ledger))?;
        normal_original_only(attempt, &state, Some(ledger))?;
        let reservation = budget
            .reserve(256 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let directories = inventory.normal_directory_inodes()?;
        let (attempt, witness) = retained
            .take()
            .ok_or_else(invalid)?
            .into_normal_prepared(directories.original, directories.events)?;
        let proof = source.take().ok_or_else(invalid)?;
        let mut owner = Self::blank(attempt, state, budget, reservation);
        owner.witness = Some(witness);
        owner.batches = batches;
        owner.metadata = Some(metadata);
        owner.proof = Some(proof);
        owner.ledger = Some(ledger.clone());
        Ok(owner)
    }
    fn new(
        attempt: RegisteredAttempt<'store>,
        state: NormalState,
        budget: &Arc<ByteBudget>,
        reservation: Option<ByteReservation>,
        ledger: Option<super::PublicationLedger>,
    ) -> io::Result<Self> {
        let reservation = match reservation {
            Some(reservation) => reservation,
            None => budget
                .reserve(256 * 1024)
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?,
        };
        normal_namespace(&attempt, &state, ledger.as_ref())?;
        normal_original_only(&attempt, &state, ledger.as_ref())?;
        let mut owner = Self::blank(attempt, state, budget, reservation);
        owner.ledger = ledger;
        let inventory = OriginalInventory::owned_counted(
            owner.attempt.take().ok_or_else(invalid)?,
            budget,
            &mut owner.work.inventory,
        );
        match inventory {
            Ok(inventory) => owner.inventory = Some(inventory),
            Err(error) => {
                if let Some(ledger) = &owner.ledger {
                    ledger.record(&owner.work, &mut PublicationWork::default());
                }
                return Err(error);
            }
        }
        Ok(owner)
    }
    fn blank(
        attempt: RegisteredAttempt<'store>,
        state: NormalState,
        budget: &Arc<ByteBudget>,
        reservation: ByteReservation,
    ) -> Self {
        Self {
            inventory: None,
            attempt: Some(attempt),
            state,
            metadata: None,
            frontier: None,
            witness: None,
            budget: Arc::clone(budget),
            _reservation: reservation,
            tables_next: 0,
            batches_next: 0,
            batches: Vec::new(),
            ticks: None,
            inherited_rows: 0,
            rows: 0,
            validation: Validation::Raw,
            ready: false,
            links: Vec::with_capacity(62),
            link_next: 0,
            done: false,
            row_unresolved: false,
            draft: None,
            proof: None,
            work: PublicationWork::default(),
            ledger: None,
            accounted: None,
        }
    }
    #[must_use]
    pub fn work(&self) -> PublicationWork {
        let mut work = self.work;
        if let Some(proof) = &self.proof {
            work.source_reads = proof.reads;
            work.source_observations = proof.observations.get();
            work.proof_operations = proof.operations;
            (work.proof_publication_next, work.proof_event_next) = proof.positions();
        }
        work
    }
    pub fn step(&mut self) -> io::Result<PublicationStep> {
        let result = self.step_inner();
        let work = self.work();
        if let (Some(ledger), Some(accounted)) = (&self.ledger, &mut self.accounted) {
            ledger.record(&work, accounted);
        }
        result
    }
    fn step_inner(&mut self) -> io::Result<PublicationStep> {
        if let Some(proof) = &mut self.proof {
            proof.begin_pass();
        }
        if self.done {
            return Ok(PublicationStep::Idle);
        }

        let owner = if let Some(inventory) = self.inventory.as_ref() {
            inventory.owner()
        } else {
            self.attempt.as_ref().ok_or_else(invalid)?
        };
        self.work.control_reads += 1;
        super::publication_control::visible(
            &super::publication_control::CountedControl {
                root: owner.root,
                ledger: self.ledger.as_ref(),
            },
            &self.state,
        )?;
        if self.inventory.is_some() {
            self.step_inventory()?;
        } else if self.validation != Validation::Complete {
            self.step_validation()?;
        } else if !self.ready {
            self.finish_ready()?;
            self.ready = true;
        } else {
            self.step_publication()?;
        }
        Ok(if self.done {
            PublicationStep::Idle
        } else {
            PublicationStep::Continue
        })
    }
    fn step_inventory(&mut self) -> io::Result<()> {
        let inventory = self.inventory.as_mut().ok_or_else(invalid)?;
        let result = inventory.step().map_err(rows::source_error);
        self.work.inventory = inventory.work();
        if result? != InventoryStep::Enumerated {
            return Ok(());
        }

        let inventory = self.inventory.as_ref().ok_or_else(invalid)?;
        let attempt = inventory.owner();
        let metadata = if let Some(draft) = self.draft.take() {
            draft
        } else {
            self.work.canonical.namespace_attempts += 1;
            let mut file = attempt.original_directory().open_file("metadata")?;
            inventory.metadata_file(&file)?;
            let length = file.metadata()?.len();
            let metadata = CanonicalMetadata::read_from(
                &mut WorkFile {
                    file: &mut file,
                    work: &mut self.work,
                },
                length,
            )?;
            inventory.metadata_file(&file)?;
            metadata
        };
        validate_metadata(&metadata, &self.state)?;
        inventory.validate_normal(&metadata)?;
        self.batches = inventory.normal_batches(&metadata)?;
        let (owner, witness) = self.inventory.take().ok_or_else(invalid)?.into_normal()?;
        self.attempt = Some(owner);
        self.witness = Some(witness);
        self.metadata = Some(metadata);
        Ok(())
    }
    fn step_validation(&mut self) -> io::Result<()> {
        if self.validation == Validation::Metadata {
            return self.step_validation_metadata();
        }

        if self.step_validation_table()? {
            return Ok(());
        }

        if self.step_validation_batch()? {
            return Ok(());
        }

        if !self.step_validation_proof()? {
            return Ok(());
        }

        let attempt = self.attempt.as_ref().ok_or_else(invalid)?;
        let metadata = self.metadata.as_ref().ok_or_else(invalid)?;
        let witness = self.witness.as_ref().ok_or_else(invalid)?;
        let ordinary = matches!(metadata.body, Body::Ordinary(_));
        let events = if ordinary {
            self.work.canonical.namespace_attempts += 1;
            Some(attempt.directory.open_directory("events")?)
        } else {
            None
        };
        let directory = events.as_ref().unwrap_or(&attempt.directory);
        let domain = &metadata.loss.inherited;
        if self.inherited_rows.checked_add(domain.events)
            != Some(domain.event_inherited_end - domain.event_start)
            || (self.rows - self.inherited_rows).checked_add(metadata.loss.events_new)
                != Some(metadata.loss.event_end - domain.event_inherited_end)
        {
            return Err(invalid());
        }

        self.links.clear();
        if ordinary {
            self.links = rows::observation_keys(
                &attempt.directory,
                metadata,
                &self.budget,
                &mut self.work.canonical,
                self.ledger
                    .as_ref()
                    .map(|ledger| -> &dyn crate::engine::RawTableWork { ledger }),
            )?;
            self.work.canonical.namespace_attempts += 1;
            let index_file = directory.open_file("index")?;
            witness.index(&index_file)?;
            self.work.full_syncs += 1;
            private::full_sync(&index_file)?;
            sync_directory(directory, &mut self.work)?;
        }
        let namespace = if ordinary {
            Namespace::Ordinary
        } else {
            Namespace::Terminal
        };
        self.links.push((
            namespace,
            CanonicalKey::source(namespace, &metadata.run, metadata.canonical_origin)?,
        ));
        drop(events);
        self.validation = Validation::Metadata;
        self.step_validation_metadata()
    }
    fn step_validation_proof(&mut self) -> io::Result<bool> {
        let metadata = self.metadata.as_ref().ok_or_else(invalid)?;
        if let Some(proof) = self.proof.as_mut() {
            if !proof.publications(
                &self.state.input,
                self.ticks.as_deref().unwrap_or(&[]),
                metadata.loss.publication_end,
            )? || !proof.events(&self.state.input, metadata.loss.event_end)?
                || !proof.boundary(&self.state.input, metadata.original_end)?
            {
                return Ok(false);
            }

            if proof.losses
                != [
                    self.state.input.losses.publications,
                    metadata.loss.publications_new,
                    self.state.input.losses.events,
                    metadata.loss.events_new,
                ]
            {
                return Err(invalid());
            }

            proof.check(&self.state.input)?;
        }
        Ok(true)
    }
    fn step_validation_metadata(&mut self) -> io::Result<()> {
        let attempt = self.attempt.as_ref().ok_or_else(invalid)?;
        let metadata = self.metadata.as_ref().ok_or_else(invalid)?;
        let witness = self.witness.as_ref().ok_or_else(invalid)?;
        if self.proof.is_some() {
            write_metadata(
                attempt,
                metadata,
                &mut self.work,
                &self.state,
                self.ledger.as_ref(),
            )?;
        }
        self.work.canonical.namespace_attempts += 1;
        let metadata_file = attempt.directory.open_file("metadata")?;
        if self.proof.is_none() {
            witness.file("metadata", &metadata_file)?;
        }
        self.work.full_syncs += 1;
        private::full_sync(&metadata_file)?;
        drop(metadata_file);
        sync_directory(&attempt.directory, &mut self.work)?;
        validate_pending(
            attempt.root,
            &self.state,
            metadata,
            &self.links,
            self.ledger.as_ref(),
        )?;
        self.witness = None;
        if let Some(proof) = self.proof.take() {
            self.work.source_reads = proof.reads;
            self.work.source_observations = proof.observations.get();
            self.work.proof_operations = proof.operations;
            (self.work.proof_publication_next, self.work.proof_event_next) = proof.positions();
        }
        self.validation = Validation::Complete;
        Ok(())
    }
    fn step_validation_table(&mut self) -> io::Result<bool> {
        let attempt = self.attempt.as_ref().ok_or_else(invalid)?;
        let metadata = self.metadata.as_ref().ok_or_else(invalid)?;
        let witness = self.witness.as_ref().ok_or_else(invalid)?;
        let events_for_check = if matches!(metadata.body, Body::Ordinary(_)) {
            self.work.canonical.namespace_attempts += 1;
            Some(attempt.directory.open_directory("events")?)
        } else {
            None
        };
        witness.directories(&attempt.directory, events_for_check.as_ref())?;
        drop(events_for_check);
        if let Body::Ordinary(manifest) = &metadata.body {
            if self.tables_next < manifest.tables.len() {
                let table = &manifest.tables[self.tables_next];
                if table.name != "events" {
                    validate_table(
                        &attempt.directory,
                        table,
                        &self.budget,
                        witness,
                        &mut self.work,
                        self.ledger.as_ref(),
                    )?;
                }
                self.tables_next += 1;
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn step_validation_batch(&mut self) -> io::Result<bool> {
        let attempt = self.attempt.as_ref().ok_or_else(invalid)?;
        let metadata = self.metadata.as_ref().ok_or_else(invalid)?;
        let witness = self.witness.as_ref().ok_or_else(invalid)?;
        let ordinary = matches!(metadata.body, Body::Ordinary(_));
        if let Body::Ordinary(manifest) = &metadata.body {
            if self.ticks.is_none() {
                self.ticks = Some(rows::verify_ticks(
                    &attempt.directory,
                    metadata,
                    manifest,
                    &self.budget,
                    &mut self.work.canonical,
                    self.ledger
                        .as_ref()
                        .map(|ledger| -> &dyn crate::engine::RawTableWork { ledger }),
                )?);
            }
        }
        if let Some(proof) = self.proof.as_mut() {
            if proof.frame.is_some() {
                proof.events(&self.state.input, metadata.loss.event_end)?;
                return Ok(true);
            }
        }
        let events = if ordinary {
            self.work.canonical.namespace_attempts += 1;
            Some(attempt.directory.open_directory("events")?)
        } else {
            None
        };
        let directory = events.as_ref().unwrap_or(&attempt.directory);
        if let Some(batch) = self.batches.get(self.batches_next) {
            let workspace = self
                .budget
                .reserve(crate::event_recovery::batch::WORKING_BYTES)
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
            let name = crate::event_recovery::batch_name(self.batches_next);
            self.work.canonical.namespace_attempts += 1;
            let file = directory.open_file(&name)?;
            witness.batch(self.batches_next, &file)?;
            self.work.full_syncs += 1;
            private::full_sync(&file)?;
            let (frame, counts) = rows::admit_batch_file(
                file,
                metadata,
                batch,
                &rows::BatchMode {
                    ordinal: self.batches_next,
                    associated: ordinary,
                    ticks: self.ticks.as_deref(),
                },
                &self.budget,
                &mut self.work.canonical,
                self.ledger
                    .as_ref()
                    .map(|ledger| -> &dyn crate::engine::RawTableWork { ledger }),
            )?;
            if let Some(proof) = self.proof.as_mut() {
                proof.frame = Some(frame);
                proof.workspace = Some(workspace);
            }
            self.rows += counts.0;
            self.inherited_rows += counts.1;
            self.batches_next += 1;
            return Ok(true);
        }

        Ok(false)
    }
    fn finish_ready(&mut self) -> io::Result<()> {
        let attempt = self.attempt.as_mut().ok_or_else(invalid)?;
        let metadata = self.metadata.as_ref().ok_or_else(invalid)?;
        let mut next = self.state.clone();
        next.original_end = metadata.original_end;
        next.origin = metadata.canonical_origin;
        next.loss = metadata.loss.clone();
        if self.state.phase == Phase::Registered {
            next.phase = Phase::Ready;
        } else if next != self.state {
            return Err(invalid());
        }

        let mut before = [0; 4096];
        let mut after = [0; 4096];
        self.state.encode(&mut before)?;
        next.encode(&mut after)?;
        if self.state.phase == Phase::Registered {
            super::publication_control::check(
                &super::publication_control::CountedControl {
                    root: attempt.root,
                    ledger: self.ledger.as_ref(),
                },
                "foreground",
                &before,
                &after,
            )?;
        }
        self.work.inventory.open_attempts += 1;
        let base = attempt.root.open_directory(base_name(&attempt.input))?;
        self.work.inventory.open_attempts += 1;
        let object = base.open_directory(
            &collector::recording::identity_text(attempt.transaction).to_string(),
        )?;
        let (selected, placement) = super::inventory::select_original_counted(
            &object,
            &mut self.work.inventory.open_attempts,
        )?
        .ok_or_else(invalid)?;
        if !selected.same_inode(&attempt.directory)? {
            return Err(invalid());
        }

        drop(selected);
        if placement == OriginalPlacement::Pending {
            self.work.canonical.renames += 1;
            object.rename("pending", "ready")?;
        }
        sync_directory(&attempt.directory, &mut self.work)?;
        sync_directory(&object, &mut self.work)?;
        sync_directory(&base, &mut self.work)?;
        attempt.placement = OriginalPlacement::Ready;
        if self.state.phase == Phase::Registered {
            self.commit_state(next)?;
        }
        Ok(())
    }
    fn commit_state(&mut self, next: NormalState) -> io::Result<()> {
        if !super::publication_control::census_next(&self.state, &next) {
            return Err(invalid());
        }
        let attempt = self.attempt.as_ref().ok_or_else(invalid)?;
        let mut before = [0; 4096];
        let mut after = [0; 4096];
        self.state.encode(&mut before)?;
        next.encode(&mut after)?;
        self.work.controls += 1;
        super::publication_control::replace(
            &super::publication_control::CountedControl {
                root: attempt.root,
                ledger: self.ledger.as_ref(),
            },
            "foreground",
            &before,
            &after,
            &mut self.work.full_syncs,
        )?;
        self.state = next;
        Ok(())
    }
    fn step_publication(&mut self) -> io::Result<()> {
        match self.state.phase {
            Phase::Ready | Phase::SalvagePlanned => self.acquire_busy(),
            Phase::UnresolvedExtent if self.state.input.is_finite() && self.state.sequence == 0 => {
                self.acquire_busy()
            }
            Phase::Publishing => self.publish_link(),
            Phase::Published => self.finish_quiet(),
            Phase::Cleanup => self.finish_idle(),
            _ => Err(io::Error::from(io::ErrorKind::Unsupported)),
        }
    }
    fn acquire_busy(&mut self) -> io::Result<()> {
        let root = self.attempt.as_ref().ok_or_else(invalid)?.root;
        let current = super::publication_control::read_state(
            &super::publication_control::CountedControl {
                root,
                ledger: self.ledger.as_ref(),
            },
            self.state.input.run.recording,
        )?;
        let sequence = match current {
            NamespaceState::Quiet { sequence } => {
                if !self.acquire_busy_frontier()? {
                    return Ok(());
                }

                sequence.checked_add(1).ok_or_else(invalid)?
            }
            NamespaceState::Busy {
                sequence,
                transaction,
            } if transaction == self.state.transaction => {
                insert::validate_roots(root, &mut self.work.canonical)?;
                sequence
            }
            NamespaceState::Busy { .. } => return Err(io::Error::from(io::ErrorKind::WouldBlock)),
        };
        let mut next = self.state.clone();
        next.phase = Phase::Publishing;
        next.sequence = sequence;
        self.check_publishing_transition(&next)?;
        let mut old = [0; 256];
        let mut new = [0; 256];
        codec::encode_state(
            self.state.input.run.recording,
            &NamespaceState::Quiet {
                sequence: sequence.checked_sub(1).ok_or_else(invalid)?,
            },
            &mut old,
        )?;
        codec::encode_state(
            self.state.input.run.recording,
            &NamespaceState::Busy {
                sequence,
                transaction: self.state.transaction,
            },
            &mut new,
        )?;
        self.work.controls += 1;
        super::publication_control::replace(
            &super::publication_control::CountedControl {
                root,
                ledger: self.ledger.as_ref(),
            },
            "namespace-state",
            &old,
            &new,
            &mut self.work.full_syncs,
        )?;
        self.commit_state(next)
    }
    fn check_publishing_transition(&self, next: &NormalState) -> io::Result<()> {
        let mut slot_old = [0; 4096];
        let mut slot_new = [0; 4096];
        self.state.encode(&mut slot_old)?;
        next.encode(&mut slot_new)?;
        super::publication_control::check(
            &super::publication_control::CountedControl {
                root: self.attempt.as_ref().ok_or_else(invalid)?.root,
                ledger: self.ledger.as_ref(),
            },
            "foreground",
            &slot_old,
            &slot_new,
        )
    }
    fn acquire_busy_frontier(&mut self) -> io::Result<bool> {
        let attempt = self.attempt.as_ref().ok_or_else(invalid)?;
        if self.frontier.is_none() {
            insert::validate_roots(attempt.root, &mut self.work.canonical)?;
            let query = crate::canonical::SourceFrontierRead::begin_owned(
                attempt.root,
                self.state.input.run,
                &self.budget,
            )?;
            self.work.canonical.add(query.work());
            self.frontier = Some(query);
            return Ok(false);
        }

        let query = self.frontier.as_mut().ok_or_else(invalid)?;
        let before = query.work();
        let result: io::Result<_> = (|| {
            if query.step()? != crate::canonical::FrontierStep::Complete {
                return Ok(None);
            }

            query.validate_owner(attempt.root, &self.state.input.run)?;
            Ok(Some(query.result()?.cloned()))
        })();
        let after = query.work();
        self.work.canonical.namespace_attempts +=
            after.namespace_attempts - before.namespace_attempts;
        self.work.canonical.node_attempts += after.node_attempts - before.node_attempts;
        self.work.canonical.entries += after.entries - before.entries;
        self.work.canonical.native_entries += after.native_entries - before.native_entries;
        self.work.canonical.directory_reads += after.directory_reads - before.directory_reads;
        self.work.canonical.raw_admissions += after.raw_admissions - before.raw_admissions;
        self.work.canonical.metadata_admissions +=
            after.metadata_admissions - before.metadata_admissions;
        let Some(frontier) = result? else {
            return Ok(false);
        };
        self.frontier = None;
        if let Some(frontier) = frontier {
            frontier.matches_input(&self.state.input)?;
        } else if self.state.input.losses.predecessor != crate::transaction::Predecessor::Genesis {
            return Err(invalid());
        }

        Ok(true)
    }
    fn select_row_source(&mut self, index: usize) -> io::Result<()> {
        if self.state.row_plan.is_none() || index >= self.links.len() {
            return Ok(());
        }
        let attempt = self.attempt.as_mut().ok_or_else(invalid)?;
        self.work.inventory.open_attempts += 1;
        let base = attempt.root.open_directory(base_name(&attempt.input))?;
        self.work.inventory.open_attempts += 1;
        let object = base.open_directory(
            &collector::recording::identity_text(attempt.transaction).to_string(),
        )?;
        self.work.inventory.open_attempts += 1;
        // Rebind the consumed capability rather than retain another source descriptor
        // across insertion. Original cleanup reopens through the fixed object.
        attempt.directory = object.open_directory(&format!("fragment-{index}"))?;
        Ok(())
    }
    fn publish_link(&mut self) -> io::Result<()> {
        self.require_busy()?;
        self.select_row_source(if self.state.structure.is_some() {
            usize::try_from(self.state.next).map_err(|_| invalid())?
        } else {
            self.link_next
        })?;
        let attempt = self.attempt.as_ref().ok_or_else(invalid)?;
        if self.state.structure.is_some() {
            return self.publish_structure();
        }

        if let Some((namespace, key)) = self.links.get(self.link_next) {
            self.work.links += 1;
            let plan = insert::begin(
                attempt.root,
                &attempt.directory,
                *namespace,
                key,
                u64::try_from(self.link_next + 1).map_err(|_| invalid())?,
                &mut self.work.canonical,
            )?;
            if let Some(plan) = plan {
                let mut next = self.state.clone();
                next.structure = Some(plan);
                next.next = u64::try_from(self.link_next).map_err(|_| invalid())?;
                self.commit_state(next)?;
            } else {
                self.link_next += 1;
            }
        } else {
            let mut next = self.state.clone();
            next.phase = Phase::Published;
            if let Some(plan) = &mut next.row_plan {
                plan.committed = (1 << self.links.len()) - 1;
            }
            next.next = u64::try_from(self.links.len()).map_err(|_| invalid())?;
            self.commit_state(next)?;
        }
        Ok(())
    }
    fn publish_structure(&mut self) -> io::Result<()> {
        self.select_row_source(if self.state.structure.is_some() {
            usize::try_from(self.state.next).map_err(|_| invalid())?
        } else {
            self.link_next
        })?;
        let attempt = self.attempt.as_ref().ok_or_else(invalid)?;
        let mut structure = self.state.structure.clone().ok_or_else(invalid)?;
        let index = usize::try_from(self.state.next).map_err(|_| invalid())?;
        let (namespace, key) = self.links.get(index).ok_or_else(invalid)?;
        let expected = crate::canonical::NodeLocator::from_key(
            *namespace,
            *key,
            u8::try_from(namespace.width() * 2).map_err(|_| invalid())?,
        )?;
        if structure.insertion != expected || structure.serial != self.state.next + 1 {
            return Err(invalid());
        }

        if *namespace == Namespace::Ordinary && self.link_next < index {
            let (previous_namespace, previous_key) =
                self.links.get(self.link_next).ok_or_else(invalid)?;
            insert::verify(
                attempt.root,
                &attempt.directory,
                *previous_namespace,
                previous_key,
                &mut self.work.canonical,
            )?;
            self.link_next += 1;
            return Ok(());
        }

        let done = insert::step(
            attempt.root,
            &attempt.directory,
            self.state.transaction,
            &mut structure,
            &mut self.work.canonical,
        )?;
        let serial = structure.serial;
        let mut next = self.state.clone();
        next.structure = if done { None } else { Some(structure) };
        self.commit_state(next)?;
        if done {
            self.link_next =
                if serial == u64::try_from(self.link_next + 1).map_err(|_| invalid())? {
                    self.link_next + 1
                } else {
                    0
                };
        }
        Ok(())
    }
    fn require_busy(&self) -> io::Result<()> {
        let attempt = self.attempt.as_ref().ok_or_else(invalid)?;
        let expected = NamespaceState::Busy {
            sequence: self.state.sequence,
            transaction: self.state.transaction,
        };
        if super::publication_control::read_state(
            &super::publication_control::CountedControl {
                root: attempt.root,
                ledger: self.ledger.as_ref(),
            },
            self.state.input.run.recording,
        )? != expected
        {
            return Err(invalid());
        }

        Ok(())
    }
    fn finish_quiet(&mut self) -> io::Result<()> {
        if self.link_next < self.links.len() {
            self.select_row_source(self.link_next)?;
        }
        let attempt = self.attempt.as_ref().ok_or_else(invalid)?;
        // Reapply all exact links after restart, even if the slot claims Published.
        if self.link_next < self.links.len() {
            let (namespace, key) = &self.links[self.link_next];
            insert::verify(
                attempt.root,
                &attempt.directory,
                *namespace,
                key,
                &mut self.work.canonical,
            )?;
            self.link_next += 1;
            return Ok(());
        }

        insert::validate_roots(attempt.root, &mut self.work.canonical)?;
        let mut before = [0; 256];
        let mut after = [0; 256];
        codec::encode_state(
            self.state.input.run.recording,
            &NamespaceState::Busy {
                sequence: self.state.sequence,
                transaction: self.state.transaction,
            },
            &mut before,
        )?;
        let sequence = self.state.sequence.checked_add(1).ok_or_else(invalid)?;
        codec::encode_state(
            self.state.input.run.recording,
            &NamespaceState::Quiet { sequence },
            &mut after,
        )?;
        self.work.controls += 1;
        super::publication_control::replace(
            &super::publication_control::CountedControl {
                root: attempt.root,
                ledger: self.ledger.as_ref(),
            },
            "namespace-state",
            &before,
            &after,
            &mut self.work.full_syncs,
        )?;
        let mut next = self.state.clone();
        next.phase = if self.state.row_plan.is_some()
            && ((self.state.original_end == 0 && !self.state.input.is_finite())
                || self.row_unresolved)
        {
            Phase::UnresolvedExtent
        } else if self.state.salvage.is_some() || self.state.row_plan.is_some() {
            Phase::SalvageCommitted
        } else {
            Phase::Cleanup
        };
        self.commit_state(next)
    }
    fn finish_idle(&mut self) -> io::Result<()> {
        let attempt = self.attempt.as_ref().ok_or_else(invalid)?;
        if super::publication_control::read_state(
            &super::publication_control::CountedControl {
                root: attempt.root,
                ledger: self.ledger.as_ref(),
            },
            self.state.input.run.recording,
        )? != (NamespaceState::Quiet {
            sequence: self.state.sequence.checked_add(1).ok_or_else(invalid)?,
        }) {
            return Err(invalid());
        }

        if self.link_next < self.links.len() {
            let (namespace, key) = &self.links[self.link_next];
            insert::verify(
                attempt.root,
                &attempt.directory,
                *namespace,
                key,
                &mut self.work.canonical,
            )?;
            self.link_next += 1;
            return Ok(());
        }

        // A normal original was renamed, never copied. Its sole ready source is canonical.
        // Refuse salvage/residual children instead of removing uncomposed ownership.
        self.work.inventory.open_attempts += 1;
        let base = attempt.root.open_directory(base_name(&attempt.input))?;
        self.work.inventory.open_attempts += 1;
        let object = base.open_directory(
            &collector::recording::identity_text(attempt.transaction).to_string(),
        )?;
        object.visit_names_counted(1, &mut self.work.inventory.directory, |name| {
            if name == "ready" {
                Ok(())
            } else {
                Err(invalid())
            }
        })?;
        sync_directory(&object, &mut self.work)?;
        sync_directory(&base, &mut self.work)?;
        let mut before = [0; 4096];
        let mut idle = [0; 4096];
        self.state.encode(&mut before)?;
        codec::encode_idle(
            self.state.input.run.recording,
            SlotKind::Foreground,
            &mut idle,
        )?;
        self.work.controls += 1;
        super::publication_control::replace(
            &super::publication_control::CountedControl {
                root: attempt.root,
                ledger: self.ledger.as_ref(),
            },
            "foreground",
            &before,
            &idle,
            &mut self.work.full_syncs,
        )?;
        self.done = true;
        Ok(())
    }
}
fn validate_pending(
    root: &PrivateDirectory,
    state: &NormalState,
    metadata: &CanonicalMetadata,
    links: &[(Namespace, CanonicalKey)],
    ledger: Option<&super::PublicationLedger>,
) -> io::Result<()> {
    let Some(candidate) = super::publication_control::pending(
        &super::publication_control::CountedControl { root, ledger },
        state,
    )?
    else {
        return Ok(());
    };
    if candidate.phase != Phase::Registered
        && (candidate.original_end != metadata.original_end
            || candidate.origin != metadata.canonical_origin
            || candidate.loss != metadata.loss)
    {
        return Err(invalid());
    }

    if let Some(plan) = &candidate.structure {
        let (namespace, key) = links
            .get(usize::try_from(candidate.next).map_err(|_| invalid())?)
            .ok_or_else(invalid)?;
        if plan.insertion
            != crate::canonical::NodeLocator::from_key(
                *namespace,
                *key,
                u8::try_from(namespace.width() * 2).map_err(|_| invalid())?,
            )?
        {
            return Err(invalid());
        }
    }
    if matches!(candidate.phase, Phase::Published | Phase::Cleanup)
        && candidate.next != collector::u64_from_usize(links.len())
    {
        return Err(invalid());
    }

    Ok(())
}
fn normal_namespace(
    attempt: &RegisteredAttempt<'_>,
    state: &NormalState,
    ledger: Option<&super::PublicationLedger>,
) -> io::Result<()> {
    let namespace = super::publication_control::read_state(
        &super::publication_control::CountedControl {
            root: attempt.root,
            ledger,
        },
        state.input.run.recording,
    )?;
    match (state.phase, namespace) {
        (Phase::UnresolvedExtent, NamespaceState::Quiet { .. })
            if state.input.is_finite() && state.sequence == 0 =>
        {
            Ok(())
        }
        (
            Phase::UnresolvedExtent,
            NamespaceState::Busy {
                sequence,
                transaction,
            },
        ) if state.input.is_finite()
            && state.sequence == 0
            && sequence > 0
            && transaction == state.transaction =>
        {
            Ok(())
        }
        (
            Phase::Registered | Phase::Ready | Phase::SalvagePlanned,
            NamespaceState::Quiet { .. },
        ) => Ok(()),
        (
            Phase::Ready | Phase::SalvagePlanned,
            NamespaceState::Busy {
                sequence,
                transaction,
            },
        ) if sequence > 0 && transaction == state.transaction => Ok(()),
        (
            Phase::Publishing | Phase::Published,
            NamespaceState::Busy {
                sequence,
                transaction,
            },
        ) if transaction == state.transaction && sequence == state.sequence => Ok(()),
        (
            Phase::Published | Phase::Cleanup | Phase::SalvageCommitted,
            NamespaceState::Quiet { sequence },
        ) if state.sequence.checked_add(1) == Some(sequence) => Ok(()),
        _ => Err(invalid()),
    }
}
fn normal_original_only(
    attempt: &RegisteredAttempt<'_>,
    state: &NormalState,
    ledger: Option<&super::PublicationLedger>,
) -> io::Result<()> {
    if let Some(ledger) = ledger {
        ledger.namespace_attempt();
    }
    let base = attempt.root.open_directory(base_name(&attempt.input))?;
    if let Some(ledger) = ledger {
        ledger.namespace_attempt();
    }
    let object =
        base.open_directory(&collector::recording::identity_text(attempt.transaction).to_string())?;
    let selected = match attempt.placement {
        OriginalPlacement::Pending => "pending",
        OriginalPlacement::Ready => "ready",
    };
    let mut work = PublicationWork::default();
    let result = object.visit_names_counted(2, &mut work.inventory.directory, |name| {
        if name == selected || (name == "original-census" && state.census.kind == 1) {
            Ok(())
        } else {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
    });
    if let Some(ledger) = ledger {
        ledger.record(&work, &mut PublicationWork::default());
    }
    result
}
fn final_loss(
    input: &AttemptInput,
    publication_end: u64,
    event_end: u64,
    totals: &crate::Losses,
) -> io::Result<CanonicalLoss> {
    Ok(CanonicalLoss {
        inherited: input.losses.clone(),
        publication_end,
        event_end,
        publications_new: totals
            .publications
            .checked_sub(input.losses.publications)
            .ok_or_else(invalid)?,
        events_new: totals
            .events
            .checked_sub(input.losses.events)
            .ok_or_else(invalid)?,
        unknown_flags: if totals.unknown {
            u32::from(input.losses.unknown.is_some())
        } else {
            0
        },
    })
}
fn write_metadata(
    attempt: &RegisteredAttempt<'_>,
    metadata: &CanonicalMetadata,
    work: &mut PublicationWork,
    state: &NormalState,
    ledger: Option<&super::PublicationLedger>,
) -> io::Result<()> {
    write_metadata_at(attempt, &attempt.directory, metadata, work, state, ledger)
}
fn write_metadata_at(
    attempt: &RegisteredAttempt<'_>,
    directory: &PrivateDirectory,
    metadata: &CanonicalMetadata,
    work: &mut PublicationWork,
    state: &NormalState,
    ledger: Option<&super::PublicationLedger>,
) -> io::Result<()> {
    metadata.validate()?;
    write_matching_at(
        attempt,
        directory,
        "metadata",
        work,
        state,
        ledger,
        |mut file| metadata.write(&mut file),
    )
}
pub(super) fn write_matching_at(
    attempt: &RegisteredAttempt<'_>,
    directory: &PrivateDirectory,
    name: &str,
    work: &mut PublicationWork,
    state: &NormalState,
    ledger: Option<&super::PublicationLedger>,
    mut emit: impl FnMut(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    work.inventory.open_attempts += 1;
    match directory.open_file(name) {
        Ok(mut file) => {
            let before = file.metadata()?;
            let expected = {
                let mut counted = WorkFile {
                    file: &mut file,
                    work,
                };
                let mut comparison = MetadataComparison {
                    file: &mut counted,
                    retained: before.len(),
                    expected: 0,
                };
                emit(&mut comparison)?;
                comparison.expected
            };
            if before.len() > expected {
                return Err(invalid());
            }

            if before.len() < expected {
                // Only this exact registered, unpublished owner may replace a matching
                // torn prefix. A linked or contradictory file remains evidence and refuses.
                write_metadata_validate_prefix(
                    attempt, directory, name, &before, state, ledger, work,
                )?;

                write_matching_replace(attempt, directory, name, work, &mut emit)?;
            } else {
                work.full_syncs += 1;
                private::full_sync(&file)?;
            }
        }
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && (matches!(state.phase, Phase::Registered | Phase::SalvagePlanned)
                    || (state.input.is_finite()
                        && state.phase == Phase::UnresolvedExtent
                        && state.sequence == 0)) =>
        {
            work.canonical.creates += 1;
            let mut file = directory.create_file(name)?;
            emit(&mut WorkFile {
                file: &mut file,
                work,
            })?;
            work.full_syncs += 1;
            private::full_sync(&file)?;
        }
        Err(error) => return Err(error),
    }
    sync_directory(directory, work)
}
fn write_matching_replace(
    attempt: &RegisteredAttempt<'_>,
    directory: &PrivateDirectory,
    name: &str,
    work: &mut PublicationWork,
    emit: &mut impl FnMut(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    work.full_syncs += 1;
    work.inventory.open_attempts += 1;
    private::full_sync(&attempt.root.open_file("foreground")?)?;
    sync_directory(attempt.root, work)?;
    work.canonical.removals += 1;
    directory.remove_file(name)?;
    sync_directory(directory, work)?;
    work.canonical.creates += 1;
    let mut replacement = directory.create_file(name)?;
    emit(&mut WorkFile {
        file: &mut replacement,
        work,
    })?;
    work.full_syncs += 1;
    private::full_sync(&replacement)
}
fn write_metadata_validate_prefix(
    attempt: &RegisteredAttempt<'_>,
    directory: &PrivateDirectory,
    name: &str,
    before: &std::fs::Metadata,
    state: &NormalState,
    ledger: Option<&super::PublicationLedger>,
    work: &mut PublicationWork,
) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if !(matches!(state.phase, Phase::Registered | Phase::SalvagePlanned)
        || (state.input.is_finite()
            && state.phase == Phase::UnresolvedExtent
            && state.sequence == 0))
        || super::publication_control::read_normal(
            &super::publication_control::CountedControl {
                root: attempt.root,
                ledger,
            },
            attempt.input.run.recording,
        )? != *state
        || before.nlink() != 1
    {
        return Err(invalid());
    }

    normal_namespace(attempt, state, ledger)?;
    work.inventory.open_attempts += 1;
    let current = directory.open_file(name)?.metadata()?;
    if before.dev() != current.dev()
        || before.ino() != current.ino()
        || before.len() != current.len()
        || before.mtime() != current.mtime()
        || before.mtime_nsec() != current.mtime_nsec()
        || before.ctime() != current.ctime()
        || before.ctime_nsec() != current.ctime_nsec()
        || current.nlink() != 1
    {
        return Err(invalid());
    }

    Ok(())
}
struct WorkFile<'file> {
    file: &'file mut std::fs::File,
    work: &'file mut PublicationWork,
}
impl Read for WorkFile<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.work.raw_read_operations += 1;
        self.work.raw_read_requested_bytes += collector::u64_from_usize(bytes.len());
        self.file.read(bytes)
    }
}
impl Write for WorkFile<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.work.raw_write_operations += 1;
        self.work.raw_write_requested_bytes += collector::u64_from_usize(bytes.len());
        self.file.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
struct MetadataComparison<'file, F> {
    file: &'file mut F,
    retained: u64,
    expected: u64,
}
impl<F: Read> Write for MetadataComparison<'_, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut scratch = [0; 4096];
        for chunk in bytes.chunks(scratch.len()) {
            let count = usize::try_from(
                self.retained
                    .saturating_sub(self.expected)
                    .min(collector::u64_from_usize(chunk.len())),
            )
            .map_err(|_| invalid())?;
            self.file.read_exact(&mut scratch[..count])?;
            if scratch[..count] != chunk[..count] {
                return Err(invalid());
            }

            self.expected = self
                .expected
                .checked_add(collector::u64_from_usize(chunk.len()))
                .ok_or_else(invalid)?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn validate_metadata(metadata: &CanonicalMetadata, state: &NormalState) -> io::Result<()> {
    let input = &state.input;
    if metadata.run != input.run
        || metadata.transaction != state.transaction
        || metadata.captured != input.captured
        || metadata.timing() != input.source
        || metadata.binding != input.binding
        || metadata.child != RawChild::Ready
        || metadata.loss.inherited != input.losses
        || metadata.trailing_unknown
    {
        return Err(invalid());
    }

    match input.operation {
        AttemptOperation::Ordinary {
            window,
            attempted_origin,
        } if metadata.kind == CanonicalKind::Ordinary
            && metadata.window == window
            && metadata.attempted_origin == attempted_origin
            && metadata.lifecycle == Lifecycle::Active
            && metadata.evidence == EndpointEvidence::ReadyManifest => {}
        AttemptOperation::Terminal { event_end }
            if metadata.kind == CanonicalKind::Terminal
                && metadata.loss.event_end == event_end
                && metadata.lifecycle == Lifecycle::AcquiredCompletion
                && metadata.evidence == EndpointEvidence::FixedSource
                && metadata.loss.publication_end == input.losses.publication_inherited_end => {}
        _ => return Err(invalid()),
    }
    if let Body::Events(manifest) = &metadata.body {
        if manifest.recovery.rows == 0
            && (manifest.recovery.losses.events != 0 || manifest.recovery.losses.publications != 0)
        {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
    }
    metadata.validate()
}
fn validate_table(
    directory: &PrivateDirectory,
    expected: &crate::TableManifest,
    budget: &Arc<ByteBudget>,
    witness: &super::inventory::NormalInventory,
    work: &mut PublicationWork,
    ledger: Option<&super::PublicationLedger>,
) -> io::Result<()> {
    let name = format!("{}.parquet", expected.name);
    work.canonical.raw_admissions += 1;
    work.canonical.namespace_attempts += 1;
    let file = match directory.open_file(&name) {
        Ok(file) => file,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && crate::schema::DERIVED_TABLES.contains(&expected.name)
                && witness.absent(&name)? =>
        {
            return Ok(());
        }

        Err(error) => return Err(error),
    };
    witness.file(&name, &file)?;
    work.full_syncs += 1;
    private::full_sync(&file)?;
    let table = if let Some(ledger) = ledger {
        crate::engine::AdmittedTable::from_file_streaming_counted(file, budget, ledger)
    } else {
        crate::engine::AdmittedTable::from_file_streaming(file, budget)
    }
    .map_err(rows::admission_error)?;
    if table.row_count() != expected.rows {
        return Err(invalid());
    }

    let catalog = model::catalog::TABLES
        .iter()
        .copied()
        .find(|schema| schema.name == expected.name);
    if let Some(catalog) = catalog {
        for (index, group) in std::iter::once(catalog.columns)
            .chain(catalog.groups.iter().copied())
            .enumerate()
        {
            let mut present = 0;
            for column in group {
                if let Some((_, actual)) = table.fields().find(|(name, _)| *name == column.name) {
                    if crate::query::storage_type(&actual).map_err(crate::metadata::invalid)?
                        != crate::schema::storage_name(column.kind)
                    {
                        return Err(invalid());
                    }

                    present += 1;
                }
            }
            if (index == 0 || present != 0) && present != group.len() {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
fn sync_directory(directory: &PrivateDirectory, work: &mut PublicationWork) -> io::Result<()> {
    work.full_syncs += 1;
    directory.full_sync()
}
pub(super) fn base_name(input: &AttemptInput) -> &'static str {
    match input.operation {
        AttemptOperation::Ordinary { .. } => "data",
        AttemptOperation::Terminal { .. } | AttemptOperation::Finite { .. } => "recovery-data",
    }
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

impl Drop for NormalPublication<'_> {
    fn drop(&mut self) {
        let work = self.work();
        if let (Some(ledger), Some(accounted)) = (&self.ledger, &mut self.accounted) {
            ledger.record(&work, accounted);
        }
    }
}
