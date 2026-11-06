//! Registered-phase payloads. Decoding is structural; Store validates source and ownership.
use std::io;

use collector::recording::bootstrap::codec::{self, Operation, Phase, Slot, SlotKind};
use collector::ring::{Identity, RunIdentity};

pub(crate) mod census_binding;
pub(crate) mod normal;
pub(crate) mod salvage;
pub(crate) mod source_binding;

const _: () = {
    assert!(codec::SLOT_BYTES == 4096);
    assert!(codec::PAYLOAD_BYTES == 2048);
    assert!(crate::SCHEMA_VERSION == 1);
    assert!(crate::schema::IDENTITY_VERSION == 1);
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptInput {
    pub run: RunIdentity,
    pub operation: AttemptOperation,
    pub captured: bool,
    pub source: SourceTiming,
    pub binding: collector::ring::ObjectBinding,
    pub losses: LossTransfer,
}

/// Immutable timing evidence admitted from the same identified source generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceTiming {
    pub layout: u64,
    pub origin_continuous_ns: Option<u64>,
}
impl SourceTiming {
    #[must_use]
    pub fn from_reader(reader: &collector::ring::RingReader) -> Self {
        Self {
            layout: reader.source_layout_version(),
            origin_continuous_ns: reader.run_start_continuous_ns(),
        }
    }
    pub(crate) fn validate(&self, captured: bool) -> io::Result<()> {
        if self.layout != if captured { 6 } else { 5 } || self.origin_continuous_ns.is_none() {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }

        Ok(())
    }
    pub(crate) fn window(&self, run_tick: u64, continuous_ns: Option<u64>) -> io::Result<u64> {
        let origin = self.origin_continuous_ns.ok_or_else(invalid)?;
        elapsed_window(run_tick, continuous_ns, Some(origin))
    }
    pub(crate) fn encode(&self, bytes: &mut [u8], layout: usize, kind: usize, origin: usize) {
        put64(bytes, layout, self.layout);
        put32(bytes, kind, u32::from(self.origin_continuous_ns.is_some()));
        put64(bytes, origin, self.origin_continuous_ns.unwrap_or(0));
    }
    pub(crate) fn decode(
        bytes: &[u8],
        layout: usize,
        kind: usize,
        origin: usize,
    ) -> io::Result<Self> {
        let value = get64(bytes, origin)?;
        Ok(Self {
            layout: get64(bytes, layout)?,
            origin_continuous_ns: match get32(bytes, kind)? {
                0 if value == 0 => None,
                1 => Some(value),
                _ => return Err(invalid()),
            },
        })
    }
}
pub(crate) fn elapsed_window(
    run_tick: u64,
    continuous_ns: Option<u64>,
    origin: Option<u64>,
) -> io::Result<u64> {
    match (continuous_ns, origin) {
        (Some(time), Some(start)) => time
            .checked_sub(start)
            .map(|elapsed| elapsed / 60_000_000_000)
            .ok_or_else(invalid),
        _ => Ok(run_tick / 60),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttemptOperation {
    Ordinary {
        window: u64,
        attempted_origin: u64,
    },
    Terminal {
        event_end: u64,
    },
    Finite {
        event_end: u64,
        order_floor: u64,
        source_first: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LossTransfer {
    pub predecessor: Predecessor,
    pub publication_start: u64,
    pub publication_inherited_end: u64,
    pub event_start: u64,
    pub event_inherited_end: u64,
    pub publications: u64,
    pub events: u64,
    pub unknown: Option<UnknownProvenance>,
    pub prior_window: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Predecessor {
    Genesis,
    Canonical {
        transaction: Identity,
        kind: CanonicalKind,
        origin: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum CanonicalKind {
    Ordinary = 1,
    Terminal = 2,
    Fragment = 3,
    LossOnly = 4,
    FiniteEvents = 5,
    FiniteEventLoss = 6,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownProvenance {
    pub reason: UnknownReason,
    pub run: Identity,
    pub boot: Identity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum UnknownReason {
    Predecessor = 1,
    Current = 2,
    Both = 3,
}

impl AttemptInput {
    pub(crate) fn is_finite(&self) -> bool {
        matches!(self.operation, AttemptOperation::Finite { .. })
    }
    pub(crate) fn finite_plan(&self) -> Option<salvage::RowPlan> {
        if let AttemptOperation::Finite {
            event_end,
            source_first,
            ..
        } = self.operation
        {
            Some(salvage::RowPlan {
                count: 2,
                first: self.losses.event_start,
                end: event_end,
                source_first,
                source_end: event_end,
                layout: self.source.layout,
                endpoint: 3,
                split: 0,
                rows: [0; 2],
                committed: 0,
            })
        } else {
            None
        }
    }
    pub fn validate(&self) -> io::Result<()> {
        self.source.validate(self.captured)?;
        source_binding::validate(&self.binding)?;
        nonzero(self.run.recording)?;
        nonzero(self.run.run)?;
        nonzero(self.run.boot)?;
        let loss = &self.losses;
        if loss.publication_start == 0
            || loss.event_start == 0
            || loss.publication_inherited_end < loss.publication_start
            || loss.event_inherited_end < loss.event_start
            || loss.publications != loss.publication_inherited_end - loss.publication_start
            || loss.events > loss.event_inherited_end - loss.event_start
        {
            return Err(invalid());
        }
        match &loss.predecessor {
            Predecessor::Genesis => {
                if loss.prior_window.is_some() {
                    return Err(invalid());
                }
            }
            Predecessor::Canonical {
                transaction,
                kind,
                origin,
            } => {
                nonzero(*transaction)?;
                if (*kind == CanonicalKind::Terminal && *origin != 0)
                    || (matches!(
                        kind,
                        CanonicalKind::Ordinary
                            | CanonicalKind::Fragment
                            | CanonicalKind::FiniteEvents
                            | CanonicalKind::FiniteEventLoss
                    ) && *origin == 0)
                {
                    return Err(invalid());
                }
            }
        }
        if let Some(unknown) = &loss.unknown {
            nonzero(unknown.run)?;
            nonzero(unknown.boot)?;
        }
        self.validate_operation()
    }

    fn validate_operation(&self) -> io::Result<()> {
        let loss = &self.losses;
        match self.operation {
            AttemptOperation::Ordinary {
                window,
                attempted_origin,
            } => {
                if attempted_origin != loss.publication_inherited_end {
                    return Err(invalid());
                }
                if loss.prior_window.is_some_and(|prior| window <= prior) {
                    return Err(invalid());
                }
            }
            AttemptOperation::Finite {
                event_end,
                source_first,
                ..
            } => {
                if event_end <= loss.event_start
                    || loss.event_start < self.binding.event_first
                    || source_first < loss.event_start
                    || source_first > event_end
                    || event_end - source_first
                        > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
                    || loss.event_inherited_end != loss.event_start
                    || loss.events != 0
                    || loss.publication_inherited_end != loss.publication_start
                    || loss.publications != 0
                {
                    return Err(invalid());
                }
            }
            AttemptOperation::Terminal { event_end } => {
                if event_end < loss.event_inherited_end {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }

    pub fn encode_registered(
        &self,
        transaction: Identity,
        bytes: &mut [u8; 4096],
    ) -> io::Result<()> {
        self.validate()?;
        nonzero(transaction)?;
        codec::encode_idle(self.run.recording, SlotKind::Foreground, bytes)?;
        put_identity(bytes, 32, transaction);
        put32(bytes, 48, 1);
        put32(bytes, 56, 2048);
        put_identity(bytes, 64, self.run.run);
        put_identity(bytes, 80, self.run.boot);
        match self.operation {
            AttemptOperation::Ordinary {
                window,
                attempted_origin,
            } => {
                put32(bytes, 52, 1);
                put64(bytes, 96, window);
                put64(bytes, 104, attempted_origin);
                put32(bytes, 136, 4095);
            }
            AttemptOperation::Finite {
                event_end,
                order_floor,
                ..
            } => {
                put32(bytes, 52, 3);
                put64(bytes, 96, order_floor);
                put32(bytes, 136, 512);
                put32(bytes, 140, 3);
                put64(bytes, 152, event_end);
                self.finite_plan()
                    .ok_or_else(invalid)?
                    .encode((&mut bytes[256..512]).try_into().map_err(|_| invalid())?)?;
            }
            AttemptOperation::Terminal { event_end } => {
                put32(bytes, 52, 2);
                put32(bytes, 140, 3);
                put64(bytes, 152, event_end);
                put32(bytes, 184, 1);
            }
        }
        put64(bytes, 112, self.losses.event_start);
        put32(bytes, 120, crate::SCHEMA_VERSION);
        put32(bytes, 124, u32::from(self.captured));
        put32(bytes, 128, 1);
        put32(bytes, 132, 4096);
        put_identity(bytes, 208, transaction);
        self.source.encode(bytes, 224, 232, 240);
        self.losses.encode(&mut bytes[1088..1344]);
        source_binding::encode(&self.binding, &mut bytes[1352..1400])?;
        Ok(())
    }

    /// Only Registered is executable in this slice. Later phases remain an explicit boundary.
    pub fn decode_registered(bytes: &[u8], recording: Identity) -> io::Result<(Identity, Self)> {
        Self::decode_construction(bytes, recording, false)
    }
    pub(crate) fn decode_construction(
        bytes: &[u8],
        recording: Identity,
        finite: bool,
    ) -> io::Result<(Identity, Self)> {
        let Slot::Occupied {
            transaction,
            phase,
            operation,
            ..
        } = codec::decode_slot(bytes, recording, SlotKind::Foreground)?
        else {
            return Err(invalid());
        };
        if operation == Operation::ExceptionalFragment && !finite {
            return Err(invalid());
        }
        if phase == Phase::SalvagePlanned {
            let state = normal::NormalState::decode(bytes, recording)?;
            if state.row_plan.is_some() {
                return Ok((state.transaction, state.input));
            }
        }
        if phase != Phase::Registered {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "transaction phase requires canonical recovery composition",
            ));
        }
        let operation = match operation {
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
        };
        let input = Self {
            run: RunIdentity {
                recording,
                run: get_identity(bytes, 64)?,
                boot: get_identity(bytes, 80)?,
            },
            operation,
            captured: get32(bytes, 124)? == 1,
            source: SourceTiming::decode(bytes, 224, 232, 240)?,
            binding: source_binding::decode(&bytes[1352..1400])?,
            losses: LossTransfer::decode(&bytes[1088..1344])?,
        };
        // Exact reconstruction checks constants, every unused field and every reserved byte.
        let mut expected = [0; 4096];
        input.encode_registered(transaction, &mut expected)?;
        if expected != bytes {
            return Err(invalid());
        }
        Ok((transaction, input))
    }
}

impl LossTransfer {
    /// Whether rows kept from a minute the ring overran own a tick's events. A window's proof
    /// refuses a tick that starts before its rows, so no window can hold this one, and it counts
    /// as lost with that minute although the ring still holds it.
    pub(crate) fn kept_rows_own(&self, timing: &collector::ring::PublicationTiming) -> bool {
        timing.events_first < self.event_start
    }

    pub(crate) fn encode(&self, bytes: &mut [u8]) {
        bytes.fill(0);
        bytes[..8].copy_from_slice(b"FDLoss02");
        put32(bytes, 8, 1);
        if let Some(unknown) = &self.unknown {
            put32(bytes, 12, 1);
            put64(
                bytes,
                128,
                match unknown.reason {
                    UnknownReason::Predecessor => 1,
                    UnknownReason::Current => 2,
                    UnknownReason::Both => 3,
                },
            );
            put_identity(bytes, 136, unknown.run);
            put_identity(bytes, 152, unknown.boot);
        }
        if let Predecessor::Canonical {
            transaction,
            kind,
            origin,
        } = &self.predecessor
        {
            put_identity(bytes, 16, *transaction);
            put64(
                bytes,
                32,
                match kind {
                    CanonicalKind::Ordinary => 1,
                    CanonicalKind::Terminal => 2,
                    CanonicalKind::Fragment => 3,
                    CanonicalKind::LossOnly => 4,
                    CanonicalKind::FiniteEvents => 5,
                    CanonicalKind::FiniteEventLoss => 6,
                },
            );
            put64(bytes, 40, *origin);
        }
        for (offset, value) in [
            (48, self.publication_start),
            (56, self.publication_inherited_end),
            (64, self.event_start),
            (72, self.event_inherited_end),
            (80, self.publications),
            (88, self.events),
            (96, self.publication_inherited_end),
            (104, self.event_inherited_end),
        ] {
            put64(bytes, offset, value);
        }
        if let Some(window) = self.prior_window {
            put64(bytes, 168, window);
            put32(bytes, 176, 1);
        }
    }

    pub(crate) fn decode(bytes: &[u8]) -> io::Result<Self> {
        let predecessor = match get64(bytes, 32)? {
            0 => Predecessor::Genesis,
            value => Predecessor::Canonical {
                transaction: get_identity(bytes, 16)?,
                kind: match value {
                    1 => CanonicalKind::Ordinary,
                    2 => CanonicalKind::Terminal,
                    3 => CanonicalKind::Fragment,
                    4 => CanonicalKind::LossOnly,
                    5 => CanonicalKind::FiniteEvents,
                    6 => CanonicalKind::FiniteEventLoss,
                    _ => return Err(invalid()),
                },
                origin: get64(bytes, 40)?,
            },
        };
        let unknown = match get64(bytes, 128)? {
            0 => None,
            value => Some(UnknownProvenance {
                reason: match value {
                    1 => UnknownReason::Predecessor,
                    2 => UnknownReason::Current,
                    3 => UnknownReason::Both,
                    _ => return Err(invalid()),
                },
                run: get_identity(bytes, 136)?,
                boot: get_identity(bytes, 152)?,
            }),
        };
        Ok(Self {
            predecessor,
            publication_start: get64(bytes, 48)?,
            publication_inherited_end: get64(bytes, 56)?,
            event_start: get64(bytes, 64)?,
            event_inherited_end: get64(bytes, 72)?,
            publications: get64(bytes, 80)?,
            events: get64(bytes, 88)?,
            unknown,
            prior_window: match get32(bytes, 176)? {
                0 => None,
                1 => Some(get64(bytes, 168)?),
                _ => return Err(invalid()),
            },
        })
    }
}

fn nonzero(identity: Identity) -> io::Result<()> {
    if identity == Identity([0, 0]) {
        Err(invalid())
    } else {
        Ok(())
    }
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
fn get32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    Ok(u32::from_be_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}
fn get64(bytes: &[u8], offset: usize) -> io::Result<u64> {
    Ok(u64::from_be_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}
fn get_identity(bytes: &[u8], offset: usize) -> io::Result<Identity> {
    Ok(Identity([get64(bytes, offset)?, get64(bytes, offset + 8)?]))
}
fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}
fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
}
fn put_identity(bytes: &mut [u8], offset: usize, value: Identity) {
    put64(bytes, offset, value.0[0]);
    put64(bytes, offset + 8, value.0[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literal() -> [u8; 4096] {
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
            (51, 1),
            (55, 1),
            (58, 8),
            (71, 3),
            (79, 4),
            (87, 5),
            (95, 6),
            (111, 20),
            (119, 30),
            (123, 1),
            (131, 1),
            (134, 16),
            (138, 15),
            (139, 255),
            (215, 7),
            (223, 8),
            (231, 5),
            (235, 1),
            (1355, 1),
            (1367, 17),
            (1375, 18),
            (1383, 1),
            (1391, 1),
            (1399, 1),
            (1099, 1),
            (1103, 1),
            (1143, 10),
            (1151, 20),
            (1159, 30),
            (1167, 30),
            (1175, 10),
            (1191, 20),
            (1199, 30),
            (1223, 1),
            (1231, 9),
            (1239, 10),
            (1247, 11),
            (1255, 12),
        ] {
            bytes[offset] = value;
        }
        bytes
    }

    #[test]
    fn registered_codec_matches_independent_literal_with_inherited_loss() {
        // Goal: wire authority is independent of agreement between paired codec routines.
        // Method: decode literal contract bytes and compare a fresh encoding with all bytes.
        let bytes = literal();
        let (transaction, input) =
            AttemptInput::decode_registered(&bytes, Identity([1, 2])).unwrap();
        assert_eq!(input.losses.publications, 10);
        assert_eq!(input.losses.publication_start, 10);
        assert_eq!(input.losses.publication_inherited_end, 20);
        assert_eq!(input.losses.event_start, 30);
        assert!(input.losses.unknown.is_some());
        let mut encoded = [0; 4096];
        input.encode_registered(transaction, &mut encoded).unwrap();
        assert_eq!(encoded, bytes);
    }

    #[test]
    fn registered_codec_accepts_a_terminal_loss_only_predecessor() {
        // Goal: a terminal loss-only gate can transfer its independent frontiers.
        // Method: round-trip its zero terminal key origin through the actual registered codec.
        let (transaction, mut input) =
            AttemptInput::decode_registered(&literal(), Identity([1, 2])).unwrap();
        input.losses.predecessor = Predecessor::Canonical {
            transaction: Identity([9, 10]),
            kind: CanonicalKind::LossOnly,
            origin: 0,
        };
        let mut bytes = [0; 4096];
        input.encode_registered(transaction, &mut bytes).unwrap();
        assert_eq!(
            AttemptInput::decode_registered(&bytes, input.run.recording)
                .unwrap()
                .1,
            input
        );
    }

    #[test]
    fn registered_codec_refuses_reserved_fields_and_cross_field_corruption() {
        // Goal: unrelated phase payloads cannot become executable registered authority.
        // Method: alter constants, padding, domains, counts, profile and the original identity.
        let bytes = literal();
        for offset in [
            11, 15, 51, 55, 58, 63, 119, 123, 127, 131, 135, 139, 143, 151, 159, 167, 183, 187,
            191, 199, 207, 223, 255, 511, 1023, 1087, 1095, 1099, 1103, 1143, 1151, 1159, 1167,
            1175, 1183, 1191, 1199, 1207, 1215, 1223, 1267, 1343, 1351, 2112, 4095,
        ] {
            let mut damaged = bytes;
            damaged[offset] ^= 128;
            assert!(
                AttemptInput::decode_registered(&damaged, Identity([1, 2])).is_err(),
                "offset {offset}"
            );
        }
        for length in [0, 63, 64, 2112, 4095] {
            assert!(AttemptInput::decode_registered(&bytes[..length], Identity([1, 2])).is_err());
        }
    }
    fn finite_literal() -> [u8; 4096] {
        let ordinary = literal();
        let mut bytes = [0; 4096];
        for range in [0..48, 64..96, 120..136, 208..256, 1352..1400] {
            bytes[range.clone()].copy_from_slice(&ordinary[range]);
        }
        bytes[1088..1096].copy_from_slice(b"FDLoss02");
        for (offset, value) in [
            (48, 1_u32),
            (52, 3),
            (56, 2048),
            (136, 512),
            (140, 3),
            (256, 1),
            (260, 2),
            (304, 1),
            (308, 3),
            (1096, 1),
        ] {
            put32(&mut bytes, offset, value);
        }
        for (offset, value) in [
            (96, 3_u64),
            (112, 30),
            (152, 35),
            (264, 30),
            (272, 35),
            (280, 33),
            (288, 35),
            (296, 5),
            (1136, 10),
            (1144, 10),
            (1152, 30),
            (1160, 30),
            (1184, 10),
            (1192, 30),
        ] {
            put64(&mut bytes, offset, value);
        }
        bytes
    }
    #[test]
    fn finite_registered_literal_pins_fixed_construction_without_writable_decode() {
        // Goal: independently pin op3 intent and refuse ordinary writable/cross-operation aliases.
        // Method: literal bytes, reserved-byte corruption and forbidden Ready/cleanup phases.
        let bytes = finite_literal();
        let state = normal::NormalState::decode(&bytes, Identity([1, 2])).unwrap();
        assert!(state.input.is_finite());
        assert_eq!(state.loss.event_end, 30);
        assert_eq!(state.loss.events_new, 0);
        assert_eq!(state.row_plan.unwrap().end, 35);
        assert!(AttemptInput::decode_registered(&bytes, Identity([1, 2])).is_err());
        let mut encoded = [0; 4096];
        state.encode(&mut encoded).unwrap();
        assert_eq!(encoded, bytes);
        for offset in [
            60, 104, 1400, 144, 160, 168, 184, 188, 192, 200, 336, 344, 348, 352, 511, 512, 1024,
            1344, 1496, 2112, 4095,
        ] {
            let mut changed = bytes;
            changed[offset] ^= 128;
            assert!(
                normal::NormalState::decode(&changed, Identity([1, 2])).is_err(),
                "offset {offset}"
            );
        }
        let mut pre_origin = bytes;
        put64(&mut pre_origin, 1392, 31);
        assert!(normal::NormalState::decode(&pre_origin, Identity([1, 2])).is_err());
        for phase in [2, 5] {
            let mut changed = bytes;
            put32(&mut changed, 48, phase);
            assert!(normal::NormalState::decode(&changed, Identity([1, 2])).is_err());
        }
        for operation in [1, 2] {
            let mut changed = bytes;
            put32(&mut changed, 52, operation);
            assert!(normal::NormalState::decode(&changed, Identity([1, 2])).is_err());
        }
    }
    #[test]
    fn finite_count_zero_and_prepublication_extent_have_contextual_wire_rules() {
        // Goal: op3 alone refines overlapping frozen source and prepublication uncertainty.
        // Method: actual state codec plus operation/phase/progress/reserved mutations.
        let mut state = normal::NormalState::decode(&finite_literal(), Identity([1, 2])).unwrap();
        let mut identity = [0; 56];
        for (offset, value) in [(0, 1_u64), (8, 2), (16, 394_720)] {
            put64(&mut identity, offset, value);
        }
        state.census = census_binding::CensusBinding {
            kind: 2,
            digest: [17; 32],
            file: Some(crate::store::FileIdentity::census_decode(&identity).unwrap()),
        };
        state.phase = Phase::SalvagePlanned;
        let mut bytes = [0; 4096];
        state.encode(&mut bytes).unwrap();
        state.phase = Phase::UnresolvedExtent;
        state.encode(&mut bytes).unwrap();
        assert_eq!(get32(&bytes, 188).unwrap(), 1);
        assert_eq!(get64(&bytes, 192).unwrap(), 0);
        assert_eq!(
            normal::NormalState::decode(&bytes, Identity([1, 2])).unwrap(),
            state
        );
        let original = state.row_plan.take().unwrap();
        state.salvage = Some(salvage::LossPlan {
            first: original.first,
            end: original.end,
            snapshot_first: original.source_first,
            snapshot_end: original.source_end,
            layout: original.layout,
            endpoint: 3,
            terminal: false,
        });
        state.loss.event_end = 35;
        state.loss.events_new = 5;
        state.encode(&mut bytes).unwrap();
        assert_eq!(get32(&bytes, 260).unwrap(), 0);
        assert_eq!(get32(&bytes, 304).unwrap(), 2);
        assert_eq!(
            normal::NormalState::decode(&bytes, Identity([1, 2])).unwrap(),
            state
        );
        assert_eq!(
            state.loss_metadata().unwrap().kind,
            CanonicalKind::FiniteEventLoss
        );
        let mut changed = bytes;
        put32(&mut changed, 304, 1);
        assert!(normal::NormalState::decode(&changed, Identity([1, 2])).is_err());
        let mut ordinary = state.clone();
        ordinary.input.operation = AttemptOperation::Ordinary {
            window: 3,
            attempted_origin: 10,
        };
        assert!(ordinary.encode(&mut changed).is_err());
        state.next = 1;
        assert!(state.encode(&mut changed).is_err());
    }
}
