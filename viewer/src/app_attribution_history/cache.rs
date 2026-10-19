use std::collections::BTreeSet;
use std::sync::Arc;

use persistence::{Error, Result, WindowIdentity};

use super::{AppAttributionMetric, Capture, Reading};
use crate::apps::AppKey;
use crate::cancellation::Operation;
use crate::live_app_history::{CompactMeasure, ResidentPin};
use crate::live_app_response::{Measure, Observation};
use crate::live_apps::Frame;

pub(crate) struct Cache {
    points: Vec<Point>,
    pins: Vec<ResidentPin>,
    bytes: usize,
    limited: Option<usize>,
}

pub(crate) struct Point {
    pub capture: Capture,
    windows: [Option<WindowIdentity>; 2],
    apps: Vec<App>,
}

struct App {
    key: Arc<Key>,
    cpu: CompactMeasure,
    power: CompactMeasure,
    members: u16,
    membership_complete: bool,
}

struct Key {
    bundle: bool,
    bytes: Box<[u8]>,
}

const _: () = {
    assert!(size_of::<App>() <= 40);
    assert!(size_of::<CompactMeasure>() <= 12);
};

impl Cache {
    pub(crate) fn new() -> Self {
        Self {
            points: Vec::new(),
            pins: Vec::new(),
            bytes: size_of::<Self>(),
            limited: None,
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn limited(&self) -> bool {
        self.limited.is_some()
    }

    pub(crate) fn limit(&mut self, available: usize) {
        self.limited = Some(available);
    }

    pub(crate) fn resume(&mut self, available: usize) {
        if self.limited.is_some_and(|previous| available > previous) {
            self.limited = None;
        }
    }

    pub(crate) fn trim(&mut self, available: usize, operation: &Operation) -> Result<()> {
        for _attempt in 0..=4096 {
            operation.check()?;
            if self.bytes <= available {
                return self.retain(operation, |_observation| Ok(true));
            }
            if let Some(point) = self.points.pop() {
                self.bytes -= point.bytes() + point.unshared_key_bytes();
                drop(point);
            } else {
                *self = Self::new();
                return Ok(());
            }
        }
        unreachable!("the bounded attribution cache can release all saved points")
    }

    pub(crate) fn count(&self) -> usize {
        self.points.len()
    }

    pub(crate) fn capture(
        &mut self,
        frame: &Frame,
        previous: Option<&Frame>,
        windows: [Option<WindowIdentity>; 2],
        available: usize,
        operation: &Operation,
    ) -> Result<()> {
        if self.limited() {
            return Ok(());
        }

        let remaining = available.saturating_sub(self.bytes);
        let captured = Point::capture(
            frame,
            previous,
            self.points.last(),
            windows,
            remaining,
            operation,
        )
        .and_then(|point| self.insert(point, available));
        match captured {
            Ok(()) => Ok(()),
            Err(Error::Bound("attribution cache space")) => {
                self.limit(available);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn contains(
        &self,
        observation: &Observation,
        operation: &Operation,
    ) -> Result<bool> {
        for point in &self.points {
            operation.check()?;
            if crate::live::resident_history::identity(&point.capture.coverage.observation)
                == crate::live::resident_history::identity(observation)
            {
                if point.capture.coverage.observation != *observation {
                    return Err(Error::Bound("attribution cache observation conflict"));
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn retain(
        &mut self,
        operation: &Operation,
        mut needed: impl FnMut(&Observation) -> Result<bool>,
    ) -> Result<()> {
        let mut failure = None;
        let count = self.points.len();
        self.points
            .retain(|point| match needed(&point.capture.coverage.observation) {
                Ok(needed) => needed,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            });
        if let Some(error) = failure {
            return Err(error);
        }
        if self.points.len() != count {
            self.limited = None;
        }
        let mut failure = None;
        self.pins.retain(|pin| {
            for point in &self.points {
                if let Err(error) = operation.check() {
                    failure = Some(error);
                    return false;
                }
                if point.windows.contains(&Some(pin.manifest.identity)) {
                    return true;
                }
            }
            false
        });
        if let Some(error) = failure {
            return Err(error);
        }
        self.bytes = size_of::<Self>()
            + self.points.capacity() * size_of::<Point>()
            + self.pins.capacity() * size_of::<ResidentPin>();
        let mut keys = BTreeSet::new();
        for point in &self.points {
            operation.check()?;
            self.bytes += point.bytes();
            for app in &point.apps {
                operation.check()?;
                if keys.insert(Arc::as_ptr(&app.key).addr()) {
                    self.bytes += app.key.bytes();
                }
            }
        }
        for pin in &self.pins {
            operation.check()?;
            self.bytes += pin.owned_bytes();
        }
        Ok(())
    }

    pub(crate) fn points(&self) -> impl Iterator<Item = &Point> {
        self.points.iter()
    }
    pub(crate) fn pins(&self) -> &[ResidentPin] {
        &self.pins
    }

    pub(crate) fn insert(&mut self, point: Point, available: usize) -> Result<()> {
        let bytes = point.bytes() + point.unshared_key_bytes();
        if self.points.capacity() == 0 {
            let required = 4096 * size_of::<Point>() + bytes;
            if required > available.saturating_sub(self.bytes) {
                return Err(Error::Bound("attribution cache space"));
            }

            self.points = Vec::with_capacity(4096);
            self.bytes += self.points.capacity() * size_of::<Point>();
        }
        if self.points.len() == 4096 {
            return Err(Error::Bound("attribution cache points"));
        }

        if self.bytes() + bytes > available {
            return Err(Error::Bound("attribution cache space"));
        }

        self.bytes += bytes;
        self.points.push(point);
        assert!(self.bytes() <= available);

        Ok(())
    }

    pub(crate) fn pin(&mut self, pin: ResidentPin, available: usize) -> Result<()> {
        if self.limited() {
            return Ok(());
        }

        if pin.bytes() > crate::live_app_history::PIN_BYTES_MAX {
            return Err(Error::Bound("attribution cache pin bytes"));
        }

        if self
            .pins
            .iter()
            .any(|existing| existing.manifest.identity == pin.manifest.identity)
        {
            return Ok(());
        }

        if self.pins.capacity() == 0 {
            let required = 4096 * size_of::<ResidentPin>() + pin.owned_bytes();
            if required > available.saturating_sub(self.bytes) {
                self.limit(available);
                return Ok(());
            }

            self.pins = Vec::with_capacity(4096);
            self.bytes += self.pins.capacity() * size_of::<ResidentPin>();
        }
        if self.pins.len() == 4096 {
            return Err(Error::Bound("attribution cache pins"));
        }

        if self.bytes() + pin.owned_bytes() > available {
            self.limit(available);
            return Ok(());
        }

        self.bytes += pin.owned_bytes();
        self.pins.push(pin);
        assert!(self.bytes() <= available);

        Ok(())
    }
}

impl Point {
    pub(crate) fn capture(
        frame: &Frame,
        previous: Option<&Frame>,
        captured: Option<&Self>,
        windows: [Option<WindowIdentity>; 2],
        available: usize,
        operation: &Operation,
    ) -> Result<Self> {
        let mut output = None;
        frame.with_aggregate(previous, |aggregate| {
            let captured = captured.map_or(&[][..], |point| &point.apps);
            let mut bytes = aggregate.apps.len() * size_of::<App>();
            for key in aggregate.apps.keys() {
                operation.check()?;
                if captured
                    .binary_search_by(|app| app.key.view().cmp(key))
                    .is_err()
                {
                    bytes += Key::owned_bytes(key);
                }
            }
            if bytes > available {
                return Err(Error::Bound("attribution cache space"));
            }
            let mut apps = Vec::with_capacity(aggregate.apps.len());
            for (key, app) in &aggregate.apps {
                operation.check()?;
                let key = match captured.binary_search_by(|app| app.key.view().cmp(key)) {
                    Ok(index) => Arc::clone(&captured[index].key),
                    Err(_) => Arc::new(Key::capture(key)),
                };
                apps.push(App {
                    key,
                    cpu: CompactMeasure::capture(&measure(&app.cpu_ns)),
                    power: CompactMeasure::capture(&measure(&app.energy_nj)),
                    members: u16::try_from(app.members)
                        .map_err(|_| Error::Bound("attribution App members"))?,
                    membership_complete: app.membership_complete,
                });
            }
            let mut coverage = crate::live::resident_history::capture(frame, aggregate);
            coverage.ungrouped_members = aggregate.ungrouped_members;
            output = Some(Self {
                capture: Capture {
                    coverage,
                    elapsed_ns: aggregate.elapsed_ns,
                },
                windows,
                apps,
            });
            Ok(())
        })?;
        output.ok_or(Error::Bound("attribution capture output"))
    }

    pub(crate) fn apps(&self, metric: AppAttributionMetric) -> impl Iterator<Item = Reading<'_>> {
        self.apps.iter().map(move |app| Reading {
            key: app.key.view(),
            measure: match metric {
                AppAttributionMetric::Cpu => app.cpu.measure(),
                AppAttributionMetric::Power => app.power.measure(),
            },
            members: u32::from(app.members),
            membership_complete: app.membership_complete,
        })
    }

    fn bytes(&self) -> usize {
        self.apps.capacity() * size_of::<App>()
    }

    fn unshared_key_bytes(&self) -> usize {
        self.apps
            .iter()
            .filter(|app| Arc::strong_count(&app.key) == 1)
            .map(|app| app.key.bytes())
            .sum()
    }
}

impl Key {
    fn capture(key: &AppKey<'_>) -> Self {
        Self {
            bundle: matches!(key, AppKey::Bundle(_)),
            bytes: match key {
                AppKey::Bundle(bytes) | AppKey::Executable(bytes) => (*bytes).into(),
            },
        }
    }

    fn view(&self) -> AppKey<'_> {
        if self.bundle {
            AppKey::Bundle(&self.bytes)
        } else {
            AppKey::Executable(&self.bytes)
        }
    }

    fn bytes(&self) -> usize {
        2 * size_of::<usize>() + size_of::<Self>() + self.bytes.len()
    }

    fn owned_bytes(key: &AppKey<'_>) -> usize {
        2 * size_of::<usize>()
            + size_of::<Self>()
            + match key {
                AppKey::Bundle(bytes) | AppKey::Executable(bytes) => bytes.len(),
            }
    }
}

fn measure(source: &crate::apps::Subtotal) -> Measure {
    Measure {
        known: source.known,
        measured_members: source.measured_members,
        unavailable_members: source.unavailable_members,
        overflowed: source.overflowed,
    }
}
