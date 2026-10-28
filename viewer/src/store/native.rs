//! One prepared native publication. Acceptance alone advances its admitted predecessors.
use std::sync::Arc;

use persistence::Error;

use super::{AppsSecond, ByteBudget, ByteReservation, MachineSecond, Missing};
use crate::cancellation::Operation;
use crate::live_app_response::{Observation, Sample};
use crate::live_apps::{Frame, Publication, PublicationEvents};
use crate::live_raw::Raw;
use crate::recording::MachineMinute;

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "native_identity_tests.rs"]
mod identity_tests;

pub(super) struct Prepared {
    pub observation: Observation,
    pub machine: Option<MachineSecond>,
    pub apps: AppsSecond,
    raw: Option<Result<Arc<Raw>, Unavailable>>,
    frame: Option<OwnedFrame>,
    frame_refused: Option<Unavailable>,
    sample: Option<OwnedSample>,
    sample_refused: Option<Unavailable>,
    stage: Stage,
    metadata_ready: bool,
    name_additions: Option<super::payload::NameAdditions>,
    predecessor_eligible: bool,
}

pub(super) struct Input<'source> {
    pub names: &'source super::payload::ProcessNames,
    pub source: Publication<'source>,
    pub events: PublicationEvents<'source>,
    pub current: &'source MachineMinute,
    pub previous: &'source MachineMinute,
    pub previous_frame: Option<&'source Frame>,
    pub previous_apps: Option<&'source AppsSecond>,
}

pub(super) struct OwnedFrame {
    frame: Box<Frame>,
    reservation: ByteReservation,
}

struct OwnedSample {
    sample: Box<Sample>,
    reservation: ByteReservation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NativeRead {
    Idle,
    Accepted,
    Blocked,
    Missed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unavailable {
    Input(&'static str),
    Frame(&'static str),
    Sample(&'static str),
    Aggregate(crate::apps::Error),
    View(crate::apps::Error),
    Raw(&'static str),
    UnvalidatedFrame,
}

#[derive(Debug)]
pub(super) enum PrepareError {
    Admission(Admission),
    Source(Error),
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admission(reason) => write!(formatter, "native admission: {reason:?}"),
            Self::Source(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for PrepareError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Admission {
    Frame,
    FrameWork,
    NameIndex,
    Sample,
    Derivation,
    AppMetadata,
    Payload,
    Raw,
    Machine(super::MachineAdmission),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Frame,
    Sample,
    Raw,
    Machine,
    Payload,
    Ready,
}

/// Resource ownership is excluded from equality of the App facts it protects.
#[derive(Clone, Debug, Default)]
pub struct AppMetadata {
    pub origin: Origin,
    reservation: Option<Arc<ByteReservation>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    #[default]
    Unknown,
    Live(persistence::Losses),
    Saved(persistence::Losses),
}

impl PartialEq for AppMetadata {
    fn eq(&self, other: &Self) -> bool {
        self.origin == other.origin
    }
}

impl Prepared {
    /// An acquisition bound is a candidate fact, unlike an interrupted or failed source read.
    pub(super) fn refuse_events(&mut self) {
        assert!(self.stage == Stage::Frame);
        self.frame_refused = Some(Unavailable::Input("store tick events"));
        self.stage = Stage::Sample;
    }

    pub fn new(source: &Publication<'_>) -> Result<Self, PrepareError> {
        let identity = source
            .reader
            .identity()
            .ok_or(PrepareError::Source(Error::Bound(
                "native publication identity",
            )))?;
        Ok(Self {
            observation: Observation {
                recording: identity.recording.0,
                stamp: crate::apps::Stamp {
                    run: identity.run.0,
                    boot: identity.boot.0,
                    publication: source.publication,
                    continuous_ns: source.tick.times.map(|times| times.continuous_ns),
                    awake_ns: source.tick.times.map(|times| times.awake_ns),
                },
                run_tick: source.tick.run_tick,
                wall_ns: source.tick.times.map(|times| times.wall_ns),
            },
            machine: None,
            apps: AppsSecond::default(),
            raw: None,
            frame: None,
            frame_refused: None,
            sample: None,
            sample_refused: None,
            stage: Stage::Frame,
            metadata_ready: false,
            name_additions: None,
            predecessor_eligible: false,
        })
    }

    /// Each successful stage runs once. Admission or cancellation leaves completed work owned.
    pub fn resume(
        &mut self,
        input: &Input<'_>,
        budget: &Arc<ByteBudget>,
        operation: &Operation,
    ) -> Result<(), PrepareError> {
        assert_eq!(self.observation.stamp.publication, input.source.publication);
        for _ in 0..6 {
            operation.check().map_err(PrepareError::Source)?;
            match self.stage {
                Stage::Frame => self.capture_frame(input, budget)?,
                Stage::Sample => self.capture_sample(input, budget)?,
                Stage::Raw => self.capture_raw(input, budget, operation)?,
                Stage::Machine => self.capture_machine(input, budget)?,
                Stage::Payload => self.capture_payload(input, budget)?,
                Stage::Ready => return Ok(()),
            }
        }
        assert!(self.stage == Stage::Ready);
        operation.check().map_err(PrepareError::Source)
    }

    #[cfg(test)]
    pub fn apps(&self) -> &AppsSecond {
        assert!(self.stage == Stage::Ready);
        &self.apps
    }

    pub fn raw(&self) -> Result<&Arc<Raw>, &Unavailable> {
        assert!(self.stage == Stage::Ready);
        self.raw
            .as_ref()
            .unwrap_or_else(|| unreachable!("prepared raw facet"))
            .as_ref()
    }

    #[cfg(test)]
    pub fn app_refusal(&self) -> Option<Unavailable> {
        assert!(self.stage == Stage::Ready);
        self.sample_refused
    }

    pub fn ready(&self) -> bool {
        self.stage == Stage::Ready
    }

    pub fn into_frame(self) -> Option<OwnedFrame> {
        assert!(self.stage == Stage::Ready);
        assert!(self.name_additions.is_none());
        if self.predecessor_eligible {
            self.frame
        } else {
            None
        }
    }

    pub(super) fn commit_names(&mut self, names: &mut super::payload::ProcessNames) {
        if let Some(additions) = self.name_additions.take() {
            names.commit(additions);
        }
    }

    fn capture_frame(
        &mut self,
        input: &Input<'_>,
        budget: &Arc<ByteBudget>,
    ) -> Result<(), PrepareError> {
        if let Err(error) = validate(input) {
            self.frame_refused = Some(bound(error, Unavailable::Input)?);
            self.stage = Stage::Sample;
            return Ok(());
        }
        let bytes = match Frame::capture_bytes(&input.source, &input.events) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.frame_refused = Some(bound(error, Unavailable::Frame)?);
                self.stage = Stage::Sample;
                return Ok(());
            }
        };
        let mut reservation = reserve(budget, bytes, Admission::Frame)?;
        let work = reserve(budget, bytes, Admission::FrameWork)?;
        let mut frame = None;
        let captured = Frame::capture(&input.source, &mut frame).and_then(|()| {
            frame
                .as_deref_mut()
                .unwrap_or_else(|| unreachable!("captured frame"))
                .capture_exits(&input.source, &input.events)
        });
        match captured {
            Ok(()) => {
                let frame = frame.unwrap_or_else(|| unreachable!("captured frame"));
                reservation.shrink_to(frame.bytes());
                self.frame = Some(OwnedFrame { frame, reservation });
                self.predecessor_eligible = true;
            }
            Err(error) => self.frame_refused = Some(bound(error, Unavailable::Frame)?),
        }
        drop(work);
        self.stage = Stage::Sample;
        Ok(())
    }

    fn capture_sample(
        &mut self,
        input: &Input<'_>,
        budget: &Arc<ByteBudget>,
    ) -> Result<(), PrepareError> {
        let Some(frame) = &self.frame else {
            self.sample_refused = self.frame_refused;
            self.stage = Stage::Raw;
            return Ok(());
        };
        let mut reservation = reserve(
            budget,
            crate::live_app_response::SAMPLE_BYTES_MAX,
            Admission::Sample,
        )?;
        let work = reserve(
            budget,
            frame.get().aggregate_work_bytes(input.previous_frame),
            Admission::Derivation,
        )?;
        let previous = input.previous_frame.filter(|old| {
            frame
                .get()
                .observation()
                .can_use_predecessor(&old.observation())
        });
        let mut sample = None;
        match Sample::capture_typed(frame.get(), previous, &mut sample) {
            Ok(()) => {
                let sample = sample.unwrap_or_else(|| unreachable!("captured sample"));
                reservation.shrink_to(sample.bytes());
                self.sample = Some(OwnedSample {
                    sample,
                    reservation,
                });
            }
            Err(error) => {
                let retains_input = error.retains_input();
                let refused = match error {
                    crate::live_apps::AggregateError::Input(error) => Unavailable::Aggregate(error),
                    crate::live_apps::AggregateError::View(error) => Unavailable::View(error),
                    crate::live_apps::AggregateError::Presentation(error) => {
                        bound(error, Unavailable::Sample)?
                    }
                };
                self.sample_refused = Some(refused);
                self.predecessor_eligible = retains_input;
            }
        }
        drop(work);
        self.stage = Stage::Raw;
        Ok(())
    }

    fn capture_raw(
        &mut self,
        input: &Input<'_>,
        budget: &Arc<ByteBudget>,
        operation: &Operation,
    ) -> Result<(), PrepareError> {
        if self.frame.is_none() {
            self.raw = Some(Err(Unavailable::UnvalidatedFrame));
        } else {
            let mut raw = None;
            self.raw = Some(
                match Raw::capture_budgeted(
                    &input.source,
                    &input.events,
                    operation,
                    Some(budget),
                    &mut raw,
                ) {
                    Ok(()) => Ok(Arc::new(
                        raw.unwrap_or_else(|| unreachable!("captured raw")),
                    )),
                    Err(crate::live_raw::CaptureError::Budget) => {
                        return Err(PrepareError::Admission(Admission::Raw));
                    }
                    Err(crate::live_raw::CaptureError::Capture(error)) => {
                        Err(bound(error, Unavailable::Raw)?)
                    }
                },
            );
        }
        self.stage = Stage::Machine;
        Ok(())
    }

    fn capture_machine(
        &mut self,
        input: &Input<'_>,
        budget: &Arc<ByteBudget>,
    ) -> Result<(), PrepareError> {
        input
            .source
            .record_work(persistence::work::ingestion::Kind::MachineReduction);
        let reading = super::readings(input.current)
            .next()
            .unwrap_or_else(|| unreachable!("current native machine"));
        let previous = super::readings(input.previous).next();
        self.machine =
            MachineSecond::complete_of(&reading, previous.as_ref(), budget).map_err(|error| {
                match error {
                    super::MachineCaptureError::Admission(reason) => {
                        PrepareError::Admission(Admission::Machine(reason))
                    }
                    super::MachineCaptureError::Projection(error) => PrepareError::Source(error),
                }
            })?;
        if self.machine.as_ref().is_some_and(|machine| {
            matches!(&machine.complete, super::CompleteMachineState::Refused(_))
        }) {
            input
                .source
                .record_work(persistence::work::ingestion::Kind::MachineReduction);
        }
        self.stage = Stage::Payload;
        Ok(())
    }

    fn capture_payload(
        &mut self,
        input: &Input<'_>,
        budget: &Arc<ByteBudget>,
    ) -> Result<(), PrepareError> {
        if !self.metadata_ready {
            self.capture_metadata(input.previous_apps, budget)?;
        }
        if self.apps.missing.is_none() {
            let sample = self
                .sample
                .as_ref()
                .unwrap_or_else(|| unreachable!("sample"))
                .get();
            let (payload, additions) = super::Payload::capture_with_names(sample, input.names)
                .map_err(|missing| match missing {
                    Missing::Budget => PrepareError::Admission(Admission::Payload),
                    _ => unreachable!("payload capture refuses only its byte budget"),
                })?;
            self.apps.processes = Ok(Arc::new(payload));
            self.name_additions = Some(additions);
        }
        self.stage = Stage::Ready;
        Ok(())
    }

    fn capture_metadata(
        &mut self,
        previous: Option<&AppsSecond>,
        budget: &Arc<ByteBudget>,
    ) -> Result<(), PrepareError> {
        let bytes = metadata_bytes(self.sample_refused.as_ref());
        let mut reservation = reserve(budget, bytes, Admission::AppMetadata)?;
        if let Some(sample) = &self.sample {
            let count = sample.get().apps().len();
            self.apps = AppsSecond::default();
            let count = if count <= super::APPS_PER_SECOND_MAX {
                count
            } else {
                0
            };
            self.apps
                .apps
                .replace_admitted(count, budget)
                .map_err(|_| PrepareError::Admission(Admission::AppMetadata))?;
            self.apps.fill(sample.get(), previous, Some(budget));
            self.apps.observation = Some(sample.get().selected);
            if self.apps.missing == Some(Missing::Budget) {
                self.apps = AppsSecond::default();
                return Err(PrepareError::Admission(Admission::AppMetadata));
            }
        } else {
            let reason = self
                .sample_refused
                .as_ref()
                .unwrap_or_else(|| unreachable!("refused sample"))
                .reason();
            let reason = Error::Bound(reason).to_string();
            self.apps
                .take(super::apps::TickApps::Refused(&reason), previous, None);
            self.apps.observation = Some(self.observation);
        }
        reservation.shrink_to(metadata_actual(&self.apps));
        self.apps.metadata = AppMetadata {
            origin: Origin::Unknown,
            reservation: Some(Arc::new(reservation)),
        };
        assert!(self.apps.metadata.bytes() <= bytes);
        self.metadata_ready = true;
        Ok(())
    }
}

impl OwnedFrame {
    pub(super) fn from_reserved(frame: Box<Frame>, reservation: ByteReservation) -> Self {
        assert_eq!(frame.bytes(), reservation.bytes());
        Self { frame, reservation }
    }

    pub fn adopt(
        frame: &mut Option<Box<Frame>>,
        budget: &Arc<ByteBudget>,
    ) -> Result<Option<Self>, PrepareError> {
        let Some(source) = frame.as_deref() else {
            return Ok(None);
        };
        let reservation = reserve(budget, source.bytes(), Admission::Frame)?;
        Ok(Some(Self {
            frame: frame
                .take()
                .unwrap_or_else(|| unreachable!("admitted frame")),
            reservation,
        }))
    }

    pub fn get(&self) -> &Frame {
        assert_eq!(self.frame.bytes(), self.reservation.bytes());
        &self.frame
    }
}

impl OwnedSample {
    fn get(&self) -> &Sample {
        assert!(self.reservation.bytes() >= size_of::<Sample>());
        &self.sample
    }
}

impl AppMetadata {
    pub(super) fn admitted(
        budget: &Arc<ByteBudget>,
        refusal: Option<&str>,
    ) -> Result<Self, Missing> {
        let bytes = size_of::<AppsSecond>()
            + metadata_owner_bytes()
            + refusal.map_or(0, |reason| 3 * reason.len());
        let reservation = budget.reserve(bytes).ok_or(Missing::Budget)?;
        Ok(Self {
            origin: Origin::Unknown,
            reservation: Some(Arc::new(reservation)),
        })
    }

    #[must_use]
    pub fn bytes(&self) -> usize {
        self.reservation
            .as_ref()
            .map_or(0, |reservation| reservation.bytes())
    }
}

impl Unavailable {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Input(reason)
            | Self::Frame(reason)
            | Self::Sample(reason)
            | Self::Raw(reason) => reason,
            Self::View(_) => "live App exit interval bound",
            Self::Aggregate(_) => "live App aggregate input",
            Self::UnvalidatedFrame => "native Frame was not admitted",
        }
    }
}

fn reserve(
    budget: &Arc<ByteBudget>,
    bytes: usize,
    reason: Admission,
) -> Result<ByteReservation, PrepareError> {
    budget.reserve(bytes).ok_or(PrepareError::Admission(reason))
}

fn bound(
    error: Error,
    facet: fn(&'static str) -> Unavailable,
) -> Result<Unavailable, PrepareError> {
    match error {
        Error::Bound(reason) => Ok(facet(reason)),
        error => Err(PrepareError::Source(error)),
    }
}

fn validate(input: &Input<'_>) -> persistence::Result<()> {
    let limits = persistence::Limits::from_shape(&input.source.reader.shape())?;
    limits.validate_tick(
        input.source.publication,
        input.source.tick,
        input.events.events,
    )?;
    if input.source.reader.dictionary_capacity().is_some() {
        limits.validate_identity(
            input.source.tick,
            input.events.events,
            &persistence::IdentitySource {
                reader: input.source.reader,
                processes: input.source.identities,
                events: input.events.identities,
            },
        )?;
    }
    Ok(())
}

fn metadata_bytes(refused: Option<&Unavailable>) -> usize {
    let mut bytes = size_of::<AppsSecond>();
    if let Some(refused) = refused {
        bytes += 3 * ("persistence bound: ".len() + refused.reason().len());
    }
    bytes + metadata_owner_bytes()
}

fn metadata_actual(apps: &AppsSecond) -> usize {
    let mut bytes = size_of::<AppsSecond>();
    for missing in [apps.missing.as_ref(), apps.processes.as_ref().err()]
        .into_iter()
        .flatten()
    {
        if let Missing::Refused(reason) = missing {
            bytes += reason.capacity();
        }
    }
    bytes + metadata_owner_bytes()
}

fn metadata_owner_bytes() -> usize {
    2 * size_of::<usize>() + size_of::<ByteReservation>()
}
