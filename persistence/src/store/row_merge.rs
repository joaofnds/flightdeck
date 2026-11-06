//! Volatile input merge. No namespace writes or lifecycle completion authority.
#![allow(dead_code)] // Crate-private merge inspection seams are exercised by tests.
use super::normal_proof::SourceProof;
use super::{
    BatchFact, IndexFact, OriginalInventory, PublicationLedger, PublicationWork, RegisteredAttempt,
};
use crate::event_recovery::{Batch, batch, read};
use crate::resources::{ByteBudget, ByteReservation};
use collector::ring::{FoundEvent, IdentityObservation, RingReader};
use polars::prelude::*;
use std::{io, sync::Arc};

pub(crate) struct RowMerge<'store> {
    pub(super) inventory: MergeInventory<'store>,
    source: MergeSource,
    budget: Arc<ByteBudget>,
    ledger: PublicationLedger,
    recorded: PublicationWork,
    selected: Option<std::ops::Range<u64>>,
    source_end: u64,
    source_first: u64,
    prefix_end: u64,
    fixed: Option<crate::transaction::salvage::RowPlan>,
    next: u64,
    ordinal: usize,
    row: usize,
    input: Option<DataFrame>,
    output: Option<MergeBatch>,
    ready: bool,
    output_rows_max: usize,
    pub losses: [u64; 2],
    pub unresolved: u64,
}
pub(super) struct MergeInventory<'store>(Option<OriginalInventory<'store, 'store>>);
impl<'store> std::ops::Deref for MergeInventory<'store> {
    type Target = OriginalInventory<'store, 'store>;
    fn deref(&self) -> &Self::Target {
        match &self.0 {
            Some(value) => value,
            None => panic!("consumed merge inventory"),
        }
    }
}
impl std::ops::DerefMut for MergeInventory<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match &mut self.0 {
            Some(value) => value,
            None => panic!("consumed merge inventory"),
        }
    }
}
struct MergeSource(Option<SourceProof>);
impl std::ops::Deref for MergeSource {
    type Target = SourceProof;
    fn deref(&self) -> &Self::Target {
        match &self.0 {
            Some(value) => value,
            None => panic!("consumed merge source"),
        }
    }
}
impl std::ops::DerefMut for MergeSource {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match &mut self.0 {
            Some(value) => value,
            None => panic!("consumed merge source"),
        }
    }
}
/// The reservation travels with all Arrow/native allocations exposed to the writer.
pub(crate) struct MergeBatch {
    rows: Vec<batch::Row>,
    bytes: usize,
    _reservation: ByteReservation,
}
impl MergeBatch {
    pub(super) fn read(
        file: &std::fs::File,
        fact: &Batch,
        identity: &super::FileIdentity,
        captured: bool,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> crate::Result<Self> {
        let reservation = budget.reserve(batch::WORKING_BYTES).ok_or_else(memory)?;
        ledger.update(|work| {
            work.inventory.raw_admissions += 1;
            work.inventory.raw_file_clones += 1;
        });
        if super::FileIdentity::read(file)? != *identity {
            return Err(invalid().into());
        }
        let mut decoded = 0;
        let result = read::read_batch_file_counted(
            file.try_clone()?,
            fact,
            captured,
            true,
            budget,
            &mut decoded,
        );
        ledger.update(|work| work.inventory.decoded_bytes += decoded);
        let frame = result?;
        if super::FileIdentity::read(file)? != *identity {
            return Err(invalid().into());
        }
        let mut rows = Vec::with_capacity(crate::event_observation::BATCH_ROWS_MAX);
        for position in 0..frame.height() {
            rows.push(batch::owned_row(&frame, position, captured)?);
        }
        Ok(Self {
            rows,
            bytes: batch::ROWS_BYTES_MAX,
            _reservation: reservation,
        })
    }
    pub(crate) fn rows(&self) -> &[batch::Row] {
        &self.rows
    }
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MergeStep {
    Continue,
    Batch,
    Finished,
}
impl<'store> RowMerge<'store> {
    pub(crate) fn new(
        attempt: RegisteredAttempt<'store>,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<Self> {
        Self::new_optional(attempt, Some(reader), budget, ledger)
    }
    pub(super) fn has_source(&self) -> bool {
        self.source.0.is_some()
    }
    pub(super) fn new_optional(
        attempt: RegisteredAttempt<'store>,
        reader: Option<RingReader>,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<Self> {
        ledger.require_budget(budget)?;
        let observations = u64::from(reader.is_some());
        let source = reader
            .map(|reader| {
                let reservation = budget.reserve(batch::WORKING_BYTES).ok_or_else(memory)?;
                let result = SourceProof::new(reader, attempt.input(), reservation);
                ledger.update(|work| work.source_observations += 1);
                result
            })
            .transpose()?;
        let mut startup = super::InventoryWork::default();
        let result = OriginalInventory::owned_counted(attempt, budget, &mut startup);
        ledger.record(
            &PublicationWork {
                inventory: startup,
                ..PublicationWork::default()
            },
            &mut PublicationWork::default(),
        );
        let inventory = result?;
        Ok(Self {
            inventory: MergeInventory(Some(inventory)),
            source: MergeSource(source),
            budget: Arc::clone(budget),
            ledger: ledger.clone(),
            recorded: PublicationWork {
                inventory: startup,
                source_observations: observations,
                ..PublicationWork::default()
            },
            selected: None,
            source_end: 0,
            source_first: 0,
            prefix_end: 0,
            fixed: None,
            next: 0,
            ordinal: 0,
            row: 0,
            input: None,
            output: None,
            ready: false,
            output_rows_max: crate::event_observation::BATCH_ROWS_MAX,
            losses: [0; 2],
            unresolved: 0,
        })
    }
    pub(super) fn into_publication(
        mut self,
    ) -> io::Result<(
        RegisteredAttempt<'store>,
        Option<SourceProof>,
        super::inventory::NormalInventory,
    )> {
        self.release_input();
        self.record();
        let inventory = self.inventory.0.take().ok_or_else(invalid)?;
        let (attempt, witness) = inventory.into_normal()?;
        let mut source = self.source.0.take();
        if let Some(source) = &mut source {
            source.release_workspace();
        }
        Ok((attempt, source, witness))
    }
    pub(super) fn fixed(&mut self, plan: crate::transaction::salvage::RowPlan) {
        self.fixed = Some(plan);
    }
    pub(super) fn release_input(&mut self) {
        if self
            .input
            .as_ref()
            .is_some_and(|frame| self.row == frame.height())
        {
            self.row = 0;
            self.ordinal += 1;
        }
        self.input = None;
    }
    pub(super) fn replay(&mut self, row: &batch::Row) -> crate::Result<bool> {
        let result = self.replay_inner(row);
        self.record();
        result
    }
    pub(super) fn replay_chunk(&mut self, rows: &[batch::Row]) -> crate::Result<usize> {
        assert!(rows.len() <= 128);
        let result = self.replay_chunk_inner(rows);
        self.record();
        result
    }
    fn replay_chunk_inner(&mut self, rows: &[batch::Row]) -> crate::Result<usize> {
        if rows.is_empty() || !self.load()? {
            return Ok(0);
        }
        let Some(frame) = &self.input else {
            return Ok(0);
        };
        let count = rows.len().min(frame.height() - self.row);
        if count == 0 || rows[0].event.number != self.next {
            return Ok(0);
        }
        self.ledger.update(|work| work.proof_operations += 1);
        let end = self
            .next
            .checked_add(collector::u64_from_usize(count))
            .ok_or_else(invalid)?;
        let relevant_first = self.next.max(self.source_first);
        let relevant_end = end.min(self.source_end);
        if relevant_first < relevant_end && self.source.0.is_some() {
            let input = self.inventory.owner().input();
            let missing = self.source.checked_read(input, |reader| {
                reader.seek_event(relevant_first)?;
                Ok(
                    matches!(reader.read_event()?, FoundEvent::Missed {first,end}
                    if first == relevant_first && end >= relevant_end
                        && first >= input.binding.event_first),
                )
            })?;
            if !missing {
                return Ok(0);
            }
        }
        let numbers = frame.column("event")?.u64()?;
        for (position, row) in rows[..count].iter().enumerate() {
            self.ledger.update(|work| work.proof_operations += 1);
            if row.event.number
                != self
                    .next
                    .checked_add(collector::u64_from_usize(position))
                    .ok_or_else(invalid)?
                || numbers.get(self.row + position) != Some(row.event.number)
            {
                return Ok(0);
            }
        }
        self.ledger.update(|work| work.proof_operations += 1);
        let expected = batch::frame(&rows[..count], self.inventory.owner().input().captured)?;
        let observed = frame.slice(i64::try_from(self.row).map_err(|_| invalid())?, count);
        self.ledger.update(|work| work.proof_operations += 1);
        if !expected.equals_missing(&observed) {
            return Err(invalid().into());
        }
        let next = self
            .next
            .checked_add(collector::u64_from_usize(count))
            .ok_or_else(invalid)?;
        self.row += count;
        self.next = next;
        Ok(count)
    }
    fn replay_inner(&mut self, row: &batch::Row) -> crate::Result<bool> {
        if row.event.number < self.next {
            return Err(invalid().into());
        }
        if !self.load()? {
            return Ok(false);
        }
        self.merge_one(
            row.event.number.checked_add(1).ok_or_else(invalid)?,
            Some(row),
        )?;
        Ok(self.next > row.event.number)
    }
    pub(crate) fn range(&self) -> Option<std::ops::Range<u64>> {
        self.selected.clone()
    }
    pub(crate) fn source_range(&self) -> Option<std::ops::Range<u64>> {
        self.selected.as_ref().map(|range| {
            let first = self.source_first.max(range.start).min(range.end);
            first..self.source_end.min(range.end).max(first)
        })
    }
    pub(crate) fn position(&self) -> u64 {
        self.next
    }
    pub(crate) fn take_batch(&mut self) -> Option<MergeBatch> {
        if self.ready {
            self.ready = false;
            self.output.take()
        } else {
            None
        }
    }
    pub(super) fn output_rows_limit(&mut self, remaining: u64) -> io::Result<()> {
        self.output_rows_max = usize::try_from(remaining.min(collector::u64_from_usize(
            crate::event_observation::BATCH_ROWS_MAX,
        )))
        .map_err(|_| invalid())?;
        if self.output_rows_max == 0 {
            return Err(invalid());
        }
        Ok(())
    }
    pub(crate) fn step(&mut self) -> crate::Result<MergeStep> {
        let result = self.step_inner();
        self.record();
        result
    }
    fn record(&mut self) {
        if self.inventory.0.is_none() {
            return;
        }
        let work = PublicationWork {
            inventory: self.inventory.work(),
            source_reads: self.source.0.as_ref().map_or(0, |source| source.reads),
            source_observations: self
                .source
                .0
                .as_ref()
                .map_or(0, |source| source.observations.get()),
            proof_operations: self.source.0.as_ref().map_or(0, |source| source.operations),
            proof_event_next: self.next,
            ..PublicationWork::default()
        };
        self.ledger.record(&work, &mut self.recorded);
    }
    fn step_inner(&mut self) -> crate::Result<MergeStep> {
        if let Some(source) = &self.source.0 {
            source.check(self.inventory.owner().input())?;
        }
        if self.ready {
            return Ok(MergeStep::Batch);
        }
        if !self.inventory.enumerated() {
            self.inventory.step()?;
            return Ok(MergeStep::Continue);
        }
        if self.source.0.is_none() && self.inventory.owner().input().is_finite() {
            return Err(io::Error::from(io::ErrorKind::WouldBlock).into());
        }

        if self.selected.is_none() {
            self.freeze()?;
            return Ok(MergeStep::Continue);
        }
        let end = self.selected.as_ref().ok_or_else(invalid)?.end;
        for _ in 0..128 {
            if self.next == end {
                if self.output.is_some() {
                    self.ready = true;
                    return Ok(MergeStep::Batch);
                }
                return Ok(MergeStep::Finished);
            }
            if !self.load()? {
                return Ok(MergeStep::Continue);
            }
            self.merge_one(end, None)?;
            if self.ready {
                return Ok(MergeStep::Batch);
            }
        }
        Ok(MergeStep::Continue)
    }
    fn freeze_original_end(&self) -> crate::Result<u64> {
        if self.inventory.has_overlaps() || self.inventory.has_unordered_extents() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "general overlapping-original merge remains open",
            )
            .into());
        }
        if self.inventory.exceeds_logical_limit() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "ordered original aggregate exceeds logical row bound",
            )
            .into());
        }
        let mut end = self.inventory.registered_event_start();
        for fact in self.inventory.batches() {
            match fact {
                BatchFact::Complete { end: last, .. } => end = end.max(*last),
                BatchFact::Malformed { framed: true, .. } | BatchFact::Unprobed => {
                    return Err(invalid().into());
                }
                _ => {}
            }
        }
        if let IndexFact::Valid {
            end: indexed, rows, ..
        } = self.inventory.index()
        {
            if end > indexed
                || self.inventory.physical_rows() > rows
                || rows > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
            {
                return Err(invalid().into());
            }
            for (ordinal, fact) in self.inventory.batches().iter().enumerate() {
                if let BatchFact::Complete {
                    first, end, rows, ..
                } = fact
                {
                    if self
                        .inventory
                        .index_batch(u32::try_from(ordinal).map_err(|_| invalid())?)
                        != Some((*first, *end, *rows))
                    {
                        return Err(invalid().into());
                    }
                }
            }
            end = indexed;
        }
        Ok(end)
    }
    fn freeze_source(&mut self) -> io::Result<(u64, u64, u64)> {
        let input = self.inventory.owner().input();
        self.source.checked_read(input, |reader| {
            let newest = reader.event_newest()?.checked_add(1).ok_or_else(invalid)?;
            let origin = input.binding.event_first;
            reader.seek_event(origin)?;
            let (first, prefix) = match reader.read_event()? {
                FoundEvent::Missed { first, end } if first == origin && end > first => (end, end),
                FoundEvent::Event { number, .. } if number == origin => (origin, origin),
                FoundEvent::Nothing if newest == origin => (origin, origin),
                _ => return Err(invalid()),
            };
            if first > newest
                || newest - first > reader.shape().event_capacity
                || newest - first > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
                || reader.event_newest()?.checked_add(1) != Some(newest)
            {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            Ok((first, newest, prefix))
        })
    }
    fn freeze(&mut self) -> crate::Result<()> {
        let end = self.freeze_original_end()?;
        let input = self.inventory.owner().input();
        if let Some(plan) = self.fixed {
            if plan.first != input.losses.event_start || end > plan.end {
                return Err(invalid().into());
            }
            if self.source.0.is_some() {
                self.source.checked_read(input, |reader| {
                    let newest = reader.event_newest()?.checked_add(1).ok_or_else(invalid)?;
                    reader.seek_event(input.binding.event_first)?;
                    let first = match reader.read_event()? {
                        FoundEvent::Missed { first, end }
                            if first == input.binding.event_first && end > first =>
                        {
                            end
                        }
                        FoundEvent::Event { number, .. } if number == input.binding.event_first => {
                            number
                        }
                        FoundEvent::Nothing if newest == input.binding.event_first => newest,
                        _ => return Err(invalid()),
                    };
                    if plan.source_end > newest
                        || (plan.source_first > plan.first && plan.source_first > first)
                    {
                        return Err(invalid());
                    }
                    Ok(())
                })?;
            }
            self.source_first = plan.source_first;
            self.source_end = plan.source_end;
            self.prefix_end = plan.source_first;
            self.next = plan.first;
            self.selected = Some(plan.first..plan.end);
            return Ok(());
        }
        let (source_first, source_end, prefix_end) = if self.source.0.is_some() {
            self.freeze_source()?
        } else {
            (end, end, end)
        };
        let input = self.inventory.owner().input();
        // A surviving/indexed finite prefix proves preservation, never final lifecycle.
        // With no such prefix ordinary source selection needs a timing-domain proof first.
        if end == input.losses.event_start {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "source-only endpoint needs selected timing-domain proof",
            )
            .into());
        }
        self.source_end = source_end;
        self.source_first = source_first;
        self.prefix_end = prefix_end;
        self.next = input.losses.event_start;
        self.selected = Some(self.next..end);
        Ok(())
    }
    fn load(&mut self) -> crate::Result<bool> {
        if let Some(frame) = &self.input {
            if self.row < frame.height() {
                return Ok(true);
            }
            self.input = None;
            self.row = 0;
            self.ordinal += 1;
        }
        if self.ordinal == self.inventory.batches().len() {
            return Ok(true);
        }
        let fact = self.inventory.batches()[self.ordinal];
        if let BatchFact::Complete {
            first,
            end,
            rows,
            file: expected,
        } = fact
        {
            self.ledger.update(|work| {
                work.inventory.raw_admissions += 1;
                work.proof_operations += 1;
            });
            let file = self
                .inventory
                .reopen_batch(u32::try_from(self.ordinal).map_err(|_| invalid())?)?;
            self.ledger.update(|work| {
                work.inventory.raw_file_clones += 1;
                // Charge the admitted decoder's expanded ceiling before fallible decoding.
                work.decode_admission_bytes +=
                    collector::u64_from_usize(crate::event_observation::BATCH_BYTES_MAX * 2);
            });
            let witness = file.try_clone()?;
            let input = self.inventory.owner().input();
            let mut decoded = 0;
            let result = read::read_batch_file_counted(
                file,
                &Batch { first, end, rows },
                input.captured,
                true,
                &self.budget,
                &mut decoded,
            );
            self.ledger
                .update(|work| work.inventory.decoded_bytes += decoded);
            let frame = result?;
            if super::FileIdentity::read(&witness)? != expected {
                return Err(invalid().into());
            }
            self.input = Some(frame);
            Ok(true)
        } else {
            self.ordinal += 1;
            Ok(false)
        }
    }
    fn merge_one_source(&mut self, end: u64) -> crate::Result<(Option<batch::Row>, u64, bool)> {
        // With no source left, a row the originals do not hold is one the writer counted as lost.
        if self.source.0.is_none() {
            return Ok((None, end, true));
        }
        let input = self.inventory.owner().input();
        let position = self.next;
        let mut identity = IdentityObservation::EMPTY;
        let mut source_row = None;
        let mut absent_end = end;
        let mut exact = false;
        if position >= input.binding.event_first && position < self.prefix_end {
            return Ok((None, end.min(self.prefix_end), true));
        }
        if position < self.source_end && position >= self.source_first {
            let captured = input.captured;
            let found = self.source.checked_read(input, |reader| {
                reader.seek_event(position)?;
                if captured {
                    reader.read_event_identity(&mut identity)
                } else {
                    reader.read_event()
                }
            })?;
            match found {
                FoundEvent::Event {
                    number,
                    tick,
                    event,
                } if number == position => {
                    source_row = Some(self.source.checked_read(input, |reader| {
                        batch::copy_row(
                            reader,
                            &crate::NumberedEvent {
                                number,
                                tick,
                                event: &event,
                            },
                            captured.then_some(&identity),
                        )
                        .map_err(|error| io::Error::other(error.to_string()))
                    })?);
                }
                FoundEvent::Missed { first, end: missed }
                    if first == position && missed > first =>
                {
                    absent_end = missed.min(self.source_end).min(end);
                    exact = true;
                }
                _ => return Err(io::Error::from(io::ErrorKind::WouldBlock).into()),
            }
        }
        Ok((source_row, absent_end, exact))
    }
    fn merge_one_row(
        &self,
        original: Option<u64>,
        source_row: Option<batch::Row>,
    ) -> crate::Result<Option<batch::Row>> {
        let input = self.inventory.owner().input();
        let position = self.next;
        let row = if original == Some(position) {
            let frame = self
                .input
                .as_ref()
                .ok_or_else(invalid)?
                .slice(i64::try_from(self.row).map_err(|_| invalid())?, 1);
            if let Some(source) = &source_row {
                self.ledger.update(|work| work.proof_operations += 1);
                let source = batch::frame(std::slice::from_ref(source), input.captured)?;
                // Association belongs to persisted provenance; source None makes no contrary claim.
                let common: Vec<_> = frame
                    .get_column_names()
                    .into_iter()
                    .filter(|name| {
                        !matches!(
                            name.as_str(),
                            "publication" | "event_observation_availability"
                        )
                    })
                    .map(PlSmallStr::as_str)
                    .collect();
                if !frame
                    .select(common.clone())?
                    .equals_missing(&source.select(common)?)
                {
                    return Err(crate::Error::Bound("conflicting original/source event"));
                }
            }
            Some(batch::owned_row(&frame, 0, input.captured)?)
        } else {
            source_row
        };
        Ok(row)
    }
    fn merge_one_output(&mut self, frame: batch::Row) -> crate::Result<bool> {
        if self.output.is_none() {
            let reservation = self
                .budget
                .reserve(batch::WORKING_BYTES)
                .ok_or_else(memory)?;
            self.output = Some(MergeBatch {
                rows: Vec::with_capacity(crate::event_observation::BATCH_ROWS_MAX),
                bytes: batch::ROWS_BYTES_MAX,
                _reservation: reservation,
            });
        }
        let output = self.output.as_mut().ok_or_else(invalid)?;
        let full = output.rows.len() == self.output_rows_max
            || output.bytes + batch::PROGRAM_BYTES_MAX > crate::event_observation::BATCH_BYTES_MAX;
        if full {
            self.ready = true;
            return Ok(false);
        }
        output.bytes +=
            frame.name.as_ref().map_or(0, Vec::len) + frame.app.as_ref().map_or(0, Vec::len);
        output.rows.push(frame);
        if output.rows.len() == self.output_rows_max {
            self.ready = true;
        }
        Ok(true)
    }
    fn merge_one_compare_replay(
        &self,
        observed: &batch::Row,
        retained: &batch::Row,
        persisted: bool,
    ) -> crate::Result<()> {
        let captured = self.inventory.owner().input().captured;
        let mut observed = batch::frame(std::slice::from_ref(observed), captured)?;
        let mut expected = batch::frame(std::slice::from_ref(retained), captured)?;
        if !persisted {
            for name in ["publication", "event_observation_availability"] {
                observed.drop_in_place(name)?;
                expected.drop_in_place(name)?;
            }
        }
        if !observed.equals_missing(&expected) {
            return Err(crate::Error::Bound("conflicting candidate event"));
        }
        Ok(())
    }
    fn merge_one(&mut self, end: u64, replay: Option<&batch::Row>) -> crate::Result<()> {
        let original = self
            .input
            .as_ref()
            .map(|frame| {
                frame
                    .column("event")?
                    .u64()?
                    .get(self.row)
                    .ok_or_else(|| crate::Error::Io(invalid()))
            })
            .transpose()?;
        if original.is_some_and(|number| number < self.next) {
            return Err(invalid().into());
        }
        let position = self.next;
        let (source_row, absent_end, exact) = self.merge_one_source(end)?;
        let inherited_end = self.inventory.owner().input().losses.event_inherited_end;
        let mut row = self.merge_one_row(original, source_row)?;
        if let Some(retained) = replay {
            if position == retained.event.number {
                if let Some(observed) = &row {
                    self.merge_one_compare_replay(observed, retained, original == Some(position))?;
                }
                if original == Some(position) {
                    self.row += 1;
                }
                self.next += 1;
                return Ok(());
            }
            if row.is_some() {
                return Err(crate::Error::Bound(
                    "candidate omitted surviving prefix row",
                ));
            }
            row = None;
        }
        if let Some(frame) = row {
            if !self.merge_one_output(frame)? {
                return Ok(());
            }
            if original == Some(position) {
                self.row += 1;
            }
            self.next += 1;
        } else {
            let next = original
                .unwrap_or(end)
                .min(absent_end)
                .min(replay.map_or(end, |row| row.event.number));
            if next <= position {
                return Err(invalid().into());
            }
            let count = next - position;
            if exact {
                let old = next
                    .min(inherited_end)
                    .saturating_sub(position.min(inherited_end));
                self.losses[0] += old;
                self.losses[1] += count - old;
            } else {
                self.unresolved += count;
            }
            self.next = next;
        }
        Ok(())
    }
}
impl Drop for RowMerge<'_> {
    fn drop(&mut self) {
        self.record();
    }
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
fn memory() -> io::Error {
    io::Error::from(io::ErrorKind::OutOfMemory)
}
