use super::tree::{CanonicalKey, Namespace, invalid};
use crate::transaction::{CanonicalKind, LossTransfer, Predecessor};
use collector::ring::{Identity, RunIdentity};
use std::fs::File;
use std::io::{self, Read, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawBase {
    Data,
    Recovery,
}
impl RawBase {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Recovery => "recovery-data",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawChild {
    Ready,
    Fragment0,
    Fragment1,
    Loss,
}
impl RawChild {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Fragment0 => "fragment-0",
            Self::Fragment1 => "fragment-1",
            Self::Loss => "loss",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointEvidence {
    Unknown,
    OrdinaryIndex,
    SurvivingRow,
    FixedSource,
    ReadyManifest,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    Active,
    AcquiredCompletion,
    DeadGeneration,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalLoss {
    pub inherited: LossTransfer,
    pub publication_end: u64,
    pub event_end: u64,
    pub publications_new: u64,
    pub events_new: u64,
    pub unknown_flags: u32,
}
impl CanonicalLoss {
    pub(crate) fn totals(&self) -> io::Result<crate::Losses> {
        Ok(crate::Losses {
            publications: self
                .inherited
                .publications
                .checked_add(self.publications_new)
                .ok_or_else(invalid)?,
            events: self
                .inherited
                .events
                .checked_add(self.events_new)
                .ok_or_else(invalid)?,
            unknown: self.unknown_flags != 0,
        })
    }
    pub(crate) fn encode(&self, bytes: &mut [u8]) {
        self.inherited.encode(bytes);
        put32(bytes, 12, self.unknown_flags);
        put64(bytes, 96, self.publication_end);
        put64(bytes, 104, self.event_end);
        put64(bytes, 112, self.publications_new);
        put64(bytes, 120, self.events_new);
    }
    pub(crate) fn decode(bytes: &[u8]) -> io::Result<Self> {
        let loss = Self {
            inherited: LossTransfer::decode(bytes)?,
            publication_end: word(bytes, 96)?,
            event_end: word(bytes, 104)?,
            publications_new: word(bytes, 112)?,
            events_new: word(bytes, 120)?,
            unknown_flags: small(bytes, 12)?,
        };
        let mut expected = [0; 256];
        loss.encode(&mut expected);
        if bytes != expected {
            return Err(invalid());
        }
        loss.validate()?;
        Ok(loss)
    }
    fn validate(&self) -> io::Result<()> {
        let old = &self.inherited;
        if old.publication_start == 0
            || old.event_start == 0
            || old.publication_start > old.publication_inherited_end
            || old.publication_inherited_end > self.publication_end
            || old.event_start > old.event_inherited_end
            || old.event_inherited_end > self.event_end
            || old.publications != old.publication_inherited_end - old.publication_start
            || old.events > old.event_inherited_end - old.event_start
            || self.publications_new > self.publication_end - old.publication_inherited_end
            || self.events_new > self.event_end - old.event_inherited_end
            || self.unknown_flags > 3
            || (self.unknown_flags == 0) != old.unknown.is_none()
        {
            return Err(invalid());
        }
        if let Some(unknown) = &old.unknown {
            nonzero(unknown.run)?;
            nonzero(unknown.boot)?;
        }
        match old.predecessor {
            Predecessor::Genesis => {
                if old.prior_window.is_some() {
                    return Err(invalid());
                }
            }
            Predecessor::Canonical {
                transaction,
                kind,
                origin,
            } => {
                nonzero(transaction)?;
                if (kind == CanonicalKind::Terminal && origin != 0)
                    || (matches!(kind, CanonicalKind::Ordinary | CanonicalKind::Fragment)
                        && origin == 0)
                {
                    return Err(invalid());
                }
            }
        }
        self.totals()?;
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Body {
    Ordinary(crate::Manifest),
    Events(crate::event_recovery::Manifest),
    LossOnly,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalMetadata {
    pub(crate) run: RunIdentity,
    pub(crate) transaction: Identity,
    pub(crate) kind: CanonicalKind,
    pub(crate) base: RawBase,
    pub(crate) child: RawChild,
    pub(crate) window: u64,
    pub(crate) attempted_origin: u64,
    pub(crate) canonical_origin: u64,
    pub(crate) original_end: u64,
    pub(crate) source_layout: u64,
    pub(crate) source_origin: Option<u64>,
    pub(crate) evidence: EndpointEvidence,
    pub(crate) lifecycle: Lifecycle,
    pub(crate) captured: bool,
    pub(crate) trailing_unknown: bool,
    pub(crate) loss: CanonicalLoss,
    pub(crate) body: Body,
}
impl CanonicalMetadata {
    pub fn write(&self, file: &mut impl Write) -> io::Result<()> {
        self.validate()?;
        let mut inner = Vec::with_capacity(4096);
        match &self.body {
            Body::Ordinary(manifest) => manifest.write(&mut inner)?,
            Body::Events(manifest) => manifest.write(&mut inner)?,
            Body::LossOnly => {}
        }
        let mut prefix = [0; 512];
        self.encode_prefix(&mut prefix, inner.len())?;
        file.write_all(&prefix)?;
        file.write_all(&inner)
    }
    pub(crate) fn read(file: &mut File) -> io::Result<Self> {
        let length = file.metadata()?.len();
        let mut bytes = [0; 512];
        file.read_exact(&mut bytes)?;
        let inner_length = usize::try_from(small(&bytes, 188)?).map_err(|_| invalid())?;
        if length != 512 + collector::u64_from_usize(inner_length) {
            return Err(invalid());
        }

        let body = match small(&bytes, 184)? {
            0 if inner_length == 0 => Body::LossOnly,
            1 if inner_length <= 4096 => {
                let mut inner = [0; 4096];
                file.read_exact(&mut inner[..inner_length])?;
                if inner_length < 24 || &inner[16..24] != 2_u64.to_le_bytes().as_slice() {
                    return Err(invalid());
                }
                Body::Ordinary(crate::Manifest::read(&mut &inner[..inner_length])?)
            }
            2 if (128..=128 + 24 * 4096).contains(&inner_length) => {
                Body::Events(crate::event_recovery::Manifest::read(file)?)
            }
            _ => return Err(invalid()),
        };
        let metadata = Self::decode_fields(&bytes, body)?;
        let mut expected = [0; 512];
        metadata.encode_prefix(&mut expected, inner_length)?;
        if bytes != expected {
            return Err(invalid());
        }

        metadata.validate()?;
        Ok(metadata)
    }
    fn decode_fields(bytes: &[u8; 512], body: Body) -> io::Result<Self> {
        let metadata = Self {
            run: RunIdentity {
                recording: identity(bytes, 16)?,
                run: identity(bytes, 32)?,
                boot: identity(bytes, 48)?,
            },
            transaction: identity(bytes, 64)?,
            kind: match small(bytes, 12)? {
                1 => CanonicalKind::Ordinary,
                2 => CanonicalKind::Terminal,
                3 => CanonicalKind::Fragment,
                4 => CanonicalKind::LossOnly,
                _ => return Err(invalid()),
            },
            base: match small(bytes, 80)? {
                1 => RawBase::Data,
                2 => RawBase::Recovery,
                _ => return Err(invalid()),
            },
            child: match small(bytes, 84)? {
                0 => RawChild::Ready,
                1 => RawChild::Fragment0,
                2 => RawChild::Fragment1,
                3 => RawChild::Loss,
                _ => return Err(invalid()),
            },
            window: word(bytes, 104)?,
            attempted_origin: word(bytes, 112)?,
            canonical_origin: word(bytes, 120)?,
            original_end: word(bytes, 128)?,
            source_layout: word(bytes, 160)?,
            source_origin: crate::transaction::SourceTiming::decode(bytes, 160, 192, 200)?
                .origin_continuous_ns,
            evidence: match small(bytes, 168)? {
                0 => EndpointEvidence::Unknown,
                1 => EndpointEvidence::OrdinaryIndex,
                2 => EndpointEvidence::SurvivingRow,
                3 => EndpointEvidence::FixedSource,
                4 => EndpointEvidence::ReadyManifest,
                _ => return Err(invalid()),
            },
            lifecycle: match small(bytes, 172)? {
                0 => Lifecycle::Active,
                1 => Lifecycle::AcquiredCompletion,
                2 => Lifecycle::DeadGeneration,
                _ => return Err(invalid()),
            },
            captured: small(bytes, 176)? == 1,
            trailing_unknown: small(bytes, 180)? == 1,
            loss: CanonicalLoss::decode(&bytes[256..])?,
            body,
        };
        Ok(metadata)
    }
    fn encode_prefix(&self, bytes: &mut [u8; 512], inner_length: usize) -> io::Result<()> {
        bytes.fill(0);
        bytes[..8].copy_from_slice(b"FDCAN002");
        put32(bytes, 8, 1);
        put32(bytes, 12, kind_number(self.kind));
        for (offset, value) in [
            (16, self.run.recording),
            (32, self.run.run),
            (48, self.run.boot),
            (64, self.transaction),
            (88, self.transaction),
        ] {
            put_identity(bytes, offset, value);
        }
        put32(
            bytes,
            80,
            match self.base {
                RawBase::Data => 1,
                RawBase::Recovery => 2,
            },
        );
        put32(
            bytes,
            84,
            match self.child {
                RawChild::Ready => 0,
                RawChild::Fragment0 => 1,
                RawChild::Fragment1 => 2,
                RawChild::Loss => 3,
            },
        );
        for (offset, value) in [
            (104, self.window),
            (112, self.attempted_origin),
            (120, self.canonical_origin),
            (128, self.original_end),
            (136, self.loss.inherited.event_start),
            (144, self.loss.event_end),
            (160, self.source_layout),
        ] {
            put64(bytes, offset, value);
        }
        crate::transaction::SourceTiming {
            layout: self.source_layout,
            origin_continuous_ns: self.source_origin,
        }
        .encode(bytes, 160, 192, 200);
        self.encode_tags(bytes);
        put32(
            bytes,
            188,
            u32::try_from(inner_length).map_err(|_| invalid())?,
        );
        self.loss.encode(&mut bytes[256..]);
        Ok(())
    }
    fn encode_tags(&self, bytes: &mut [u8; 512]) {
        put32(
            bytes,
            168,
            match self.evidence {
                EndpointEvidence::Unknown => 0,
                EndpointEvidence::OrdinaryIndex => 1,
                EndpointEvidence::SurvivingRow => 2,
                EndpointEvidence::FixedSource => 3,
                EndpointEvidence::ReadyManifest => 4,
            },
        );
        put32(
            bytes,
            172,
            match self.lifecycle {
                Lifecycle::Active => 0,
                Lifecycle::AcquiredCompletion => 1,
                Lifecycle::DeadGeneration => 2,
            },
        );
        put32(bytes, 176, u32::from(self.captured));
        put32(bytes, 180, u32::from(self.trailing_unknown));
        put32(
            bytes,
            184,
            match self.body {
                Body::Ordinary(_) => 1,
                Body::Events(_) => 2,
                Body::LossOnly => 0,
            },
        );
    }
    pub(crate) fn timing(&self) -> crate::transaction::SourceTiming {
        crate::transaction::SourceTiming {
            layout: self.source_layout,
            origin_continuous_ns: self.source_origin,
        }
    }
    pub(crate) fn validate_key(&self, namespace: Namespace, key: &CanonicalKey) -> io::Result<()> {
        let origin = match (namespace, self.kind) {
            (Namespace::Ordinary, CanonicalKind::Ordinary) => self.canonical_origin,
            (Namespace::Ordinary, CanonicalKind::LossOnly)
                if self.base == RawBase::Data
                    && self.loss.publication_end > self.loss.inherited.publication_start =>
            {
                self.attempted_origin
            }
            (Namespace::Terminal, CanonicalKind::Terminal | CanonicalKind::LossOnly)
                if self.base == RawBase::Recovery =>
            {
                0
            }
            (Namespace::Fragments, CanonicalKind::Fragment | CanonicalKind::LossOnly)
                if self.loss.event_end > self.loss.inherited.event_start
                    && (self.kind == CanonicalKind::Fragment || self.base == RawBase::Data) =>
            {
                self.loss.inherited.event_start
            }
            _ => return Err(invalid()),
        };
        if CanonicalKey::source(namespace, &self.run, origin)? != *key {
            return Err(invalid());
        }
        Ok(())
    }
    pub(crate) fn effective_window(&self) -> Option<u64> {
        if self.kind == CanonicalKind::Ordinary {
            Some(self.window)
        } else {
            self.loss.inherited.prior_window
        }
    }
    pub(crate) fn reference(&self) -> Predecessor {
        let origin = match self.kind {
            CanonicalKind::Ordinary => self.canonical_origin,
            CanonicalKind::Terminal => 0,
            CanonicalKind::LossOnly if self.base == RawBase::Recovery => 0,
            CanonicalKind::LossOnly
                if self.loss.publication_end > self.loss.inherited.publication_start =>
            {
                self.attempted_origin
            }
            CanonicalKind::Fragment | CanonicalKind::LossOnly => self.loss.inherited.event_start,
        };
        Predecessor::Canonical {
            transaction: self.transaction,
            kind: self.kind,
            origin,
        }
    }
    fn has_terminal_lifecycle(&self) -> bool {
        self.base == RawBase::Recovery
            && matches!(self.kind, CanonicalKind::Terminal | CanonicalKind::LossOnly)
            && matches!(
                self.lifecycle,
                Lifecycle::AcquiredCompletion | Lifecycle::DeadGeneration
            )
    }
    pub(crate) fn certified_terminal(&self) -> bool {
        self.has_terminal_lifecycle()
            && !self.trailing_unknown
            && matches!(
                self.evidence,
                EndpointEvidence::FixedSource | EndpointEvidence::ReadyManifest
            )
    }
    pub(crate) fn validate(&self) -> io::Result<()> {
        self.timing().validate(self.captured)?;
        self.validate_provenance()?;
        self.loss.validate()?;
        let loss = self.loss.totals()?;
        let domain = &self.loss.inherited;
        match (&self.body, self.kind) {
            (Body::Ordinary(inner), CanonicalKind::Ordinary) => {
                self.validate_ordinary(inner, loss)?;
            }
            (Body::Events(inner), CanonicalKind::Terminal | CanonicalKind::Fragment) => {
                inner.validate()?;
                if inner.recovery.run != self.run
                    || inner.identities != self.captured
                    || inner.recovery.events_first != domain.event_start
                    || inner.recovery.events_end != self.loss.event_end
                    || inner.recovery.losses != loss
                    || self.canonical_origin != 0
                    || loss.publications != self.loss.publication_end - domain.publication_start
                {
                    return Err(invalid());
                }
                if self.kind == CanonicalKind::Terminal {
                    if self.base != RawBase::Recovery
                        || self.child != RawChild::Ready
                        || self.window != 0
                        || self.attempted_origin != 0
                        || self.original_end != 0
                    {
                        return Err(invalid());
                    }
                } else if !matches!(self.child, RawChild::Fragment0 | RawChild::Fragment1) {
                    return Err(invalid());
                }
            }
            (Body::LossOnly, CanonicalKind::LossOnly) => self.validate_loss_only(loss)?,
            _ => return Err(invalid()),
        }
        if self.has_terminal_lifecycle() && !self.certified_terminal() {
            return Err(invalid());
        }
        Ok(())
    }
    fn validate_provenance(&self) -> io::Result<()> {
        nonzero(self.run.recording)?;
        nonzero(self.run.run)?;
        nonzero(self.run.boot)?;
        nonzero(self.transaction)?;
        if self.evidence == EndpointEvidence::Unknown
            || (self.trailing_unknown
                && self.lifecycle == Lifecycle::AcquiredCompletion
                && self.kind == CanonicalKind::Terminal)
        {
            return Err(invalid());
        }
        if self.base == RawBase::Recovery
            && (self.window != 0 || self.attempted_origin != 0 || self.original_end != 0)
        {
            return Err(invalid());
        }
        if self.base == RawBase::Data
            && self.original_end != 0
            && self.original_end <= self.attempted_origin
        {
            return Err(invalid());
        }
        if self.base == RawBase::Data && self.attempted_origin == 0 {
            return Err(invalid());
        }
        Ok(())
    }
    fn validate_loss_only(&self, loss: crate::Losses) -> io::Result<()> {
        let domain = &self.loss.inherited;

        if (self.base == RawBase::Data && self.attempted_origin != domain.publication_inherited_end)
            || self.child != RawChild::Loss
            || self.canonical_origin != 0
            || self.trailing_unknown
            || loss.unknown
            || loss.publications != self.loss.publication_end - domain.publication_start
            || loss.events != self.loss.event_end - domain.event_start
            || (loss.publications == 0 && loss.events == 0)
        {
            return Err(invalid());
        }
        if self.base == RawBase::Recovery
            && (self.window != 0 || self.attempted_origin != 0 || self.original_end != 0)
        {
            return Err(invalid());
        }
        if self.base == RawBase::Data
            && loss.publications > 0
            && (self.attempted_origin < domain.publication_start
                || self.attempted_origin >= self.loss.publication_end
                || self.original_end != self.loss.publication_end)
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn validate_ordinary(&self, inner: &crate::Manifest, loss: crate::Losses) -> io::Result<()> {
        inner.validate()?;
        if self.base != RawBase::Data
            || self.child != RawChild::Ready
            || inner.identity.run != self.run
            || inner.identity.window != self.window
            || inner.publication_first != self.canonical_origin
            || self.attempted_origin != self.loss.inherited.publication_inherited_end
            || self.attempted_origin > self.canonical_origin
            || self.attempted_origin < self.loss.inherited.publication_start
            || self.canonical_origin >= self.original_end
            || inner.publication_end != self.original_end
            || self.loss.publication_end != self.original_end
            || inner.events_first != self.loss.inherited.event_start
            || inner.events_end != self.loss.event_end
            || inner.losses != loss
            || inner.event_batches.is_none()
            || inner.data_directory
                != format!(
                    "data/{}/ready",
                    collector::recording::identity_text(self.transaction)
                )
        {
            return Err(invalid());
        }
        for table in crate::schema::TABLES {
            if !inner.tables.iter().any(|item| item.name == table.name()) {
                return Err(invalid());
            }
        }
        let ticks = inner
            .tables
            .iter()
            .find(|table| table.name == "ticks")
            .ok_or_else(invalid)?
            .rows;
        let events = inner
            .tables
            .iter()
            .find(|table| table.name == "events")
            .ok_or_else(invalid)?
            .rows;
        if ticks == 0
            || ticks > 61
            || ticks.checked_add(loss.publications)
                != Some(self.loss.publication_end - self.loss.inherited.publication_start)
            || events > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
            || events.checked_add(loss.events)
                != Some(self.loss.event_end - self.loss.inherited.event_start)
            || self
                .loss
                .inherited
                .prior_window
                .is_some_and(|prior| self.window <= prior)
        {
            return Err(invalid());
        }
        Ok(())
    }
}
pub(crate) fn kind_number(kind: CanonicalKind) -> u32 {
    match kind {
        CanonicalKind::Ordinary => 1,
        CanonicalKind::Terminal => 2,
        CanonicalKind::Fragment => 3,
        CanonicalKind::LossOnly => 4,
    }
}
fn nonzero(identity: Identity) -> io::Result<()> {
    if identity == Identity([0, 0]) {
        Err(invalid())
    } else {
        Ok(())
    }
}
fn small(bytes: &[u8], offset: usize) -> io::Result<u32> {
    Ok(u32::from_be_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}
fn word(bytes: &[u8], offset: usize) -> io::Result<u64> {
    Ok(u64::from_be_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}
fn identity(bytes: &[u8], offset: usize) -> io::Result<Identity> {
    Ok(Identity([word(bytes, offset)?, word(bytes, offset + 8)?]))
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
