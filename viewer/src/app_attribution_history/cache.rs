use std::sync::Arc;

use persistence::{Error, Result, WindowIdentity};

pub(crate) use super::sums::Total;
use super::sums::{SumKey, Summed, Sums};
use super::{AppAttributionMetric, Capture, Reading};
use crate::apps::AppKey;
use crate::cancellation::Operation;
use crate::live::resident_history::Capture as Coverage;
use crate::live_app_history::{CompactMeasure, ResidentFile, ResidentPin};
use crate::live_app_response::{Measure, Observation};
use crate::live_apps::Frame;

pub(crate) struct Cache {
    points: Vec<Point>,
    pins: Vec<ResidentPin>,
    sums: Sums<Arc<Key>>,
    bytes: usize,
    limited: Option<usize>,
}

pub(crate) struct Point {
    pub capture: Capture,
    windows: [Option<WindowIdentity>; 2],
    apps: Vec<App>,
}

/// What a saved point of the histories of every App is taken from.
#[derive(Clone, Copy)]
pub(crate) enum Observed<'observed> {
    /// A frame the replay of its minute gives, and the frame it takes its rates from.
    Replayed {
        frame: &'observed Frame,
        previous: Option<&'observed Frame>,
    },
    /// The facts and the Apps of an observation whose window keeps its Apps' parts.
    Sealed {
        header: &'observed crate::live_app_response::Header,
        apps: &'observed [crate::live_app_response::App],
    },
}

pub(crate) struct App {
    key: Arc<Key>,
    cpu: CompactMeasure,
    power: CompactMeasure,
    disk: CompactMeasure,
    members: u16,
    membership_complete: bool,
}

pub(crate) struct Key {
    bundle: bool,
    bytes: Box<[u8]>,
}

/// One App of an observation, as the measures a point takes from it.
#[derive(Clone, Copy)]
struct MeasuredApp<'key> {
    key: AppKey<'key>,
    cpu: Measure,
    power: Measure,
    disk: Measure,
    members: u32,
    membership_complete: bool,
}

const _: () = {
    assert!(size_of::<App>() <= 48);
    assert!(size_of::<CompactMeasure>() <= 12);
};

impl Cache {
    pub(crate) fn new() -> Self {
        Self {
            points: Vec::new(),
            pins: Vec::new(),
            sums: Sums::new(),
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
                self.bytes -=
                    point.bytes() + self.sums.remove(point.capture.elapsed_ns, &point.apps);
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
        observed: Observed<'_>,
        windows: [Option<WindowIdentity>; 2],
        available: usize,
        operation: &Operation,
    ) -> Result<()> {
        if self.limited() {
            return Ok(());
        }

        let remaining = available.saturating_sub(self.bytes);
        let captured = Point::capture(observed, Some(&self.sums), windows, remaining, operation)
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
        let dropped = self.points.extract_if(.., |point| {
            match needed(&point.capture.coverage.observation) {
                Ok(needed) => !needed,
                Err(error) => {
                    failure = Some(error);
                    true
                }
            }
        });
        for point in dropped {
            self.bytes -= point.bytes() + self.sums.remove(point.capture.elapsed_ns, &point.apps);
        }
        if let Some(error) = failure {
            return Err(error);
        }
        if self.points.len() != count {
            self.limited = None;
        }

        let points = &self.points;
        let dropped = self.pins.extract_if(.., |pin| {
            for point in points {
                if let Err(error) = operation.check() {
                    failure = Some(error);
                    return true;
                }
                if point.windows.contains(&Some(pin.manifest.identity)) {
                    return false;
                }
            }
            true
        });
        for pin in dropped {
            self.bytes -= pin.owned_bytes();
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn points(&self) -> impl Iterator<Item = &Point> {
        self.points.iter()
    }

    /// The per-App totals of every cached point, in key order.
    pub(crate) fn totals(
        &self,
        metric: AppAttributionMetric,
    ) -> impl Iterator<Item = (AppKey<'_>, Total)> {
        self.sums.totals(metric)
    }

    pub(crate) fn pins(&self) -> &[ResidentPin] {
        &self.pins
    }

    pub(crate) fn insert(&mut self, point: Point, available: usize) -> Result<()> {
        let bytes = point.bytes() + self.sums.added_bytes(&point.apps);
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

        let added = self.sums.add(point.capture.elapsed_ns, &point.apps);
        assert!(added <= bytes - point.bytes());
        self.bytes += point.bytes() + added;
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

        if let Some(index) = self
            .pins
            .iter()
            .position(|existing| existing.manifest.identity == pin.manifest.identity)
        {
            return self.pin_files(index, pin, available);
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

    /// Adds to the pin at `index` each file of `pin`, of the same window, that it lacks, since a
    /// window may give one point from its parts and another point's rates from its replay, and
    /// the cache checks every file each point was taken from.
    fn pin_files(&mut self, index: usize, pin: ResidentPin, available: usize) -> Result<()> {
        let pinned = &self.pins[index];
        let missing: Vec<ResidentFile> = pin
            .files
            .into_iter()
            .filter(|file| pinned.files.iter().all(|kept| kept.path != file.path))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }

        let mut files = Vec::with_capacity(pinned.files.len() + missing.len());
        let before = pinned.owned_bytes();
        let after = before + files.capacity() * size_of::<ResidentFile>()
            - pinned.files.capacity() * size_of::<ResidentFile>()
            + missing
                .iter()
                .map(|file| file.path.capacity() + file.alias.capacity())
                .sum::<usize>();
        if size_of::<ResidentPin>() + after > crate::live_app_history::PIN_BYTES_MAX {
            return Err(Error::Bound("attribution cache pin bytes"));
        }
        if after > before {
            if self.bytes() + (after - before) > available {
                self.limit(available);
                return Ok(());
            }
        }

        files.append(&mut self.pins[index].files);
        files.extend(missing);
        self.pins[index].files = files;
        assert_eq!(self.pins[index].owned_bytes(), after);
        self.bytes = self.bytes - before + after;
        assert!(self.bytes() <= available);

        Ok(())
    }
}

impl Point {
    pub(crate) fn capture(
        observed: Observed<'_>,
        sums: Option<&Sums<Arc<Key>>>,
        windows: [Option<WindowIdentity>; 2],
        available: usize,
        operation: &Operation,
    ) -> Result<Self> {
        match observed {
            Observed::Replayed { frame, previous } => {
                Self::capture_replayed(frame, previous, sums, windows, available, operation)
            }
            Observed::Sealed { header, apps } => {
                Self::capture_sealed(header, apps, sums, windows, available, operation)
            }
        }
    }

    fn capture_sealed(
        header: &crate::live_app_response::Header,
        apps: &[crate::live_app_response::App],
        sums: Option<&Sums<Arc<Key>>>,
        windows: [Option<WindowIdentity>; 2],
        available: usize,
        operation: &Operation,
    ) -> Result<Self> {
        let capture = Capture {
            coverage: Coverage {
                observation: header.selected,
                membership_complete: header.membership_complete,
                ungrouped_members: header.ungrouped_members,
                usage_refused: header.usage_refused,
                usage_gone: header.usage_gone,
            },
            elapsed_ns: header.elapsed_ns,
        };
        let apps = apps.iter().map(|app| MeasuredApp {
            key: app.key.view(),
            cpu: app.cpu_ns,
            power: app.energy_nj,
            disk: app.disk_io_bytes,
            members: app.members,
            membership_complete: app.membership_complete,
        });

        Self::capture_measured(capture, apps, sums, windows, available, operation)
    }

    fn capture_replayed(
        frame: &Frame,
        previous: Option<&Frame>,
        sums: Option<&Sums<Arc<Key>>>,
        windows: [Option<WindowIdentity>; 2],
        available: usize,
        operation: &Operation,
    ) -> Result<Self> {
        let mut output = None;
        frame.with_aggregate(previous, |aggregate| {
            let mut coverage = crate::live::resident_history::capture(frame, aggregate);
            coverage.ungrouped_members = aggregate.ungrouped_members;
            let capture = Capture {
                coverage,
                elapsed_ns: aggregate.elapsed_ns,
            };
            let apps = aggregate.apps.iter().map(|(key, app)| MeasuredApp {
                key: *key,
                cpu: measure(&app.cpu_ns),
                power: measure(&app.energy_nj),
                disk: measure(&app.disk_io_bytes),
                members: app.members,
                membership_complete: app.membership_complete,
            });
            output = Some(Self::capture_measured(
                capture, apps, sums, windows, available, operation,
            )?);
            Ok(())
        })?;
        output.ok_or(Error::Bound("attribution capture output"))
    }

    fn capture_measured<'key>(
        capture: Capture,
        apps: impl Iterator<Item = MeasuredApp<'key>> + Clone,
        sums: Option<&Sums<Arc<Key>>>,
        windows: [Option<WindowIdentity>; 2],
        available: usize,
        operation: &Operation,
    ) -> Result<Self> {
        let mut count = 0;
        let mut bytes = 0;
        for measured in apps.clone() {
            operation.check()?;
            count += 1;
            if sums.and_then(|sums| sums.key(&measured.key)).is_none() {
                bytes += Key::owned_bytes(&measured.key);
            }
        }
        if count * size_of::<App>() + bytes > available {
            return Err(Error::Bound("attribution cache space"));
        }

        let mut output = Vec::with_capacity(count);
        for measured in apps {
            operation.check()?;
            // A key the cache already holds is shared, so each name is stored once.
            let key = sums
                .and_then(|sums| sums.key(&measured.key))
                .map_or_else(|| Arc::new(Key::capture(&measured.key)), Arc::clone);
            output.push(App {
                key,
                cpu: CompactMeasure::capture(&measured.cpu),
                power: CompactMeasure::capture(&measured.power),
                disk: CompactMeasure::capture(&measured.disk),
                members: u16::try_from(measured.members)
                    .map_err(|_| Error::Bound("attribution App members"))?,
                membership_complete: measured.membership_complete,
            });
        }
        assert_eq!(output.len(), count);

        Ok(Self {
            capture,
            windows,
            apps: output,
        })
    }

    pub(crate) fn apps(&self, metric: AppAttributionMetric) -> impl Iterator<Item = Reading<'_>> {
        self.apps.iter().map(move |app| Reading {
            key: app.key.view(),
            measure: match metric {
                AppAttributionMetric::Cpu => app.cpu.measure(),
                AppAttributionMetric::Power => app.power.measure(),
                AppAttributionMetric::Disk => app.disk.measure(),
            },
            members: u32::from(app.members),
            membership_complete: app.membership_complete,
        })
    }

    fn bytes(&self) -> usize {
        self.apps.capacity() * size_of::<App>()
    }
}

impl Summed for App {
    type Key = Arc<Key>;

    fn view(&self) -> AppKey<'_> {
        self.key.view()
    }

    fn measures(&self) -> [Measure; 3] {
        [
            self.cpu.measure(),
            self.power.measure(),
            self.disk.measure(),
        ]
    }

    fn key(&self) -> Arc<Key> {
        Arc::clone(&self.key)
    }

    fn key_bytes(&self) -> usize {
        self.key.bytes()
    }
}

impl SumKey for Arc<Key> {
    fn view(&self) -> AppKey<'_> {
        Key::view(self)
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

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;

fn measure(source: &crate::apps::Subtotal) -> Measure {
    Measure {
        known: source.known,
        measured_members: source.measured_members,
        unavailable_members: source.unavailable_members,
        overflowed: source.overflowed,
    }
}
