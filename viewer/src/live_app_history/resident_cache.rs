use std::path::PathBuf;

use persistence::{Error, Manifest, Result, WindowIdentity};

use super::{CompactMeasure, Observation};
use crate::apps::AppKey;
use crate::live::resident_history::{AppReading, Capture};
use crate::live_app_response::KeyKind;
use crate::session::OBSERVATIONS_MAX;

pub(crate) struct ResidentCache {
    points: Vec<ResidentPoint>,
    pins: Vec<ResidentPin>,
    bytes: usize,
}

pub(crate) struct ResidentPoint {
    pub capture: Capture,
    pub window: WindowIdentity,
    apps: Vec<ResidentApp>,
}

struct ResidentApp {
    kind: KeyKind,
    bytes: Box<[u8]>,
    resident: CompactMeasure,
}

pub(crate) struct ResidentPin {
    pub manifest: Manifest,
    pub files: Vec<ResidentFile>,
}

pub(crate) struct ResidentFile {
    pub path: PathBuf,
    pub alias: String,
}

pub(crate) const PIN_BYTES_MAX: usize = 16 * 1024;
const CONTAINER_BYTES: usize =
    OBSERVATIONS_MAX * (size_of::<ResidentPoint>() + size_of::<ResidentPin>());

impl ResidentCache {
    pub(crate) fn new() -> Self {
        Self {
            points: Vec::new(),
            pins: Vec::new(),
            bytes: 0,
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn prepare(&mut self, available: usize) -> bool {
        if self.points.capacity() > 0 {
            return true;
        }
        if available < CONTAINER_BYTES {
            return false;
        }

        self.points = Vec::with_capacity(OBSERVATIONS_MAX);
        self.pins = Vec::with_capacity(OBSERVATIONS_MAX);
        self.bytes = self.points.capacity() * size_of::<ResidentPoint>()
            + self.pins.capacity() * size_of::<ResidentPin>();
        assert!(self.bytes() <= available);
        true
    }

    pub(crate) fn contains(
        &self,
        observation: &Observation,
        operation: &crate::cancellation::Operation,
    ) -> Result<bool> {
        for point in &self.points {
            operation.check()?;
            let captured = &point.capture.observation;
            if crate::live::resident_history::identity(captured)
                == crate::live::resident_history::identity(observation)
            {
                if captured != observation {
                    return Err(Error::Bound("resident cache observation conflict"));
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn retain(
        &mut self,
        operation: &crate::cancellation::Operation,
        mut needed: impl FnMut(&Observation) -> Result<bool>,
    ) -> Result<()> {
        let mut failure = None;
        self.points.retain(|point| {
            match operation
                .check()
                .and_then(|()| needed(&point.capture.observation))
            {
                Ok(needed) => needed,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            }
        });
        if let Some(error) = failure.take() {
            return Err(error);
        }
        self.pins.retain(|pin| {
            match retained_window(&self.points, &pin.manifest.identity, operation) {
                Ok(needed) => needed,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        self.bytes = self.points.capacity() * size_of::<ResidentPoint>()
            + self.pins.capacity() * size_of::<ResidentPin>();
        for point in &self.points {
            operation.check()?;
            self.bytes += point.owned_bytes();
        }
        for pin in &self.pins {
            operation.check()?;
            self.bytes += pin.owned_bytes();
        }
        if self.points.is_empty() {
            *self = Self::new();
        }
        Ok(())
    }

    pub(crate) fn points(&self) -> impl Iterator<Item = &ResidentPoint> {
        self.points.iter()
    }

    pub(crate) fn pins(&self) -> &[ResidentPin] {
        &self.pins
    }

    pub(crate) fn has_pin(&self, window: &WindowIdentity) -> bool {
        self.pins.iter().any(|pin| pin.manifest.identity == *window)
    }

    pub(crate) fn insert(&mut self, point: ResidentPoint) {
        assert!(self.points.len() < OBSERVATIONS_MAX);
        self.bytes += point.owned_bytes();
        self.points.push(point);
    }

    pub(crate) fn insert_pin(&mut self, pin: ResidentPin) {
        assert!(self.pins.len() < OBSERVATIONS_MAX);
        self.bytes += pin.owned_bytes();
        self.pins.push(pin);
    }
}

fn retained_window(
    points: &[ResidentPoint],
    window: &WindowIdentity,
    operation: &crate::cancellation::Operation,
) -> Result<bool> {
    for point in points {
        operation.check()?;
        if point.window == *window {
            return Ok(true);
        }
    }
    Ok(false)
}

impl ResidentPoint {
    pub(crate) fn capture(
        frame: &crate::live_apps::Frame,
        window: &WindowIdentity,
        available: usize,
        operation: &crate::cancellation::Operation,
    ) -> Result<Option<Self>> {
        let mut output = None;
        frame.with_aggregate(None, |aggregate| {
            let mut bytes = size_of::<Self>() + aggregate.apps.len() * size_of::<ResidentApp>();
            for key in aggregate.apps.keys() {
                operation.check()?;
                bytes += match key {
                    AppKey::Bundle(bytes) | AppKey::Executable(bytes) => bytes.len(),
                };
            }
            if bytes > available {
                return Ok(());
            }
            let mut apps = Vec::with_capacity(aggregate.apps.len());
            for (key, app) in &aggregate.apps {
                operation.check()?;
                let resident = crate::live_app_response::Measure {
                    known: app.resident_bytes.known,
                    measured_members: app.resident_bytes.measured_members,
                    unavailable_members: app.resident_bytes.unavailable_members,
                    overflowed: app.resident_bytes.overflowed,
                };
                if !crate::live::resident_history::has_resident(&resident) {
                    continue;
                }
                let (kind, bytes) = match key {
                    AppKey::Bundle(bytes) => (KeyKind::Bundle, *bytes),
                    AppKey::Executable(bytes) => (KeyKind::Executable, *bytes),
                };
                apps.push(ResidentApp {
                    kind,
                    bytes: bytes.into(),
                    resident: CompactMeasure::capture(&resident),
                });
            }
            let point = Self {
                capture: crate::live::resident_history::capture(frame, aggregate),
                window: *window,
                apps,
            };
            assert!(point.bytes() <= available);
            output = Some(point);
            Ok(())
        })?;
        Ok(output)
    }

    pub(crate) fn apps(&self) -> impl Iterator<Item = AppReading<'_>> {
        self.apps.iter().map(|app| AppReading {
            key: super::key(app.kind, &app.bytes),
            resident: app.resident.measure(),
        })
    }

    fn bytes(&self) -> usize {
        size_of::<Self>() + self.owned_bytes()
    }

    fn owned_bytes(&self) -> usize {
        self.apps.capacity() * size_of::<ResidentApp>()
            + self.apps.iter().map(|app| app.bytes.len()).sum::<usize>()
    }
}

impl ResidentPin {
    pub(crate) fn bytes(&self) -> usize {
        size_of::<Self>() + self.owned_bytes()
    }

    fn owned_bytes(&self) -> usize {
        self.manifest.data_directory.capacity()
            + self.manifest.tables.capacity() * size_of::<persistence::TableManifest>()
            + self.files.capacity() * size_of::<ResidentFile>()
            + self
                .files
                .iter()
                .map(|file| file.path.capacity() + file.alias.capacity())
                .sum::<usize>()
    }
}
