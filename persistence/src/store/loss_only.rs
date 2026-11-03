//! Consuming exact zero-survivor disposition; row-bearing salvage remains refused.
use super::{
    Arc, AttemptOperation, ByteBudget, CanonicalKey, CanonicalMetadata, InventoryStep, Namespace,
    NamespaceState, NormalPublication, NormalState, Ordering, OriginalInventory, OriginalPlacement,
    Phase, PrivateDirectory, PublicationStep, PublicationWork, RegisteredAttempt, SlotKind,
    StagingPermit, Store, base_name, codec, insert, invalid, io, normal_namespace, rows,
    sync_directory, validate_pending,
};
use crate::transaction::salvage::{Cleanup, LossPlan};
use collector::ring::RingReader;

pub struct LossPublication<'store> {
    inner: NormalPublication<'store>,
    census: Option<super::super::inventory::census::io_owner::CensusWriter>,
    census_reader: Option<super::super::inventory::census::io_owner::CensusReader>,
    census_binding: Option<crate::transaction::census_binding::CensusBinding>,
    cleanup_admission: usize,
    output_admission: usize,
    proof: Option<super::super::normal_proof::SourceProof>,
    endpoint: Option<u64>,
    snapshot: Option<(u64, u64)>,
    evidence: u32,
    intent: Option<NormalState>,
    adopted: bool,
    ledger: super::super::PublicationLedger,
    accounted: PublicationWork,
}
impl<'store> RegisteredAttempt<'store> {
    /// Consumes writable authority before reconciling exact zero-survivor loss.
    /// ```compile_fail,E0505
    /// use collector::ring::RingReader;
    /// use persistence::{Store, resources::ByteBudget, transaction::AttemptInput};
    /// use std::{io::Write, sync::Arc};
    /// fn loss(store: &Store, input: &AttemptInput, mut reader: RingReader,
    ///         budget: &Arc<ByteBudget>) {
    ///     let attempt = store.register_attempt(input, &mut reader, budget).unwrap();
    ///     let mut file = attempt.create_file("ticks.parquet").unwrap();
    ///     let proof = attempt.prove_loss(reader, budget).unwrap();
    ///     file.write_all(b"late write").unwrap();
    /// }
    /// ```
    pub fn prove_loss(
        self,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<LossPublication<'store>> {
        let ledger = super::super::PublicationLedger::new(budget)?;
        self.prove_loss_with_ledger(reader, budget, &ledger)
    }
    pub fn prove_loss_with_ledger(
        self,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<LossPublication<'store>> {
        ledger.require_budget(budget)?;
        if self.input.losses.unknown.is_some() {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }

        let scratch = budget
            .reserve(64 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let current = super::super::publication_control::read_normal(
            &super::super::publication_control::CountedControl {
                root: self.root,
                ledger: Some(ledger),
            },
            self.input.run.recording,
        )?;
        if current.row_plan.is_some() {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        let proof = source_proof(reader, &self.input, scratch, ledger)?;
        let state = current;
        let inner = NormalPublication::new(self, state, budget, None, Some(ledger.clone()))?;
        Ok(LossPublication {
            inner,
            census: None,
            census_reader: None,
            census_binding: None,
            cleanup_admission: 0,
            output_admission: 0,
            proof: Some(proof),
            endpoint: None,
            snapshot: None,
            evidence: 0,
            intent: None,
            adopted: false,
            ledger: ledger.clone(),
            accounted: PublicationWork::default(),
        })
    }
}
impl Store {
    pub fn resume_loss_publication(
        &self,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<LossPublication<'_>> {
        let ledger = super::super::PublicationLedger::new(budget)?;
        self.resume_loss_owner(None, budget, &ledger)
    }
    pub fn resume_loss_proof(
        &self,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<LossPublication<'_>> {
        let ledger = super::super::PublicationLedger::new(budget)?;
        self.resume_loss_owner(Some(reader), budget, &ledger)
    }
    pub fn resume_loss_publication_with_ledger(
        &self,
        budget: &Arc<ByteBudget>,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<LossPublication<'_>> {
        self.resume_loss_owner(None, budget, ledger)
    }
    pub fn resume_loss_proof_with_ledger(
        &self,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<LossPublication<'_>> {
        self.resume_loss_owner(Some(reader), budget, ledger)
    }
    fn resume_loss_owner(
        &self,
        reader: Option<RingReader>,
        budget: &Arc<ByteBudget>,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<LossPublication<'_>> {
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
        let reservation = budget
            .reserve(256 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let state = super::super::publication_control::read_normal(
            &super::super::publication_control::CountedControl {
                root: &self.directory,
                ledger: Some(ledger),
            },
            self.recording,
        )?;
        if state.row_plan.is_some() {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        ledger.namespace_attempt();
        let base = self.directory.open_directory(base_name(&state.input))?;
        ledger.namespace_attempt();
        let object = base
            .open_directory(&collector::recording::identity_text(state.transaction).to_string())?;
        ledger.namespace_attempt();
        let directory = match optional_directory(&object, "loss")? {
            Some(directory) => directory,
            None if matches!(state.phase, Phase::Registered | Phase::SalvagePlanned) => {
                let mut startup = PublicationWork::default();
                let result =
                    loss_original_counted(&object, &mut startup.cleanup_directory_attempts);
                ledger.record(&startup, &mut PublicationWork::default());
                result?.ok_or_else(invalid)?.0
            }
            None => return Err(invalid()),
        };
        drop(object);
        drop(base);
        let attempt = RegisteredAttempt {
            transaction: state.transaction,
            input: state.input.clone(),
            directory,
            resumed: true,
            candidates: false,
            _permit: permit,
            owner: &self.owner,
            root: &self.directory,
            placement: OriginalPlacement::Pending,
        };
        LossPublication::resume(attempt, state, reader, budget, reservation, ledger)
    }
}
impl<'store> LossPublication<'store> {
    fn resume(
        attempt: RegisteredAttempt<'store>,
        state: NormalState,
        reader: Option<RingReader>,
        budget: &Arc<ByteBudget>,
        reservation: super::ByteReservation,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<Self> {
        normal_namespace(&attempt, &state, Some(ledger))?;
        if state.phase == Phase::Registered {
            return Self::resume_registered(attempt, state, reader, budget, reservation, ledger);
        }
        if state.salvage.is_none() {
            return Err(invalid());
        }

        let mut inner = NormalPublication::blank(attempt, state, budget, reservation);
        inner.ledger = Some(ledger.clone());
        inner.metadata = Some(inner.state.loss_metadata()?);
        Ok(LossPublication {
            inner,
            census: None,
            census_reader: None,
            census_binding: None,
            cleanup_admission: 0,
            output_admission: 0,
            proof: None,
            endpoint: None,
            snapshot: None,
            evidence: 0,
            intent: None,
            adopted: false,
            ledger: ledger.clone(),
            accounted: PublicationWork::default(),
        })
    }
}

impl<'store> LossPublication<'store> {
    fn local_work(&self) -> PublicationWork {
        let mut work = self.inner.work();
        if let Some(proof) = &self.proof {
            work.source_reads = proof.reads;
            work.source_observations = proof.observations.get();
            work.proof_operations = proof.operations;
            (work.proof_publication_next, work.proof_event_next) = proof.positions();
        }
        work
    }
    pub fn work(&self) -> PublicationWork {
        self.ledger.project(&self.local_work(), &self.accounted)
    }
    pub fn step(&mut self) -> io::Result<PublicationStep> {
        let result = self.step_inner();
        self.ledger.record(&self.local_work(), &mut self.accounted);
        result
    }
    fn resume_registered(
        attempt: RegisteredAttempt<'store>,
        state: NormalState,
        reader: Option<RingReader>,
        budget: &Arc<ByteBudget>,
        reservation: super::ByteReservation,
        ledger: &super::super::PublicationLedger,
    ) -> io::Result<Self> {
        if state.input.losses.unknown.is_some() {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }

        let reader = reader.ok_or_else(|| io::Error::from(io::ErrorKind::WouldBlock))?;
        let scratch = budget
            .reserve(64 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let proof = source_proof(reader, &attempt.input, scratch, ledger)?;
        let intent = super::super::publication_control::pending(
            &super::super::publication_control::CountedControl {
                root: attempt.root,
                ledger: Some(ledger),
            },
            &state,
        )?
        .filter(|next| next.census.kind == 2);
        let inner = NormalPublication::new(
            attempt,
            state,
            budget,
            Some(reservation),
            Some(ledger.clone()),
        )?;
        Ok(LossPublication {
            inner,
            census: None,
            census_reader: None,
            census_binding: None,
            cleanup_admission: 0,
            output_admission: 0,
            proof: Some(proof),
            endpoint: None,
            snapshot: None,
            evidence: 0,
            intent,
            adopted: false,
            ledger: ledger.clone(),
            accounted: PublicationWork::default(),
        })
    }
    fn step_inner(&mut self) -> io::Result<PublicationStep> {
        if self.inner.done {
            return Ok(PublicationStep::Idle);
        }

        if self.step_inner_visible()? {
            return Ok(PublicationStep::Continue);
        }
        if self.inner.inventory.is_some() {
            self.census()?;
        } else if self.inner.state.salvage.is_some()
            && self.inner.witness.is_none()
            && self.inner.state.cleanup.region != 8
        {
            self.load_census()?;
        } else if self.inner.state.salvage.is_none() {
            if self.inner.tables_next < super::super::attempt::TABLE_FILES.len() {
                self.original_table()?;
            } else if self.snapshot.is_none() {
                self.prove()?;
            } else if self.census_binding.is_none() {
                self.prepare_census()?;
            } else {
                self.commit_plan()?;
            }
        } else if self.output_admission < 4109 && self.inner.state.cleanup.region != 8 {
            self.output_admission = self.step_inner_admission()?;
        } else if !self.inner.ready {
            self.output()?;
        } else if self.inner.state.phase == Phase::SalvageCommitted {
            self.cleanup()?;
        } else {
            self.inner.step_publication()?;
        }
        Ok(if self.inner.done {
            PublicationStep::Idle
        } else {
            PublicationStep::Continue
        })
    }
    fn step_inner_visible(&mut self) -> io::Result<bool> {
        let owner = if let Some(inventory) = &self.inner.inventory {
            inventory.owner()
        } else {
            self.inner.attempt.as_ref().ok_or_else(invalid)?
        };
        self.inner.work.control_reads += 1;
        super::super::publication_control::visible(
            &super::super::publication_control::CountedControl {
                root: owner.root,
                ledger: Some(&self.ledger),
            },
            &self.inner.state,
        )?;
        if self.inner.state.salvage.is_some() && !self.adopted {
            normal_namespace(owner, &self.inner.state, Some(&self.ledger))?;
            super::super::publication_control::adopt(
                &super::super::publication_control::CountedControl {
                    root: owner.root,
                    ledger: Some(&self.ledger),
                },
                &self.inner.state,
                &mut self.inner.work,
            )?;
            self.adopted = true;
            return Ok(true);
        }

        if self.inner.state.census.kind == 2 && self.inner.state.cleanup.region != 8 {
            super::super::inventory::census::io_owner::check_bound(
                owner,
                &self.inner.state.census,
                &self.ledger,
            )?;
        }
        Ok(false)
    }
    fn step_inner_admission(&self) -> io::Result<usize> {
        let next = self
            .inner
            .witness
            .as_ref()
            .ok_or_else(invalid)?
            .census_cleanup_pass(
                self.inner.attempt.as_ref().ok_or_else(invalid)?,
                &self.inner.state,
                self.output_admission,
                &self.ledger,
            )?;
        Ok(next)
    }
    fn census(&mut self) -> io::Result<()> {
        let inventory = self.inner.inventory.as_mut().ok_or_else(invalid)?;
        let step = inventory.step().map_err(rows::source_error);
        self.inner.work.inventory = inventory.work();
        if step? != InventoryStep::Enumerated {
            return Ok(());
        }

        let result = loss_census(inventory);
        self.inner.work.inventory = inventory.work();
        let (end, evidence) = result?;
        self.evidence = evidence;
        let object =
            super::super::inventory::census::io_owner::object(inventory.owner(), &self.ledger)?;
        self.census = Some(
            super::super::inventory::census::io_owner::CensusWriter::new(
                inventory.census_header(object.device_inode()?)?,
                &self.inner.budget,
                &self.ledger,
            )?,
        );
        let (owner, mut witness) = self
            .inner
            .inventory
            .take()
            .ok_or_else(invalid)?
            .into_normal()?;
        witness.census_bind_header(self.census.as_ref().ok_or_else(invalid)?.header());
        self.inner.attempt = Some(owner);
        self.inner.witness = Some(witness);
        self.endpoint = end;
        Ok(())
    }
    fn prepare_census(&mut self) -> io::Result<()> {
        let retained = self
            .intent
            .as_ref()
            .filter(|state| state.census.kind == 2)
            .map_or(&self.inner.state.census, |state| &state.census);
        let binding = self.census.as_mut().ok_or_else(invalid)?.step(
            self.inner.witness.as_ref().ok_or_else(invalid)?,
            self.inner.attempt.as_ref().ok_or_else(invalid)?,
            retained,
        )?;
        if let Some(binding) = binding {
            if binding.kind == 1 {
                if self.intent.is_none() {
                    let mut next = self.inner.state.clone();
                    next.census = binding;
                    self.intent = Some(next);
                }
                self.inner
                    .commit_state(self.intent.as_ref().ok_or_else(invalid)?.clone())?;
                self.intent = None;
            } else {
                self.census_binding = Some(binding);
            }
        }
        Ok(())
    }
    fn load_census(&mut self) -> io::Result<()> {
        if self.census_reader.is_none() {
            self.census_reader = Some(
                super::super::inventory::census::io_owner::CensusReader::new(
                    &self.inner.budget,
                    &self.ledger,
                )?,
            );
        }
        if let Some(witness) = self.census_reader.as_mut().ok_or_else(invalid)?.step(
            self.inner.attempt.as_ref().ok_or_else(invalid)?,
            &self.inner.state.census,
            &self.inner.budget,
        )? {
            self.inner.witness = Some(witness);
            self.census_reader = None;
        }
        Ok(())
    }
    fn original_table(&mut self) -> io::Result<()> {
        let name = super::super::attempt::TABLE_FILES[self.inner.tables_next];
        let witness = self.inner.witness.as_ref().ok_or_else(invalid)?;
        let owner = self.inner.attempt.as_ref().ok_or_else(invalid)?;
        let table = name.strip_suffix(".parquet").ok_or_else(invalid)?;
        if !witness.absent(name)? && !crate::schema::DERIVED_TABLES.contains(&table) {
            self.inner.work.canonical.namespace_attempts += 1;
            let mut file = owner.directory.open_file(name)?;
            witness.file(name, &file)?;
            let bytes = file.metadata()?.len();
            let framed = super::super::inventory::parquet_framing(&mut file, bytes)?;
            if framed {
                let empty = if name == "ticks.parquet" {
                    rows::zero_tick_file(file, &self.inner.budget, &mut self.inner.work.canonical)?
                } else {
                    rows::zero_table_file(file, &self.inner.budget, &mut self.inner.work.canonical)?
                };
                if !empty {
                    return Err(io::Error::from(io::ErrorKind::Unsupported));
                }
            }
            self.inner.work.canonical.namespace_attempts += 1;
            witness.file(name, &owner.directory.open_file(name)?)?;
        }
        self.inner.tables_next += 1;
        Ok(())
    }
    fn prove(&mut self) -> io::Result<()> {
        self.proof.as_mut().ok_or_else(invalid)?.begin_pass();
        if self.inner.metadata.is_none() {
            self.prepare_metadata()?;
            return Ok(());
        }

        let proof = self.proof.as_mut().ok_or_else(invalid)?;
        let input = &self.inner.state.input;
        proof.begin_pass();
        let metadata = self.inner.metadata.as_ref().ok_or_else(invalid)?;
        if !proof.publications(input, &[], metadata.loss.publication_end)?
            || !proof.events(input, metadata.loss.event_end)?
        {
            return Ok(());
        }

        if matches!(input.operation, AttemptOperation::Terminal { .. })
            && !proof.boundary(input, 0)?
        {
            return Ok(());
        }

        if proof.losses
            != [
                input.losses.publications,
                metadata.loss.publications_new,
                input.losses.events,
                metadata.loss.events_new,
            ]
        {
            return Err(invalid());
        }

        proof.check(input)?;
        if self.evidence == 3 {
            let Some(end) = proof.loss_event_end(input)? else {
                return Ok(());
            };
            if end != metadata.loss.event_end {
                return Err(invalid());
            }
        }
        let Some(snapshot) = proof.loss_snapshot(input, metadata.loss.event_end)? else {
            return Ok(());
        };
        self.snapshot = Some(snapshot);
        Ok(())
    }
    fn commit_plan(&mut self) -> io::Result<()> {
        self.proof
            .as_mut()
            .ok_or_else(invalid)?
            .check(&self.inner.state.input)?;
        let metadata = self.inner.metadata.as_ref().ok_or_else(invalid)?;
        let snapshot = self.snapshot.ok_or_else(invalid)?;
        let mut next = proof_plan(
            &self.inner.state,
            self.intent.as_ref(),
            metadata,
            snapshot,
            self.evidence,
        );
        next.census = self.census_binding.ok_or_else(invalid)?;
        self.intent = Some(next.clone());
        self.inner.commit_state(next)?;
        self.adopted = true;
        if let Some(proof) = &self.proof {
            self.inner.work.source_reads = proof.reads;
            self.inner.work.source_observations = proof.observations.get();
            self.inner.work.proof_operations = proof.operations;
            (
                self.inner.work.proof_publication_next,
                self.inner.work.proof_event_next,
            ) = proof.positions();
        }
        self.proof = None;
        Ok(())
    }
    fn prepare_metadata(&mut self) -> io::Result<()> {
        let input = &self.inner.state.input;
        let proof = self.proof.as_mut().ok_or_else(invalid)?;
        if self.endpoint.is_none() {
            let Some(end) = proof.loss_event_end(input)? else {
                return Ok(());
            };
            self.endpoint = Some(end);
        }
        let owner = self.inner.attempt.as_ref().ok_or_else(invalid)?;
        if let Some(candidate) = super::super::publication_control::pending(
            &super::super::publication_control::CountedControl {
                root: owner.root,
                ledger: Some(&self.ledger),
            },
            &self.inner.state,
        )? {
            if candidate.phase == Phase::SalvagePlanned && candidate.salvage.is_some() {
                if Some(candidate.loss.event_end) != self.endpoint
                    || candidate.salvage.as_ref().ok_or_else(invalid)?.endpoint != self.evidence
                {
                    return Err(invalid());
                }

                self.inner.metadata = Some(candidate.loss_metadata()?);
                self.intent = Some(candidate);
                return Ok(());
            }
        }
        let publication_end = proof.loss_publication_end(input)?;
        let end = self.endpoint.ok_or_else(invalid)?;
        let mut state = self.inner.state.clone();
        state.loss.publication_end = publication_end;
        state.loss.event_end = end;
        state.loss.publications_new = publication_end
            .checked_sub(input.losses.publication_inherited_end)
            .ok_or_else(invalid)?;
        state.loss.events_new = end
            .checked_sub(input.losses.event_inherited_end)
            .ok_or_else(invalid)?;
        state.original_end = match input.operation {
            AttemptOperation::Ordinary {
                attempted_origin, ..
            } if publication_end > attempted_origin => publication_end,
            _ => 0,
        };
        state.salvage = Some(LossPlan {
            first: input.losses.event_start,
            end,
            snapshot_first: input.losses.event_start,
            snapshot_end: end,
            layout: input.source.layout,
            endpoint: self.evidence,
            terminal: matches!(input.operation, AttemptOperation::Terminal { .. }),
        });
        let metadata = state.loss_metadata()?;
        metadata.validate()?;
        self.inner.metadata = Some(metadata);
        Ok(())
    }
    fn output(&mut self) -> io::Result<()> {
        let attempt = self.inner.attempt.as_mut().ok_or_else(invalid)?;
        self.inner.work.canonical.namespace_attempts += 1;
        let base = attempt.root.open_directory(base_name(&attempt.input))?;
        self.inner.work.canonical.namespace_attempts += 1;
        let object = base.open_directory(
            &collector::recording::identity_text(attempt.transaction).to_string(),
        )?;
        object.visit_names_counted(4, &mut self.inner.work.inventory.directory, loss_child)?;
        let loss = object.create_directory_counted("loss", &mut self.inner.work.full_syncs)?;
        sync_directory(&loss, &mut self.inner.work)?;
        sync_directory(&object, &mut self.inner.work)?;
        let metadata = self.inner.metadata.as_ref().ok_or_else(invalid)?;
        loss.visit_names_counted(1, &mut self.inner.work.inventory.directory, |name| {
            if name == "metadata" {
                Ok(())
            } else {
                Err(invalid())
            }
        })?;
        attempt.directory = loss;
        super::write_metadata(
            attempt,
            metadata,
            &mut self.inner.work,
            &self.inner.state,
            Some(&self.ledger),
        )?;
        sync_directory(&attempt.directory, &mut self.inner.work)?;
        sync_directory(&object, &mut self.inner.work)?;
        self.inner.links = loss_links(metadata)?;
        validate_pending(
            attempt.root,
            &self.inner.state,
            metadata,
            &self.inner.links,
            Some(&self.ledger),
        )?;
        self.inner.ready = true;
        Ok(())
    }
    fn cleanup(&mut self) -> io::Result<()> {
        if self.inner.state.cleanup.region != 8 && self.cleanup_admission < 4109 {
            self.cleanup_admission = self
                .inner
                .witness
                .as_ref()
                .ok_or_else(invalid)?
                .census_cleanup_pass(
                    self.inner.attempt.as_ref().ok_or_else(invalid)?,
                    &self.inner.state,
                    self.cleanup_admission,
                    &self.ledger,
                )?;
            return Ok(());
        }
        cleanup_original(&mut self.inner)
    }
}
fn loss_census(inventory: &OriginalInventory<'_, '_>) -> io::Result<(Option<u64>, u32)> {
    // Any surviving raw row, even at a sparse high ordinal, belongs to general salvage.
    if inventory.physical_rows() != 0
        || inventory.batches().iter().any(|fact| {
            matches!(
                fact,
                super::super::BatchFact::Malformed { framed: true, .. }
            )
        })
        || inventory
            .file(super::super::OriginalFile::Metadata)
            .is_some()
    {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }

    let end = match inventory.owner().input.operation {
        AttemptOperation::Ordinary { .. } => match inventory.index() {
            super::super::IndexFact::Valid {
                first,
                end,
                rows: 0,
                batches: 0,
                ..
            } if first == inventory.owner().input.losses.event_start => (Some(end), 1),
            super::super::IndexFact::Missing | super::super::IndexFact::Malformed { .. } => {
                (None, 3)
            }
            _ => return Err(io::Error::from(io::ErrorKind::WouldBlock)),
        },
        AttemptOperation::Terminal { event_end } => (Some(event_end), 3),
    };
    Ok(end)
}
fn proof_plan(
    state: &NormalState,
    intent: Option<&NormalState>,
    metadata: &CanonicalMetadata,
    snapshot: (u64, u64),
    evidence: u32,
) -> NormalState {
    let mut next = intent.cloned().unwrap_or_else(|| state.clone());
    next.original_end = metadata.original_end;
    next.loss = metadata.loss.clone();
    if next.salvage.is_none() {
        next.salvage = Some(LossPlan {
            first: state.input.losses.event_start,
            end: metadata.loss.event_end,
            snapshot_first: snapshot.0,
            snapshot_end: snapshot.1,
            layout: state.input.source.layout,
            endpoint: evidence,
            terminal: matches!(state.input.operation, AttemptOperation::Terminal { .. }),
        });
    }
    next.phase = Phase::SalvagePlanned;
    next
}
fn loss_links(metadata: &CanonicalMetadata) -> io::Result<Vec<(Namespace, CanonicalKey)>> {
    let mut links = Vec::with_capacity(2);
    for namespace in metadata.loss_routes()?.into_iter().flatten() {
        let origin = match namespace {
            Namespace::Fragments => metadata.loss.inherited.event_start,
            Namespace::Ordinary => metadata.attempted_origin,
            Namespace::Terminal => 0,
            _ => return Err(invalid()),
        };
        links.push((
            namespace,
            CanonicalKey::source(namespace, &metadata.run, origin)?,
        ));
    }
    Ok(links)
}
fn cleanup_original(inner: &mut NormalPublication<'_>) -> io::Result<()> {
    cleanup_original_quiet(inner)?;
    if cleanup_original_gate(inner)? {
        return Ok(());
    }
    let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
    inner.work.cleanup_directory_attempts += 1;
    let base = attempt.root.open_directory(base_name(&attempt.input))?;
    inner.work.cleanup_directory_attempts += 1;
    let object =
        base.open_directory(&collector::recording::identity_text(attempt.transaction).to_string())?;
    drop(base);
    object.visit_names_counted(4, &mut inner.work.inventory.directory, loss_child)?;
    if inner.state.cleanup.region == 8 {
        super::super::inventory::census::io_owner::retire(
            attempt,
            &inner.state,
            inner.ledger.as_ref().ok_or_else(invalid)?,
        )?;
        cleanup_original_idle(inner)?;
        inner.done = true;
        return Ok(());
    }
    let original = loss_original_counted(&object, &mut inner.work.cleanup_directory_attempts)?;
    if let Some((directory, _)) = &original {
        inner
            .witness
            .as_ref()
            .ok_or_else(invalid)?
            .cleanup_original_directory(directory)?;
    }
    let mut next = inner.state.clone();
    let mut cursor = next.cleanup;
    cursor = cleanup_pass(
        cursor,
        original.as_ref(),
        &mut inner.work,
        &CleanupAdmission {
            input: &attempt.input,
            budget: &inner.budget,
            event_end: inner.state.loss.event_end,
            witness: inner.witness.as_ref().ok_or_else(invalid)?,
        },
        &object,
    )?;
    if cursor.directory == 2 {
        inner.work.cleanup_directory_attempts += 1;
        let base = attempt.root.open_directory(base_name(&attempt.input))?;
        sync_directory(&base, &mut inner.work)?;
        next.cleanup = Cleanup {
            region: 8,
            ..Default::default()
        };
        inner.commit_state(next)?;
    } else {
        next.cleanup = cursor;
        drop(original);
        drop(object);
        inner.commit_state(next)?;
    }
    Ok(())
}
fn cleanup_original_gate(inner: &mut NormalPublication<'_>) -> io::Result<bool> {
    let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
    if inner.link_next < inner.links.len() {
        let (namespace, key) = &inner.links[inner.link_next];
        insert::verify(
            attempt.root,
            &attempt.directory,
            *namespace,
            key,
            &mut inner.work.canonical,
        )?;
        inner.link_next += 1;
        return Ok(true);
    }

    Ok(false)
}
fn cleanup_original_quiet(inner: &NormalPublication<'_>) -> io::Result<()> {
    let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
    if super::super::publication_control::read_state(
        &super::super::publication_control::CountedControl {
            root: attempt.root,
            ledger: inner.ledger.as_ref(),
        },
        attempt.input.run.recording,
    )? != (NamespaceState::Quiet {
        sequence: inner.state.sequence.checked_add(1).ok_or_else(invalid)?,
    }) {
        return Err(invalid());
    }

    Ok(())
}
fn cleanup_original_idle(inner: &mut NormalPublication<'_>) -> io::Result<()> {
    let attempt = inner.attempt.as_ref().ok_or_else(invalid)?;
    inner.work.cleanup_directory_attempts += 1;
    let base = attempt.root.open_directory(base_name(&attempt.input))?;
    sync_directory(&base, &mut inner.work)?;
    drop(base);
    let mut before = [0; 4096];
    let mut idle = [0; 4096];
    inner.state.encode(&mut before)?;
    codec::encode_idle(attempt.input.run.recording, SlotKind::Foreground, &mut idle)?;
    inner.work.controls += 1;
    super::super::publication_control::replace(
        &super::super::publication_control::CountedControl {
            root: attempt.root,
            ledger: inner.ledger.as_ref(),
        },
        "foreground",
        &before,
        &idle,
        &mut inner.work.full_syncs,
    )?;
    Ok(())
}
struct CleanupAdmission<'a> {
    input: &'a crate::transaction::AttemptInput,
    budget: &'a Arc<ByteBudget>,
    event_end: u64,
    witness: &'a super::super::inventory::NormalInventory,
}
fn cleanup_pass(
    mut cursor: Cleanup,
    original: Option<&(PrivateDirectory, &'static str)>,
    work: &mut PublicationWork,
    admission: &CleanupAdmission<'_>,
    object: &PrivateDirectory,
) -> io::Result<Cleanup> {
    match cursor.region {
        0 => {
            cursor = Cleanup {
                region: 1,
                ..Cleanup::default()
            }
        }
        1 => return cleanup_batches(cursor, original, work, admission),
        2 => {
            if let Some((directory, _)) = original {
                if matches!(admission.input.operation, AttemptOperation::Ordinary { .. }) {
                    for name in super::super::attempt::TABLE_FILES {
                        remove_raw(directory, name, work, admission)?;
                    }
                }
                sync_directory(directory, work)?;
            } else {
                return Err(invalid());
            }

            cursor = Cleanup {
                region: 3,
                ..Cleanup::default()
            };
        }
        3 => return cleanup_directories(cursor, original, work, object, admission),
        _ => return Err(invalid()),
    }
    Ok(cursor)
}
fn cleanup_batches(
    mut cursor: Cleanup,
    original: Option<&(PrivateDirectory, &'static str)>,
    work: &mut PublicationWork,
    admission: &CleanupAdmission<'_>,
) -> io::Result<Cleanup> {
    if let Some((directory, _)) = original {
        let events = if matches!(admission.input.operation, AttemptOperation::Ordinary { .. }) {
            {
                work.cleanup_directory_attempts += 1;
                optional_directory(directory, "events")?
            }
        } else {
            None
        };
        if let Some(events) = &events {
            admission.witness.cleanup_directories(directory, events)?;
        }
        let parent = if matches!(admission.input.operation, AttemptOperation::Ordinary { .. }) {
            events.as_ref()
        } else {
            Some(directory)
        };
        let attempts = work.cleanup_file_attempts;
        for _ in 0..128 {
            if cursor.ordinal == 4096 || work.cleanup_file_attempts - attempts >= 127 {
                break;
            }
            if let Some(parent) = parent {
                remove_raw(
                    parent,
                    &crate::event_recovery::batch_name(
                        usize::try_from(cursor.ordinal).map_err(|_| invalid())?,
                    ),
                    work,
                    admission,
                )?;
            } else {
                work.cleanup_positions += 1;
            }
            cursor.ordinal += 1;
        }
        sync_directory(parent.unwrap_or(directory), work)?;
    } else if cursor.ordinal != 4096 {
        return Err(invalid());
    }

    if cursor.ordinal == 4096 {
        cursor = Cleanup {
            region: 2,
            ..Cleanup::default()
        };
    }
    Ok(cursor)
}
fn cleanup_directories(
    mut cursor: Cleanup,
    original: Option<&(PrivateDirectory, &'static str)>,
    work: &mut PublicationWork,
    object: &PrivateDirectory,
    admission: &CleanupAdmission<'_>,
) -> io::Result<Cleanup> {
    if let Some((directory, name)) = original {
        if cursor.directory == 0 {
            if matches!(admission.input.operation, AttemptOperation::Ordinary { .. }) {
                work.cleanup_directory_attempts += 1;
                if let Some(events) = optional_directory(directory, "events")? {
                    admission.witness.cleanup_directories(directory, &events)?;
                    remove_raw(&events, "index", work, admission)?;
                    events.visit_names_counted(0, &mut work.inventory.directory, |_| {
                        Err(invalid())
                    })?;
                    sync_directory(&events, work)?;
                    work.cleanup_removals += 1;
                    directory.remove_directory("events")?;
                }
            }
            sync_directory(directory, work)?;
            cursor.directory = 1;
        } else if cursor.directory == 1 {
            remove_raw(directory, "metadata", work, admission)?;
            directory.visit_names_counted(0, &mut work.inventory.directory, |_| Err(invalid()))?;
            sync_directory(directory, work)?;
            work.cleanup_removals += 1;
            object.remove_directory(name)?;
            sync_directory(object, work)?;
            cursor.directory = 2;
        } else {
            return Err(invalid());
        }
    } else {
        if cursor.directory < 1 {
            return Err(invalid());
        }

        sync_directory(object, work)?;
        cursor.directory = 2;
    }
    Ok(cursor)
}
fn optional_directory(
    parent: &PrivateDirectory,
    name: &str,
) -> io::Result<Option<PrivateDirectory>> {
    match parent.open_directory(name) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
fn loss_child(name: &str) -> io::Result<()> {
    if matches!(name, "loss" | "pending" | "ready" | "original-census") {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn loss_original_counted(
    object: &PrivateDirectory,
    attempts: &mut u64,
) -> io::Result<Option<(PrivateDirectory, &'static str)>> {
    *attempts += 1;
    let pending = optional_directory(object, "pending")?;
    *attempts += 1;
    let ready = optional_directory(object, "ready")?;
    match (pending, ready) {
        (Some(directory), None) => Ok(Some((directory, "pending"))),
        (None, Some(directory)) => Ok(Some((directory, "ready"))),
        (None, None) => Ok(None),
        _ => Err(invalid()),
    }
}
fn remove_raw(
    directory: &PrivateDirectory,
    name: &str,
    work: &mut PublicationWork,
    admission: &CleanupAdmission<'_>,
) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    work.cleanup_positions += 1;
    work.cleanup_file_attempts += 1;
    match directory.open_file(name) {
        Ok(mut file) => {
            let before = super::super::inventory::FileIdentity::read(&file)?;
            admission.witness.cleanup_named(name, &file)?;
            cleanup_admit(&mut file, name, work, admission)?;
            work.cleanup_file_attempts += 1;
            let current = directory.open_file(name)?;
            if super::super::inventory::FileIdentity::read(&file)? != before
                || super::super::inventory::FileIdentity::read(&current)? != before
                || file.metadata()?.nlink() != 1
            {
                return Err(invalid());
            }

            work.cleanup_removals += 1;
            directory.remove_file(name)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn cleanup_admit(
    file: &mut std::fs::File,
    name: &str,
    work: &mut PublicationWork,
    admission: &CleanupAdmission<'_>,
) -> io::Result<()> {
    if name == "metadata" {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    if name == "index" {
        let input = admission.input;
        let AttemptOperation::Ordinary { window, .. } = input.operation else {
            return Err(invalid());
        };
        let result = crate::event_table::index::Index::read_attempt_file(
            file,
            &crate::WindowIdentity {
                run: input.run,
                window,
            },
            Some(input.captured),
        );
        let index = match result {
            Ok(index) => index,
            Err(crate::Error::Bound(_)) => return Ok(()),
            Err(error) => return Err(rows::source_error(error)),
        };
        if index.rows != 0
            || !index.batches.is_empty()
            || index.first != input.losses.event_start
            || index.end != admission.event_end
        {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        return Ok(());
    }
    if !super::super::inventory::parquet_framing(file, file.metadata()?.len())? {
        return Ok(());
    }
    if !super::super::attempt::TABLE_FILES.contains(&name) {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    let table = name.strip_suffix(".parquet").ok_or_else(invalid)?;
    if crate::schema::DERIVED_TABLES.contains(&table) {
        return Ok(());
    }
    let empty = if name == "ticks.parquet" {
        rows::zero_tick_file(file.try_clone()?, admission.budget, &mut work.canonical)?
    } else {
        rows::zero_table_file(file.try_clone()?, admission.budget, &mut work.canonical)?
    };
    if !empty {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    Ok(())
}

impl Drop for LossPublication<'_> {
    fn drop(&mut self) {
        self.ledger.record(&self.local_work(), &mut self.accounted);
    }
}

fn source_proof(
    reader: RingReader,
    input: &crate::transaction::AttemptInput,
    scratch: super::ByteReservation,
    ledger: &super::super::PublicationLedger,
) -> io::Result<super::super::normal_proof::SourceProof> {
    let result = super::super::normal_proof::SourceProof::new(reader, input, scratch);
    ledger.update(|work| work.source_observations += 1);
    if let Ok(proof) = &result {
        proof.observations.set(0);
    }
    result
}
