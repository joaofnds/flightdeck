use super::{BTreeMap, BTreeSet, Frame, Observation, Reading, Restore, Session, Source};
use crate::live_app_history::{
    PIN_BYTES_MAX, ResidentCache, ResidentFile, ResidentPin, ResidentPoint,
};
use crate::live_app_response::Selected;
use persistence::{Error, Result};
use serde_json::Value;

pub(crate) struct Replay {
    restore: Restore,
    readings: Vec<Reading>,
}

impl Session {
    pub(crate) fn with_app_resident_replay(
        &self,
        selected: &Selected,
        consume: impl FnOnce(&mut Replay) -> Result<Value>,
    ) -> Result<Value> {
        self.operation.attach(&self.connection)?;
        self.operation.check()?;
        if collector::recording::read_identity(&self.root)?.0 != selected.observation.recording {
            return Err(Error::Bound("resident history saved recording"));
        }

        let range = self.app_history_range(selected)?;
        let mut restore = Restore {
            readings: BTreeMap::new(),
            sources: BTreeMap::new(),
            source_bytes: 0,
            plotted: 0,
        };
        self.app_history_gather(&range, &BTreeSet::new(), &mut restore)?;
        let readings: Vec<_> = restore.readings.values().copied().collect();
        let mut replay = Replay { restore, readings };
        self.app_resident_validate(&replay)?;

        let response = consume(&mut replay)?;
        self.app_resident_validate(&replay)?;

        Ok(response)
    }

    pub(crate) fn visit_app_resident(
        &self,
        replay: &mut Replay,
        mut consume: impl FnMut(&Frame) -> Result<()>,
    ) -> Result<()> {
        let mut index = 0;
        for _batch in 0..super::READINGS_MAX {
            self.operation.check()?;
            if index == replay.readings.len() {
                self.app_resident_validate(replay)?;
                return Ok(());
            }

            let count = replay.readings[index..]
                .iter()
                .take_while(|reading| reading.window == replay.readings[index].window)
                .count();
            let end = index + count;
            self.app_resident_batch(replay, index..end, &mut consume)?;
            index = end;
        }

        Err(Error::Bound("resident history saved batches"))
    }

    fn app_resident_batch(
        &self,
        replay: &mut Replay,
        range: std::ops::Range<usize>,
        consume: &mut impl FnMut(&Frame) -> Result<()>,
    ) -> Result<()> {
        let readings: Vec<_> = replay.readings[range]
            .iter()
            .copied()
            .filter(|reading| reading.plotted)
            .collect();
        if readings.is_empty() {
            return Ok(());
        }
        if readings.len() > super::TICKS_PER_WINDOW_MAX {
            return Err(Error::Bound("resident history saved endpoints"));
        }

        let first = readings
            .first()
            .ok_or(Error::Bound("resident history empty batch"))?;
        self.validate_commitment(first.window)?;
        let endpoints: Vec<_> = readings.iter().map(Reading::endpoint).collect();
        let source = self.app_history_source(&mut replay.restore, first.window, Source::Apps)?;
        let mut index = 0;
        Frame::committed_many(
            &endpoints,
            &self.connection,
            source,
            &self.operation,
            |frame| {
                let reading = readings
                    .get(index)
                    .ok_or(Error::Bound("resident history saved output"))?;
                if frame.stamp() != &reading.observation.stamp {
                    return Err(Error::Bound("resident history saved association"));
                }

                consume(&frame)?;
                index += 1;
                Ok(())
            },
        )?;
        assert_eq!(index, readings.len());

        self.validate_commitment(first.window)
    }

    fn app_resident_validate(&self, replay: &Replay) -> Result<()> {
        self.operation.check()?;
        for (window, _source) in replay.restore.sources.keys() {
            self.operation.check()?;
            self.validate_commitment(*window)?;
        }
        for source in replay.restore.sources.values() {
            source.refresh(&self.connection)?;
        }

        self.operation.check()
    }
}

impl Replay {
    pub(crate) fn exclude(
        &mut self,
        operation: &crate::cancellation::Operation,
        mut covered: impl FnMut(&Observation) -> Result<bool>,
    ) -> Result<()> {
        for reading in &mut self.readings {
            operation.check()?;
            if reading.plotted {
                if covered(&reading.observation)? {
                    reading.plotted = false;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn needs(
        &self,
        observation: &Observation,
        operation: &crate::cancellation::Operation,
    ) -> Result<bool> {
        for reading in &self.readings {
            operation.check()?;
            if reading.plotted {
                if reading.observation == *observation {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(crate) fn reset(&mut self) {
        for reading in &mut self.readings {
            reading.plotted = true;
        }
    }

    pub(crate) fn count(&self) -> usize {
        self.readings.len()
    }
}

impl Session {
    pub(crate) fn resident_cache_validate(&self, cache: &ResidentCache) -> Result<()> {
        for pin in cache.pins() {
            self.operation.check()?;
            let window = self
                .windows
                .iter()
                .position(|manifest| manifest.identity == pin.manifest.identity)
                .ok_or(Error::Bound("resident cache commitment missing"))?;
            if self.windows[window] != pin.manifest {
                return Err(Error::Bound("resident cache commitment changed"));
            }
            self.validate_commitment(window)?;
            for file in &pin.files {
                if self.connection.parquet_source(&file.path)? != file.alias {
                    return Err(Error::Bound("resident cache file changed"));
                }
            }
        }
        self.operation.check()
    }

    pub(crate) fn resident_cache_window(
        &self,
        observation: &Observation,
    ) -> Result<persistence::WindowIdentity> {
        let window = self
            .app_history_window(observation)?
            .ok_or(Error::Bound("resident cache window"))?;
        Ok(self.windows[window].identity)
    }

    pub(crate) fn resident_cache_pin(
        &self,
        replay: &Replay,
        observation: &Observation,
    ) -> Result<ResidentPin> {
        let window = self
            .app_history_window(observation)?
            .ok_or(Error::Bound("resident cache window"))?;
        let source = replay
            .restore
            .sources
            .get(&(window, Source::Apps))
            .ok_or(Error::Bound("resident cache source"))?;
        let ticks = replay
            .restore
            .sources
            .get(&(window, Source::Ticks))
            .ok_or(Error::Bound("resident cache ticks"))?;
        let pin = ResidentPin {
            manifest: self.windows[window].clone(),
            files: source
                .files
                .iter()
                .chain(ticks.files.iter())
                .map(|file| ResidentFile {
                    path: file.path.clone(),
                    alias: file.alias.clone(),
                })
                .collect(),
        };
        if pin.bytes() > PIN_BYTES_MAX {
            return Err(Error::Bound("resident cache pin bytes"));
        }
        Ok(pin)
    }
}

impl Session {
    pub(crate) fn prepare_resident_cache(
        &self,
        replay: &mut Replay,
        cache: &mut ResidentCache,
        available: usize,
        mut live: impl FnMut(&Observation) -> Result<bool>,
    ) -> Result<bool> {
        replay.exclude(&self.operation, |observation| {
            cache.contains(observation, &self.operation)?;
            live(observation)
        })?;
        cache.retain(&self.operation, |observation| {
            replay.needs(observation, &self.operation)
        })?;
        self.resident_cache_validate(cache)?;
        replay.exclude(&self.operation, |observation| {
            cache.contains(observation, &self.operation)
        })?;
        if !replay.readings.iter().any(|reading| reading.plotted) {
            return Ok(true);
        }
        if !cache.prepare(available) {
            *cache = ResidentCache::new();
            return Ok(false);
        }

        let result = self
            .prepare_resident_pins(replay, cache, available)
            .and_then(|()| {
                self.visit_app_resident(replay, |frame| {
                    self.prepare_resident_capture(frame, cache, available)
                })
            });
        match result {
            Ok(()) => {
                self.resident_cache_validate(cache)?;
                Ok(true)
            }
            Err(Error::Bound("resident cache space")) => {
                *cache = ResidentCache::new();
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    fn prepare_resident_capture(
        &self,
        frame: &Frame,
        cache: &mut ResidentCache,
        available: usize,
    ) -> Result<()> {
        let observation = Observation {
            recording: frame.identity().recording.0,
            stamp: *frame.stamp(),
            run_tick: frame.run_tick(),
            wall_ns: frame.wall_ns(),
        };
        let window = self.resident_cache_window(&observation)?;
        let remaining = available
            .checked_sub(cache.bytes())
            .ok_or(Error::Bound("resident cache space"))?;
        let point = ResidentPoint::capture(frame, &window, remaining, &self.operation)?
            .ok_or(Error::Bound("resident cache space"))?;
        assert!(cache.has_pin(&window));
        cache.insert(point);
        assert!(cache.bytes() <= available);
        Ok(())
    }

    fn prepare_resident_pins(
        &self,
        replay: &mut Replay,
        cache: &mut ResidentCache,
        available: usize,
    ) -> Result<()> {
        for index in 0..replay.readings.len() {
            self.operation.check()?;
            let reading = replay.readings[index];
            if !reading.plotted {
                continue;
            }
            if cache.has_pin(&self.windows[reading.window].identity) {
                continue;
            }
            if available.saturating_sub(cache.bytes()) < PIN_BYTES_MAX {
                return Err(Error::Bound("resident cache space"));
            }
            self.app_history_source(&mut replay.restore, reading.window, Source::Apps)?;
            cache.insert_pin(self.resident_cache_pin(replay, &reading.observation)?);
            assert!(cache.bytes() <= available);
        }
        Ok(())
    }
}
