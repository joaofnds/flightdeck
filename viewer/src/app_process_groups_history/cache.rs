use std::collections::VecDeque;
use std::sync::Arc;

use persistence::{Error, Result};

use super::reduce::{Capture, Phase, Reduction};
use super::{GROUPS_MAX, KEY_BYTES_MAX, POINTS_MAX, ProcessGroupReading};
use crate::cancellation::Operation;
use crate::live_app_history::cohort::Snapshot;
use crate::live_app_history::{CompactMeasure, ResidentPin, Target};
use crate::live_app_response::{KeyKind, Observation};

pub(crate) struct Cache {
    kind: KeyKind,
    key: Box<[u8]>,
    points: VecDeque<Box<Point>>,
    names: Vec<Arc<InternedName>>,
    pins: VecDeque<Box<ResidentPin>>,
    limited: Option<(usize, u64)>,
}

pub(crate) struct Point {
    pub observation: Observation,
    pub predecessor: Option<Observation>,
    elapsed_ns: Option<u64>,
    membership_complete: bool,
    unnamed_members: Option<u32>,
    groups: Vec<Group>,
}

struct Group {
    name: Arc<InternedName>,
    measures: [CompactMeasure; 10],
    members: u16,
}

struct InternedName {
    bytes: Box<[u8]>,
}

const PINS_MAX: usize = POINTS_MAX * 2;
const CONTAINERS_BYTES: usize = POINTS_MAX * size_of::<Box<Point>>()
    + GROUPS_MAX * size_of::<Arc<InternedName>>()
    + PINS_MAX * size_of::<Box<ResidentPin>>();

const _: () = {
    assert!(size_of::<Arc<InternedName>>() == 8);
    assert!(size_of::<Group>() <= 136);
    assert!(CONTAINERS_BYTES < 512 * 1024);
};

impl Cache {
    pub(crate) fn new(target: &Target<'_>) -> Box<Self> {
        Box::new(Self {
            kind: target.kind,
            key: target.bytes.into(),
            points: VecDeque::new(),
            names: Vec::new(),
            pins: VecDeque::new(),
            limited: None,
        })
    }

    pub(crate) fn matches(&self, target: &Target<'_>) -> bool {
        if self.kind != target.kind {
            return false;
        }

        self.key.as_ref() == target.bytes
    }

    pub(crate) fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.key.len()
            + self.points.capacity() * size_of::<Box<Point>>()
            + self.names.capacity() * size_of::<Arc<InternedName>>()
            + self.pins.capacity() * size_of::<Box<ResidentPin>>()
            + self.points.iter().map(|point| point.bytes()).sum::<usize>()
            + self.names.iter().map(|name| name.bytes()).sum::<usize>()
            + self.pins.iter().map(|pin| pin.bytes()).sum::<usize>()
    }

    pub(crate) fn prepare(&mut self, available: usize, second: u64) -> bool {
        if let Some((previous, failed_second)) = self.limited {
            if available <= previous {
                if second <= failed_second.saturating_add(899) {
                    return false;
                }
            }

            self.limited = None;
        }

        if self.points.capacity() == 0 {
            if CONTAINERS_BYTES > available.saturating_sub(self.bytes()) {
                return false;
            }

            self.points = VecDeque::with_capacity(POINTS_MAX);
            self.names = Vec::with_capacity(GROUPS_MAX);
            self.pins = VecDeque::with_capacity(PINS_MAX);
        }

        self.bytes() <= available
    }

    pub(crate) fn points(&self) -> impl Iterator<Item = &Point> {
        self.points.iter().map(Box::as_ref)
    }

    pub(crate) fn pins(&self) -> impl Iterator<Item = &ResidentPin> {
        self.pins.iter().map(Box::as_ref)
    }

    pub(crate) fn retain(
        &mut self,
        operation: &Operation,
        mut needed: impl FnMut(&Point) -> Result<bool>,
    ) -> Result<()> {
        let mut failure = None;
        self.points.retain(
            |point| match operation.check().and_then(|()| needed(point)) {
                Ok(keep) => keep,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }

        self.names.retain(|name| Arc::strong_count(name) > 1);
        self.pins
            .retain(|pin| match retain_pin(&self.points, pin, operation) {
                Ok(keep) => keep,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            });

        if let Some(error) = failure {
            return Err(error);
        }

        Ok(())
    }

    pub(crate) fn contains(&self, observation: &Observation) -> Result<bool> {
        for point in &self.points {
            if point.observation.recording != observation.recording {
                continue;
            }

            if point.observation.stamp.boot != observation.stamp.boot {
                continue;
            }

            if point.observation.stamp.run != observation.stamp.run {
                continue;
            }

            if point.observation.stamp.publication != observation.stamp.publication {
                continue;
            }

            if point.observation != *observation {
                return Err(Error::Bound("process group cache observation conflict"));
            }

            return Ok(true);
        }

        Ok(false)
    }

    pub(crate) fn insert(
        &mut self,
        snapshot: &Snapshot,
        predecessor: Option<&Observation>,
        available: usize,
        operation: &Operation,
    ) -> Result<bool> {
        if !self.insert_fits(snapshot, available, operation)? {
            return Ok(false);
        }

        let mut point = Box::new(Point {
            observation: snapshot.observation,
            predecessor: predecessor.copied(),
            elapsed_ns: snapshot.elapsed_ns,
            membership_complete: snapshot.membership_complete,
            unnamed_members: snapshot.unnamed_members,
            groups: Vec::with_capacity(snapshot.groups.len()),
        });

        for group in &snapshot.groups {
            operation.check()?;
            let name = self.insert_intern(&group.name);
            point.groups.push(Group {
                name,
                members: u16::try_from(group.reading.members)
                    .map_err(|_| Error::Bound("process group cache members"))?,
                measures: group
                    .reading
                    .measures
                    .map(|measure| CompactMeasure::capture(&measure)),
            });
        }

        if self.points.len() == POINTS_MAX {
            return Err(Error::Bound("process group cache points"));
        }

        self.points.push_back(point);
        assert!(self.bytes() <= available);

        Ok(true)
    }

    fn insert_fits(
        &self,
        snapshot: &Snapshot,
        available: usize,
        operation: &Operation,
    ) -> Result<bool> {
        let mut required = size_of::<Point>() + snapshot.groups.len() * size_of::<Group>();
        let mut new_names = 0;
        for group in &snapshot.groups {
            operation.check()?;
            if self.name_index(&group.name).is_err() {
                required += size_of::<InternedName>() + 2 * size_of::<usize>() + group.name.len();
                new_names += 1;
            }
        }

        if required > available.saturating_sub(self.bytes()) {
            return Ok(false);
        }

        if new_names > GROUPS_MAX - self.names.len() {
            return Ok(false);
        }

        let named_bytes: usize = self.names.iter().map(|name| name.bytes.len()).sum();
        if snapshot
            .groups
            .iter()
            .filter(|group| self.name_index(&group.name).is_err())
            .map(|group| group.name.len())
            .sum::<usize>()
            > KEY_BYTES_MAX - named_bytes
        {
            return Ok(false);
        }

        Ok(true)
    }

    fn name_index(&self, name: &[u8]) -> std::result::Result<usize, usize> {
        self.names
            .binary_search_by(|candidate| candidate.bytes.as_ref().cmp(name))
    }

    fn insert_intern(&mut self, name: &[u8]) -> Arc<InternedName> {
        match self.name_index(name) {
            Ok(index) => Arc::clone(&self.names[index]),
            Err(index) => {
                let key = Arc::new(InternedName { bytes: name.into() });
                self.names.insert(index, Arc::clone(&key));
                key
            }
        }
    }

    pub(crate) fn has_pin(&self, pin: &ResidentPin) -> bool {
        self.pins
            .iter()
            .any(|known| known.manifest.identity == pin.manifest.identity)
    }

    pub(crate) fn pin(&mut self, pin: Box<ResidentPin>, available: usize) -> bool {
        if self.has_pin(&pin) {
            return true;
        }

        if pin.bytes() > available.saturating_sub(self.bytes()) {
            return false;
        }

        if self.pins.len() == PINS_MAX {
            return false;
        }

        self.pins.push_back(pin);
        assert!(self.bytes() <= available);

        true
    }

    pub(crate) fn limit(&mut self, available: usize, second: u64) {
        self.points = VecDeque::new();
        self.names = Vec::new();
        self.pins = VecDeque::new();
        if self.limited.is_none() {
            self.limited = Some((available, second));
        }
    }
}

impl InternedName {
    fn bytes(&self) -> usize {
        2 * size_of::<usize>() + size_of::<Self>() + self.bytes.len()
    }
}

impl Point {
    fn bytes(&self) -> usize {
        size_of::<Self>() + self.groups.capacity() * size_of::<Group>()
    }

    pub(crate) fn visit(&self, reduction: &mut Reduction<'_>, phase: Phase) -> Result<()> {
        if !reduction.begin(
            &Capture {
                observation: &self.observation,
                elapsed_ns: self.elapsed_ns,
                source_available: true,
                membership_complete: self.membership_complete,
                unnamed_members: self.unnamed_members,
                live: false,
            },
            phase,
        )? {
            return Ok(());
        }

        for group in &self.groups {
            reduction.check()?;
            let reading = ProcessGroupReading {
                present: true,
                members: u32::from(group.members),
                measures: group.measures.map(CompactMeasure::measure),
            };

            reduction.group(&group.name.bytes, &reading)?;
        }

        Ok(())
    }

    fn retain_pin_uses(&self, pin: &ResidentPin) -> bool {
        [Some(&self.observation), self.predecessor.as_ref()]
            .into_iter()
            .flatten()
            .any(|observation| {
                let manifest = &pin.manifest;
                if observation.recording != manifest.identity.run.recording.0 {
                    return false;
                }

                if observation.stamp.run != manifest.identity.run.run.0 {
                    return false;
                }

                if observation.stamp.boot != manifest.identity.run.boot.0 {
                    return false;
                }

                (manifest.publication_first..manifest.publication_end)
                    .contains(&observation.stamp.publication)
            })
    }
}

fn retain_pin(
    points: &VecDeque<Box<Point>>,
    pin: &ResidentPin,
    operation: &Operation,
) -> Result<bool> {
    for point in points {
        operation.check()?;
        if point.retain_pin_uses(pin) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cancellation;
    use crate::live_app_history::cohort::NamedReading;

    #[test]
    fn accounts_actual_capacity_shared_names_point_metadata_and_exact_measures() {
        // Two points share one interned raw name while retaining all ten exact compact measures.
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
        let mut cache = Cache::new(&Target {
            kind: KeyKind::Bundle,
            bytes: b"/A.app\xff",
        });
        assert!(cache.prepare(64 * 1024 * 1024, 1002));
        let mut snapshot = snapshot(1, 4095);
        snapshot.groups[0].reading.measures[2].known = 9_007_199_254_740_993;
        assert!(
            cache
                .insert(&snapshot, None, 64 * 1024 * 1024, &operation)
                .unwrap()
        );
        snapshot.observation.stamp.publication += 1;
        assert!(
            cache
                .insert(&snapshot, None, 64 * 1024 * 1024, &operation)
                .unwrap()
        );
        let expected = size_of::<Cache>()
            + b"/A.app\xff".len()
            + cache.points.capacity() * size_of::<Box<Point>>()
            + cache.names.capacity() * size_of::<Arc<InternedName>>()
            + cache.pins.capacity() * size_of::<Box<ResidentPin>>()
            + 2 * (size_of::<Point>() + size_of::<Group>())
            + 2 * size_of::<usize>()
            + size_of::<InternedName>()
            + 4095;

        assert_eq!(cache.bytes(), expected);
        assert_eq!(cache.names.len(), 1);
        assert_eq!(Arc::strong_count(&cache.names[0]), 3);
        assert_eq!(
            cache.points[1].groups[0].measures[2].measure().known,
            9_007_199_254_740_993
        );
        assert_eq!(size_of::<Group>(), 136);
        eprintln!(
            "group cache layout: root={} point={} row={} name={} containers={} two_points={}",
            size_of::<Cache>(),
            size_of::<Point>(),
            size_of::<Group>(),
            size_of::<InternedName>(),
            CONTAINERS_BYTES,
            cache.bytes()
        );
    }

    #[test]
    fn refuses_name_count_growth_and_aggregate_key_bytes_without_partial_insertion() {
        // Exercise exact 24,000-name and eight-MiB boundaries, then one new raw name past each.
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
        let mut cache = Cache::new(&Target {
            kind: KeyKind::Bundle,
            bytes: b"/A.app",
        });
        assert!(cache.prepare(64 * 1024 * 1024, 1002));
        assert!(
            cache
                .insert(&snapshot(24_000, 8), None, 64 * 1024 * 1024, &operation)
                .unwrap()
        );
        let bytes = cache.bytes();
        let mut extra = snapshot(1, 1);
        extra.groups[0].name = b"z".as_slice().into();
        assert!(
            !cache
                .insert(&extra, None, 64 * 1024 * 1024, &operation)
                .unwrap()
        );
        assert_eq!(cache.bytes(), bytes);
        cache.limit(64 * 1024 * 1024, 1002);
        assert!(cache.prepare(64 * 1024 * 1024, 1902));
        let mut keys = snapshot(2049, 4095);
        keys.groups[2048].name = vec![b'z'; 2048].into_boxed_slice();
        assert!(
            cache
                .insert(&keys, None, 64 * 1024 * 1024, &operation)
                .unwrap()
        );
        assert_eq!(
            cache
                .names
                .iter()
                .map(|name| name.bytes.len())
                .sum::<usize>(),
            KEY_BYTES_MAX
        );
        let bytes = cache.bytes();
        assert!(
            !cache
                .insert(&extra, None, 64 * 1024 * 1024, &operation)
                .unwrap()
        );
        assert_eq!(cache.bytes(), bytes);
    }

    #[test]
    fn exact_identity_conflicts_and_raw_target_keys_never_reuse_other_facts() {
        // An identical observation key with a changed clock conflicts; raw bytes and kind differ.
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
        let mut cache = Cache::new(&Target {
            kind: KeyKind::Bundle,
            bytes: b"\xff",
        });
        assert!(cache.prepare(64 * 1024 * 1024, 1002));
        let mut snapshot = snapshot(1, 1);
        assert!(
            cache
                .insert(&snapshot, None, 64 * 1024 * 1024, &operation)
                .unwrap()
        );
        snapshot.observation.stamp.awake_ns = None;
        assert!(matches!(
            cache.contains(&snapshot.observation),
            Err(Error::Bound("process group cache observation conflict"))
        ));
        assert!(!cache.matches(&Target {
            kind: KeyKind::Bundle,
            bytes: b"\\xff"
        }));
        assert!(!cache.matches(&Target {
            kind: KeyKind::Executable,
            bytes: b"\xff"
        }));
    }

    #[test]
    fn budget_refusal_retries_only_after_more_room_or_the_old_context_expires() {
        // An unchanged envelope avoids a doomed fill; more room or 900 seconds enables another try.
        let mut cache = Cache::new(&Target {
            kind: KeyKind::Bundle,
            bytes: b"/A.app",
        });
        cache.limit(1024 * 1024, 1002);
        assert!(!cache.prepare(1024 * 1024, 1003));
        cache.limit(1024 * 1024, 1003);
        assert!(cache.prepare(1024 * 1024 + 1, 1003));
        cache.limit(1024 * 1024, 1002);
        assert!(!cache.prepare(1024 * 1024, 1901));
        cache.limit(1024 * 1024, 1901);
        assert!(cache.prepare(1024 * 1024, 1902));
    }

    fn snapshot(count: usize, length: usize) -> Box<Snapshot> {
        let groups = (0..count)
            .map(|index| {
                let mut name = vec![b'x'; length];
                for (target, source) in name
                    .iter_mut()
                    .zip(u32::try_from(index).unwrap().to_be_bytes())
                {
                    *target = source;
                }
                NamedReading {
                    name: name.into_boxed_slice(),
                    reading: ProcessGroupReading {
                        present: true,
                        members: 1,
                        measures: [crate::live_app_response::Measure {
                            known: 0,
                            measured_members: 1,
                            unavailable_members: 0,
                            overflowed: false,
                        }; 10],
                    },
                }
            })
            .collect();
        Box::new(Snapshot {
            observation: Observation {
                recording: [1, 2],
                stamp: crate::apps::Stamp {
                    boot: [3, 4],
                    run: [5, 6],
                    publication: 1,
                    continuous_ns: Some(1_000_000_000),
                    awake_ns: Some(1_000_000_000),
                },
                run_tick: 1,
                wall_ns: Some(1_002_000_000_000),
            },
            elapsed_ns: Some(1_000_000_000),
            membership_complete: true,
            unnamed_members: Some(0),
            groups,
        })
    }
}
