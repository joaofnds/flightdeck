//! Closed normal, exact loss-only and fixed row-bearing publication state.
use super::{
    AttemptInput, AttemptOperation, Identity, LossTransfer, Operation, Phase, RunIdentity, Slot,
    SlotKind, SourceTiming, codec, get_identity, get32, get64, invalid, io, put32, put64,
};
use crate::canonical::{CanonicalLoss, NodeLocator};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Structure {
    pub kind: u32,
    pub stage: u32,
    pub serial: u64,
    pub parent: NodeLocator,
    pub old: NodeLocator,
    pub replacement: NodeLocator,
    pub insertion: NodeLocator,
    pub device: u64,
    pub inode: u64,
    pub copied: u32,
    pub children: u32,
    pub cleanup: u32,
}
impl Structure {
    pub fn validate(&self) -> io::Result<()> {
        let parent = &self.parent;
        let old = &self.old;
        let new = &self.replacement;
        let key = &self.insertion;
        if !matches!(self.kind, 1 | 2)
            || !(1..=5).contains(&self.stage)
            || self.serial == 0
            || [old.namespace, new.namespace, key.namespace]
                .iter()
                .any(|namespace| *namespace != parent.namespace)
            || new.nibbles == 0
            || new.nibbles <= parent.nibbles
            || key.nibbles != u8::try_from(key.namespace.width() * 2).map_err(|_| invalid())?
            || !prefix_of(parent, new)
            || !prefix_of(new, key)
            || self.copied > 128
            || self.children > 65535
            || self.cleanup > 128
            || (self.stage == 1 && self.children != 0)
            || (self.stage >= 2 && self.inode != 0 && self.children.count_ones() < 2)
        {
            return Err(invalid());
        }

        if self.device == 0 && self.inode == 0 {
            if self.kind != 2 || old != parent || self.copied != 0 {
                return Err(invalid());
            }
        } else if self.device == 0
            || self.inode == 0
            || old.nibbles <= parent.nibbles
            || !prefix_of(parent, old)
        {
            return Err(invalid());
        }

        if self.inode != 0 {
            match self.kind {
                1 if self.copied == 128 && prefix_of(old, new) => {}
                2 if self.copied == 0 && prefix_of(new, old) && old.nibbles > new.nibbles => {}
                _ => return Err(invalid()),
            }
        }
        Ok(())
    }
    fn encode(&self, bytes: &mut [u8]) -> io::Result<()> {
        self.validate()?;
        bytes.fill(0);
        for (offset, value) in [
            (0, self.kind),
            (4, self.stage),
            (288, self.copied),
            (292, self.children),
            (296, self.cleanup),
        ] {
            put32(bytes, offset, value);
        }
        put64(bytes, 8, self.serial);
        for (offset, locator) in [
            (16, &self.parent),
            (80, &self.old),
            (144, &self.replacement),
            (208, &self.insertion),
        ] {
            bytes[offset..offset + 64].copy_from_slice(&locator.encode());
        }
        put64(bytes, 272, self.device);
        put64(bytes, 280, self.inode);
        Ok(())
    }
    fn decode(bytes: &[u8]) -> io::Result<Self> {
        let record = Self {
            kind: get32(bytes, 0)?,
            stage: get32(bytes, 4)?,
            serial: get64(bytes, 8)?,
            parent: NodeLocator::decode(&bytes[16..80])?,
            old: NodeLocator::decode(&bytes[80..144])?,
            replacement: NodeLocator::decode(&bytes[144..208])?,
            insertion: NodeLocator::decode(&bytes[208..272])?,
            device: get64(bytes, 272)?,
            inode: get64(bytes, 280)?,
            copied: get32(bytes, 288)?,
            children: get32(bytes, 292)?,
            cleanup: get32(bytes, 296)?,
        };
        let mut expected = [0; 512];
        record.encode(&mut expected)?;
        if expected != bytes {
            return Err(invalid());
        }

        Ok(record)
    }
}
pub(crate) fn prefix_of(prefix: &NodeLocator, key: &NodeLocator) -> bool {
    prefix.namespace == key.namespace
        && prefix.nibbles <= key.nibbles
        && (0..usize::from(prefix.nibbles))
            .all(|index| nibble(&prefix.prefix, index) == nibble(&key.prefix, index))
}
pub(crate) fn nibble(bytes: &[u8], index: usize) -> u8 {
    if index.is_multiple_of(2) {
        bytes[index / 2] >> 4
    } else {
        bytes[index / 2] & 15
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NormalState {
    pub transaction: Identity,
    pub input: AttemptInput,
    pub phase: Phase,
    pub original_end: u64,
    pub origin: u64,
    pub loss: CanonicalLoss,
    pub sequence: u64,
    pub next: u64,
    pub structure: Option<Structure>,
    pub salvage: Option<super::salvage::LossPlan>,
    pub cleanup: super::salvage::Cleanup,
    pub census: super::census_binding::CensusBinding,
    pub row_plan: Option<super::salvage::RowPlan>,
}
impl NormalState {
    pub fn registered(transaction: Identity, input: AttemptInput) -> Self {
        let loss = CanonicalLoss {
            publication_end: input.losses.publication_inherited_end,
            event_end: input.losses.event_inherited_end,
            publications_new: 0,
            events_new: 0,
            unknown_flags: u32::from(input.losses.unknown.is_some()),
            inherited: input.losses.clone(),
        };
        let row_plan = input.finite_plan();
        Self {
            transaction,
            input,
            phase: Phase::Registered,
            original_end: 0,
            origin: 0,
            loss,
            sequence: 0,
            next: 0,
            structure: None,
            salvage: None,
            row_plan,
            cleanup: super::salvage::Cleanup::default(),
            census: super::census_binding::CensusBinding::default(),
        }
    }
    pub fn encode(&self, bytes: &mut [u8; 4096]) -> io::Result<()> {
        self.input.encode_registered(self.transaction, bytes)?;
        self.census
            .encode((&mut bytes[1400..1496]).try_into().map_err(|_| invalid())?)?;
        if self.phase == Phase::Registered {
            let mut expected = Self::registered(self.transaction, self.input.clone());
            expected.census = self.census;
            if self.census.kind > 1 || *self != expected {
                return Err(invalid());
            }

            return Ok(());
        }

        if let Some(plan) = &self.row_plan {
            return self.encode_rows(bytes, plan);
        }
        if let Some(plan) = &self.salvage {
            return self.encode_loss(bytes, plan);
        }
        if self.census.kind != 0 || self.cleanup != super::salvage::Cleanup::default() {
            return Err(invalid());
        }
        let phase = match self.phase {
            Phase::Ready => 2,
            Phase::Publishing => 3,
            Phase::Published => 4,
            Phase::Cleanup => 5,
            _ => return Err(invalid()),
        };
        if self.loss.inherited != self.input.losses || self.next > 62 {
            return Err(invalid());
        }

        self.encode_normal_origin(bytes)?;
        if self.phase == Phase::Ready
            && (self.sequence != 0 || self.next != 0 || self.structure.is_some())
        {
            return Err(invalid());
        }

        if self.phase != Phase::Ready && self.sequence == 0 {
            return Err(invalid());
        }

        put32(bytes, 48, phase);
        put64(bytes, 144, self.original_end);
        put64(bytes, 152, self.loss.event_end);
        put64(bytes, 192, self.sequence);
        put64(bytes, 200, self.next);
        put64(bytes, 1344, self.origin);
        self.loss.encode(&mut bytes[1088..1344]);
        if let Some(structure) = &self.structure {
            if self.phase != Phase::Publishing {
                return Err(invalid());
            }

            structure.encode(&mut bytes[512..1024])?;
        }
        Ok(())
    }
    fn encode_normal_origin(&self, bytes: &mut [u8; 4096]) -> io::Result<()> {
        match self.input.operation {
            AttemptOperation::Ordinary {
                attempted_origin, ..
            } => {
                if self.origin < attempted_origin
                    || self.origin >= self.original_end
                    || self.loss.publication_end != self.original_end
                {
                    return Err(invalid());
                }

                put32(bytes, 140, 4);
            }
            AttemptOperation::Finite { .. } => return Err(invalid()),
            AttemptOperation::Terminal { event_end } => {
                if self.origin != 0
                    || self.original_end != 0
                    || self.loss.event_end != event_end
                    || self.loss.publication_end != self.input.losses.publication_inherited_end
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
    fn encode_rows(
        &self,
        bytes: &mut [u8; 4096],
        plan: &super::salvage::RowPlan,
    ) -> io::Result<()> {
        if plan.first != self.input.losses.event_start
            || plan.layout != self.input.source.layout
            || self.salvage.is_some()
            || self.origin != 0
            || !matches!(
                self.input.operation,
                AttemptOperation::Ordinary { .. } | AttemptOperation::Finite { .. }
            )
            || (self.input.is_finite() != (plan.endpoint == 3))
            || (self.input.is_finite() && plan.source_end != plan.end)
        {
            return Err(invalid());
        }
        let phase = match self.phase {
            Phase::SalvagePlanned => 6,
            Phase::Publishing => 3,
            Phase::Published => 4,
            Phase::SalvageCommitted => 7,
            Phase::UnresolvedExtent => 8,
            _ => return Err(invalid()),
        };
        if self.census.kind != 2 {
            return Err(invalid());
        }
        self.validate_row_state(plan, phase)?;
        put32(bytes, 48, phase);
        put32(bytes, 140, plan.endpoint);
        put64(bytes, 144, self.original_end);
        put64(bytes, 152, plan.end);
        put32(
            bytes,
            188,
            u32::from(
                (self.original_end == 0 && !self.input.is_finite())
                    || self.phase == Phase::UnresolvedExtent,
            ),
        );
        put64(bytes, 192, self.sequence);
        put64(bytes, 200, self.next);
        self.loss.encode(&mut bytes[1088..1344]);
        plan.encode((&mut bytes[256..512]).try_into().map_err(|_| invalid())?)?;
        self.cleanup
            .encode((&mut bytes[1024..1088]).try_into().map_err(|_| invalid())?)?;
        if let Some(structure) = &self.structure {
            if phase != 3 {
                return Err(invalid());
            }
            structure.encode(&mut bytes[512..1024])?;
        }
        Ok(())
    }
    fn validate_row_state(&self, plan: &super::salvage::RowPlan, phase: u32) -> io::Result<()> {
        let prepublication = self.input.is_finite() && phase == 8 && self.sequence == 0;
        if prepublication
            && (self.next != 0
                || self.structure.is_some()
                || plan.committed != 0
                || self.cleanup != super::salvage::Cleanup::default())
        {
            return Err(invalid());
        }
        if plan.rows == [0, 0] {
            let mut expected = Self::registered(self.transaction, self.input.clone());
            expected.phase = self.phase;
            expected.row_plan = Some(*plan);
            expected.census = self.census;
            if *self != expected || (phase != 6 && !prepublication) {
                return Err(invalid());
            }
        } else {
            self.loss.totals()?;
            let accounted = plan.rows[0]
                .checked_add(plan.rows[1])
                .and_then(|rows| rows.checked_add(self.loss.inherited.events))
                .and_then(|total| total.checked_add(self.loss.events_new))
                .ok_or_else(invalid)?;
            if self.loss.inherited != self.input.losses
                || self.loss.event_end != plan.end
                || accounted != plan.end - plan.first
                || self.next > 2
                || ((phase == 3 || phase == 4 || phase == 7) && self.sequence == 0)
                || (self.cleanup != super::salvage::Cleanup::default() && !matches!(phase, 7 | 8))
            {
                return Err(invalid());
            }
        }
        let used = 1 + u64::from(plan.rows[1] != 0);
        if (phase == 6 && (self.sequence != 0 || self.next != 0 || plan.committed != 0))
            || (phase == 8 && self.sequence == 0 && !prepublication)
            || (matches!(phase, 4 | 7 | 8)
                && !prepublication
                && (self.next != used || plan.committed != (1 << used) - 1))
            || (self.cleanup.region > 1 && self.cleanup.region != 8)
            || (self.cleanup.region == 8
                && (phase != 7
                    || (self.original_end == 0 && !self.input.is_finite())
                    || self.cleanup
                        != super::salvage::Cleanup {
                            region: 8,
                            ..Default::default()
                        }))
            || self.cleanup.table != 0
            || self.cleanup.directory > 1
            || (self.cleanup.directory != 0 && self.cleanup.ordinal != 4096)
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn encode_loss(
        &self,
        bytes: &mut [u8; 4096],
        plan: &super::salvage::LossPlan,
    ) -> io::Result<()> {
        if self.census.kind != 2 {
            return Err(invalid());
        }
        self.validate_loss_state(plan)?;
        let phase = match self.phase {
            Phase::SalvagePlanned
                if self.sequence == 0 && self.next == 0 && self.structure.is_none() =>
            {
                6
            }
            Phase::Publishing if self.sequence > 0 => 3,
            Phase::Published if self.sequence > 0 && self.structure.is_none() => 4,
            Phase::SalvageCommitted if self.sequence > 0 && self.structure.is_none() => 7,
            Phase::UnresolvedExtent
                if self.input.is_finite()
                    && self.sequence == 0
                    && self.next == 0
                    && self.structure.is_none() =>
            {
                8
            }
            _ => return Err(invalid()),
        };
        put32(bytes, 48, phase);
        put32(bytes, 140, plan.endpoint);
        put64(bytes, 144, self.original_end);
        put64(bytes, 152, self.loss.event_end);
        put32(bytes, 184, u32::from(plan.terminal));
        put32(bytes, 188, u32::from(phase == 8));
        put64(bytes, 192, self.sequence);
        put64(bytes, 200, self.next);
        plan.encode_context(
            (&mut bytes[256..512]).try_into().map_err(|_| invalid())?,
            self.input.is_finite(),
        )?;
        self.cleanup
            .encode((&mut bytes[1024..1088]).try_into().map_err(|_| invalid())?)?;
        self.loss.encode(&mut bytes[1088..1344]);
        if let Some(structure) = &self.structure {
            if self.phase != Phase::Publishing {
                return Err(invalid());
            }
            structure.encode(&mut bytes[512..1024])?;
        }
        Ok(())
    }
    fn validate_loss_state(&self, plan: &super::salvage::LossPlan) -> io::Result<()> {
        if !matches!(plan.endpoint, 1 | 3) {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        let metadata = self.loss_metadata()?;
        metadata.validate()?;
        let links = u64::try_from(metadata.loss_routes()?.iter().flatten().count())
            .map_err(|_| invalid())?;
        if self.next > links
            || (matches!(self.phase, Phase::Published | Phase::SalvageCommitted)
                && self.next != links)
        {
            return Err(invalid());
        }
        if self.loss.inherited != self.input.losses
            || self.origin != 0
            || self.next > 2
            || plan.first != self.input.losses.event_start
            || plan.end != self.loss.event_end
            || plan.layout != self.input.source.layout
            || plan.terminal != matches!(self.input.operation, AttemptOperation::Terminal { .. })
            || (self.cleanup.region > 3 && self.cleanup.region != 8)
            || (self.cleanup.region != 0 && self.phase != Phase::SalvageCommitted)
            || (self.cleanup.region == 1
                && (self.cleanup.table != 0 || self.cleanup.directory != 0))
            || (self.cleanup.region == 2
                && (self.cleanup.ordinal != 0
                    || self.cleanup.table != 0
                    || self.cleanup.directory != 0))
            || (self.cleanup.region == 3
                && (self.cleanup.ordinal != 0
                    || self.cleanup.table != 0
                    || self.cleanup.directory > 1))
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn loss_metadata(&self) -> io::Result<crate::canonical::CanonicalMetadata> {
        use crate::canonical::{CanonicalMetadata, EndpointEvidence, Lifecycle, RawBase, RawChild};
        let plan = self.salvage.as_ref().ok_or_else(invalid)?;
        let (base, window, attempted_origin) = match self.input.operation {
            AttemptOperation::Ordinary {
                window,
                attempted_origin,
            } => (RawBase::Data, window, attempted_origin),
            AttemptOperation::Terminal { event_end }
                if event_end == self.loss.event_end
                    && self.loss.publication_end == self.input.losses.publication_inherited_end
                    && self.original_end == 0 =>
            {
                (RawBase::Recovery, 0, 0)
            }
            AttemptOperation::Finite { order_floor, .. } => (RawBase::Recovery, order_floor, 0),
            AttemptOperation::Terminal { .. } => return Err(invalid()),
        };
        Ok(CanonicalMetadata {
            run: self.input.run,
            transaction: self.transaction,
            kind: if self.input.is_finite() {
                super::CanonicalKind::FiniteEventLoss
            } else {
                super::CanonicalKind::LossOnly
            },
            base,
            child: RawChild::Loss,
            window,
            attempted_origin,
            canonical_origin: 0,
            original_end: self.original_end,
            binding: self.input.binding,
            source_layout: self.input.source.layout,
            source_origin: self.input.source.origin_continuous_ns,
            evidence: match plan.endpoint {
                1 => EndpointEvidence::OrdinaryIndex,
                3 => EndpointEvidence::FixedSource,
                4 => EndpointEvidence::ReadyManifest,
                _ => return Err(invalid()),
            },
            lifecycle: if plan.terminal {
                Lifecycle::AcquiredCompletion
            } else {
                Lifecycle::Active
            },
            captured: self.input.captured,
            trailing_unknown: false,
            loss: self.loss.clone(),
            body: crate::canonical::metadata::Body::LossOnly,
        })
    }
    fn decode_input(
        bytes: &[u8],
        recording: Identity,
        operation: Operation,
    ) -> io::Result<AttemptInput> {
        Ok(AttemptInput {
            run: RunIdentity {
                recording,
                run: get_identity(bytes, 64)?,
                boot: get_identity(bytes, 80)?,
            },
            captured: get32(bytes, 124)? == 1,
            source: SourceTiming::decode(bytes, 224, 232, 240)?,
            binding: super::source_binding::decode(&bytes[1352..1400])?,
            losses: LossTransfer::decode(&bytes[1088..1344])?,
            operation: match operation {
                Operation::Ordinary => AttemptOperation::Ordinary {
                    window: get64(bytes, 96)?,
                    attempted_origin: get64(bytes, 104)?,
                },
                Operation::Terminal => AttemptOperation::Terminal {
                    event_end: get64(bytes, 152)?,
                },
                Operation::ExceptionalFragment => AttemptOperation::Finite {
                    event_end: get64(bytes, 152)?,
                    order_floor: get64(bytes, 96)?,
                    source_first: get64(bytes, 280)?,
                },
            },
        })
    }
    fn decode_registered_census(
        bytes: &[u8],
        recording: Identity,
        transaction: Identity,
    ) -> io::Result<Self> {
        let census = super::census_binding::CensusBinding::decode(
            bytes[1400..1496].try_into().map_err(|_| invalid())?,
        )?;
        if census.kind > 1 {
            return Err(invalid());
        }
        let mut registered = <[u8; 4096]>::try_from(bytes).map_err(|_| invalid())?;
        registered[1400..1496].fill(0);
        let (_, input) = AttemptInput::decode_construction(&registered, recording, true)?;
        let mut state = Self::registered(transaction, input);
        state.census = census;
        Ok(state)
    }
    fn decode_plans(
        bytes: &[u8],
        operation: Operation,
    ) -> io::Result<(
        Option<super::salvage::RowPlan>,
        Option<super::salvage::LossPlan>,
    )> {
        let row_plan = if matches!(get32(bytes, 260)?, 1 | 2) {
            Some(super::salvage::RowPlan::decode(
                bytes[256..512].try_into().map_err(|_| invalid())?,
            )?)
        } else {
            None
        };
        let salvage = if matches!(get32(bytes, 260)?, 1 | 2)
            || bytes[256..512].iter().all(|byte| *byte == 0)
        {
            None
        } else {
            Some(super::salvage::LossPlan::decode_context(
                bytes[256..512].try_into().map_err(|_| invalid())?,
                operation == Operation::ExceptionalFragment,
            )?)
        };
        Ok((row_plan, salvage))
    }
    pub fn decode(bytes: &[u8], recording: Identity) -> io::Result<Self> {
        let Slot::Occupied {
            phase,
            transaction,
            operation,
            ..
        } = codec::decode_slot(bytes, recording, SlotKind::Foreground)?
        else {
            return Err(invalid());
        };
        if phase == Phase::Registered {
            return Self::decode_registered_census(bytes, recording, transaction);
        }
        if !matches!(
            phase,
            Phase::Ready
                | Phase::Publishing
                | Phase::Published
                | Phase::Cleanup
                | Phase::SalvagePlanned
                | Phase::SalvageCommitted
                | Phase::UnresolvedExtent
        ) {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        let input = Self::decode_input(bytes, recording, operation)?;
        let (row_plan, salvage) = Self::decode_plans(bytes, operation)?;
        let state = Self {
            transaction,
            input,
            phase,
            census: super::census_binding::CensusBinding::decode(
                bytes[1400..1496].try_into().map_err(|_| invalid())?,
            )?,
            original_end: get64(bytes, 144)?,
            origin: get64(bytes, 1344)?,
            loss: CanonicalLoss::decode(&bytes[1088..1344])?,
            sequence: get64(bytes, 192)?,
            next: get64(bytes, 200)?,
            row_plan,
            salvage,
            cleanup: super::salvage::Cleanup::decode(
                bytes[1024..1088].try_into().map_err(|_| invalid())?,
            )?,
            structure: if bytes[512..1024].iter().all(|byte| *byte == 0) {
                None
            } else {
                Some(Structure::decode(&bytes[512..1024])?)
            },
        };
        let mut expected = [0; 4096];
        state.encode(&mut expected)?;
        if expected != bytes {
            return Err(invalid());
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal_literal(phase: u8) -> [u8; 4096] {
        let mut bytes = [0; 4096];
        bytes[..8].copy_from_slice(b"FDTXN002");
        bytes[1088..1096].copy_from_slice(b"FDLoss02");
        for (offset, value) in [
            (11, 1),
            (15, 1),
            (23, 1),
            (31, 2),
            (39, 7),
            (47, 8),
            (51, phase),
            (55, 1),
            (58, 8),
            (71, 3),
            (79, 4),
            (87, 5),
            (95, 6),
            (111, 1),
            (119, 1),
            (123, 1),
            (131, 1),
            (134, 16),
            (138, 15),
            (139, 255),
            (143, 4),
            (151, 3),
            (159, 1),
            (215, 7),
            (223, 8),
            (231, 5),
            (235, 1),
            (1099, 1),
            (1143, 1),
            (1151, 1),
            (1159, 1),
            (1167, 1),
            (1191, 3),
            (1199, 1),
            (1207, 1),
            (1351, 2),
        ] {
            bytes[offset] = value;
        }
        if phase != 2 {
            bytes[199] = 9;
            bytes[207] = 1;
        }
        if phase == 3 {
            for (offset, value) in [
                (515, 2),
                (519, 2),
                (527, 9),
                (528, 1),
                (592, 1),
                (656, 1),
                (657, 1),
                (660, 0x10),
                (720, 1),
                (721, 80),
                (724, 0x10),
                (807, 2),
            ] {
                bytes[offset] = value;
            }
        }
        normal_literal_binding(&mut bytes);
        bytes
    }

    fn normal_literal_binding(bytes: &mut [u8; 4096]) {
        for (offset, value) in [
            (1355, 1),
            (1367, 17),
            (1375, 18),
            (1383, 1),
            (1391, 1),
            (1399, 1),
        ] {
            bytes[offset] = value;
        }
    }
    fn assert_normal_literal(phase: Phase, wire_phase: u8) {
        let literal = normal_literal(wire_phase);
        let state = NormalState::decode(&literal, Identity([1, 2])).unwrap();
        assert_eq!(state.phase, phase);
        assert_eq!(state.origin, 2);
        assert_eq!(state.original_end, 3);
        assert_eq!(state.loss.publications_new, 1);
        assert_eq!(state.structure.is_some(), phase == Phase::Publishing);
        if let Some(structure) = &state.structure {
            assert_eq!(structure.stage, 2);
            assert_eq!(structure.serial, 9);
            assert_eq!(structure.insertion.nibbles, 80);
            assert_eq!(structure.replacement.prefix[0], 0x10);
        }

        let mut encoded = [0; 4096];
        state.encode(&mut encoded).unwrap();
        assert_eq!(encoded, literal);
    }

    #[test]
    fn ready_matches_the_literal_wire_contract() {
        // Goal: Ready bytes pin the canonical origin independently of paired codecs.
        // Method: decode a literal payload and compare every re-encoded byte.
        assert_normal_literal(Phase::Ready, 2);
    }

    #[test]
    fn publishing_matches_the_literal_structural_descriptor() {
        // Goal: an occupied descriptor retains its fixed offsets and zero padding.
        // Method: admit a literal absent-child wrapper and compare every encoded byte.
        assert_normal_literal(Phase::Publishing, 3);
    }

    #[test]
    fn published_matches_the_literal_wire_contract() {
        // Goal: Published retains the exact source and canonical origin encoding.
        // Method: compare an independently assembled payload with its re-encoding.
        assert_normal_literal(Phase::Published, 4);
    }

    #[test]
    fn cleanup_matches_the_literal_wire_contract() {
        // Goal: Cleanup cannot reinterpret the retained normal-publication fields.
        // Method: compare a literal payload with its decoded and re-encoded value.
        assert_normal_literal(Phase::Cleanup, 5);
    }

    #[test]
    fn normal_payload_refuses_nonzero_reserved_bytes() {
        // Goal: origin and structural padding remain closed persistent grammar.
        // Method: alter each reserved byte independently in a valid occupied literal.
        let literal = normal_literal(3);
        for offset in (812..1024).chain(1356..1360).chain(1400..2112) {
            let mut altered = literal;
            altered[offset] = 1;
            assert_eq!(
                NormalState::decode(&altered, Identity([1, 2]))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData,
                "reserved byte {offset}"
            );
        }
    }

    #[test]
    fn typed_origin_presence_boundaries_fallback_and_layout_profile_are_closed() {
        // Goal: retained and live timing share the exact elapsed-window rule, including zero.
        // Method: exercise exact boundaries, absent times, underflow and incompatible profiles.
        let timing = SourceTiming {
            layout: 5,
            origin_continuous_ns: Some(0),
        };
        timing.validate(false).unwrap();
        assert_eq!(timing.window(999, Some(119_999_999_999)).unwrap(), 1);
        assert_eq!(timing.window(999, Some(120_000_000_000)).unwrap(), 2);
        assert_eq!(timing.window(120, None).unwrap(), 2);
        assert!(timing.validate(true).is_err());
        let shifted = SourceTiming {
            layout: 5,
            origin_continuous_ns: Some(60_000_000_000),
        };
        assert_eq!(shifted.window(999, Some(120_000_000_000)).unwrap(), 1);
        assert!(shifted.window(999, Some(59_999_999_999)).is_err());
        let missing = SourceTiming {
            layout: 5,
            origin_continuous_ns: None,
        };
        assert!(missing.validate(false).is_err());
        assert!(missing.window(120, None).is_err());
        let mut bytes = [0; 4096];
        timing.encode(&mut bytes, 224, 232, 240);
        assert_eq!(&bytes[224..232], &5u64.to_be_bytes());
        assert_eq!(&bytes[232..236], &1u32.to_be_bytes());
        assert_eq!(SourceTiming::decode(&bytes, 224, 232, 240).unwrap(), timing);
        bytes[235] = 2;
        assert!(SourceTiming::decode(&bytes, 224, 232, 240).is_err());
    }
    fn count_zero_slot_literal() -> [u8; 4096] {
        let mut literal = normal_literal(2);
        for (offset, value) in [
            (51, 6),
            (143, 1),
            (1207, 2),
            (1351, 0),
            (259, 1),
            (271, 1),
            (279, 1),
            (287, 1),
            (295, 1),
            (303, 5),
            (307, 2),
            (311, 1),
        ] {
            literal[offset] = value;
        }
        literal[1403] = 2;
        literal[1456..1464].copy_from_slice(&394_720u64.to_be_bytes());
        literal
    }
    fn row_plan_slot_literal() -> [u8; 4096] {
        let mut literal = normal_literal(2);
        for (offset, value) in [
            (48, 6u32),
            (140, 2),
            (188, 1),
            (256, 1),
            (260, 1),
            (304, 2),
            (308, 2),
        ] {
            literal[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
        }
        for (offset, value) in [
            (144, 0u64),
            (152, 3),
            (264, 1),
            (272, 3),
            (280, 1),
            (288, 1),
            (296, 5),
            (320, 2),
            (1184, 1),
            (1192, 3),
            (1200, 0),
            (1344, 0),
        ] {
            literal[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
        }
        literal[1403] = 2;
        literal[1456..1464].copy_from_slice(&394_720u64.to_be_bytes());
        literal
    }
    #[test]
    fn row_conservation_rejects_overflow_and_accepts_the_exact_u64_boundary() {
        // Goal: malformed retained row counts return an operating error instead of panicking.
        // Method: decode literal slots at overflow, exact conservation and adjacent mismatch.
        let recording = Identity([1, 2]);
        let mut literal = row_plan_slot_literal();
        assert!(NormalState::decode(&literal, recording).is_ok());
        for offset in [152, 272, 1192] {
            literal[offset..offset + 8].copy_from_slice(&u64::MAX.to_be_bytes());
        }
        for (lost, valid) in [
            (u64::MAX - 1, false),
            (u64::MAX - 3, true),
            (u64::MAX - 4, false),
        ] {
            literal[1208..1216].copy_from_slice(&lost.to_be_bytes());
            let result = NormalState::decode(&literal, recording);
            if valid {
                assert!(result.is_ok());
            } else {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
            }
        }
    }
    #[test]
    fn count_zero_slot_and_cleanup_match_independent_literal_offsets() {
        // Goal: pin executable count0 plan and cleanup bytes without changing normal literals.
        // Method: hand-author the new fields, compare encoding and reject contradictory records.
        let recording = Identity([1, 2]);
        let mut state = NormalState::decode(&normal_literal(2), recording).unwrap();
        state.phase = Phase::SalvagePlanned;
        state.census = super::super::census_binding::CensusBinding::decode(
            count_zero_slot_literal()[1400..1496].try_into().unwrap(),
        )
        .unwrap();
        state.origin = 0;
        state.loss.publications_new = 2;
        state.salvage = Some(super::super::salvage::LossPlan {
            first: 1,
            end: 1,
            snapshot_first: 1,
            snapshot_end: 1,
            layout: 5,
            endpoint: 1,
            terminal: false,
        });
        let literal = count_zero_slot_literal();
        let mut actual = [0; 4096];
        state.encode(&mut actual).unwrap();
        assert_eq!(actual, literal);
        assert_eq!(NormalState::decode(&literal, recording).unwrap(), state);
    }
    #[test]
    fn count_zero_cleanup_matches_literal_and_closed_reserved_fields() {
        // Goal: pin cleanup cursor bytes and reject unsupported count0 state extensions.
        // Method: extend the independently hand-authored plan literal with explicit fields.
        let recording = Identity([1, 2]);
        let mut literal = count_zero_slot_literal();
        let mut state = NormalState::decode(&literal, recording).unwrap();
        let mut actual = [0; 4096];
        state.phase = Phase::SalvageCommitted;
        state.sequence = 9;
        state.next = 1;
        state.cleanup = super::super::salvage::Cleanup {
            region: 1,
            ordinal: 128,
            table: 0,
            directory: 0,
        };
        literal[51] = 7;
        literal[199] = 9;
        literal[207] = 1;
        literal[1027] = 1;
        literal[1031] = 128;
        state.encode(&mut actual).unwrap();
        assert_eq!(actual, literal);
        for (offset, value) in [
            (263, 1),
            (315, 1),
            (344, 1),
            (348, 1),
            (1031, 129),
            (1040, 1),
        ] {
            let mut changed = literal;
            changed[offset] = value;
            // 129 is a legal cursor; every other alteration contradicts the typed count0 plan.
            if offset == 1031 {
                assert!(NormalState::decode(&changed, recording).is_ok());
            } else {
                assert!(
                    NormalState::decode(&changed, recording).is_err(),
                    "offset {offset}"
                );
            }
        }
    }
    #[test]
    fn census_binding_closes_registered_normal_and_retirement_phases() {
        // Goal: commitment consumes normal authority and only settled phase7 may retire.
        // Method: mutate literal payloads and round-trip the exact96-byte binding and region8.
        let recording = Identity([1, 2]);
        let input = NormalState::decode(&normal_literal(2), recording)
            .unwrap()
            .input;
        let mut selected = NormalState::registered(Identity([7, 8]), input);
        selected.census.kind = 1;
        selected.census.digest = [42; 32];
        let mut bytes = [0; 4096];
        selected.encode(&mut bytes).unwrap();
        assert_eq!(&bytes[1400..1408], &[0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(&bytes[1408..1440], &[42; 32]);
        assert_eq!(NormalState::decode(&bytes, recording).unwrap(), selected);
        assert!(AttemptInput::decode_registered(&bytes, recording).is_err());
        let complete = count_zero_slot_literal();
        let mut ordinary = normal_literal(2);
        ordinary[1400..1496].copy_from_slice(&complete[1400..1496]);
        assert!(NormalState::decode(&ordinary, recording).is_err());
        let mut state = NormalState::decode(&complete, recording).unwrap();
        state.phase = Phase::SalvageCommitted;
        state.sequence = 9;
        state.next = 1;
        state.cleanup.region = 8;
        state.encode(&mut bytes).unwrap();
        assert_eq!(
            &bytes[1024..1040],
            &[0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(NormalState::decode(&bytes, recording).unwrap(), state);
        for phase in [Phase::SalvagePlanned, Phase::UnresolvedExtent] {
            state.phase = phase;
            assert!(state.encode(&mut bytes).is_err());
        }
        for offset in [1404, 1407, 1496, 2111] {
            let mut bad = complete;
            bad[offset] = 1;
            assert!(NormalState::decode(&bad, recording).is_err());
        }
    }
}
