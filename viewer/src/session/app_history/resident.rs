use super::sealed_attribution::KeptWindow;
use super::{Frame, Observation, Reading, Restore, Session, Source};
use crate::live::resident_history::SavedResidents;
use crate::live_app_history::{
    PIN_BYTES_MAX, ResidentCache, ResidentFile, ResidentPin, ResidentPoint,
};
use crate::live_app_response::Selected;
use persistence::{Error, Result};
use serde_json::Value;

pub(crate) struct Replay {
    restore: Restore,
    readings: Vec<Reading>,
    /// The windows whose parts give their observations' resident memory in place of a replay.
    kept_windows: Vec<KeptWindow>,
}

impl Session {
    pub(crate) fn with_app_resident_replay(
        &self,
        selected: &Selected,
        consume: impl FnOnce(&mut Replay) -> Result<Value>,
    ) -> Result<Value> {
        self.operation.attach(&self.connection)?;
        self.operation.check()?;
        if collector::recording::read_identity(&self.root)?.0 != selected.observation().recording {
            return Err(Error::Bound("resident history saved recording"));
        }

        let range = self.app_history_range(selected)?;
        let mut restore = Restore::default();
        let mut kept_windows = Vec::new();
        let kept_indexes = self.every_app_kept(&range, &mut restore, &mut kept_windows)?;
        self.app_history_gather(&range, &kept_indexes, &mut restore)?;
        let readings: Vec<_> = restore.readings.values().copied().collect();
        let mut replay = Replay {
            restore,
            readings,
            kept_windows,
        };
        self.app_resident_validate(&replay)?;

        let response = consume(&mut replay)?;
        self.app_resident_validate(&replay)?;

        Ok(response)
    }

    /// Gives `consume` the resident memory of each plotted observation of `replay`: first those
    /// of the windows that keep their parts, then those of the windows it replays.
    pub(crate) fn visit_app_resident(
        &self,
        replay: &mut Replay,
        mut consume: impl FnMut(&SavedResidents<'_>) -> Result<()>,
    ) -> Result<()> {
        for kept in &replay.kept_windows {
            self.visit_app_resident_kept(kept, &mut consume)?;
        }

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

    /// Gives `consume` the resident memory of each plotted observation of `kept`, from the App
    /// rows of its window's parts.
    fn visit_app_resident_kept(
        &self,
        kept: &KeptWindow,
        consume: &mut impl FnMut(&SavedResidents<'_>) -> Result<()>,
    ) -> Result<()> {
        let plotted_headers = kept.plotted_headers();
        if plotted_headers.is_empty() {
            return Ok(());
        }

        let _observation = self.source_scope(kept.window, Some(persistence::schema::APP_PARTS));
        self.validate_commitment(kept.window)?;
        self.visit_kept_parts_residents(&kept.source, &plotted_headers, |header, apps| {
            consume(&SavedResidents::Sealed { header, apps })
        })?;

        self.validate_commitment(kept.window)
    }

    fn app_resident_batch(
        &self,
        replay: &mut Replay,
        range: std::ops::Range<usize>,
        consume: &mut impl FnMut(&SavedResidents<'_>) -> Result<()>,
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
        let _observation = self.source_scope(first.window, None);
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

                consume(&SavedResidents::Replayed(&frame))?;
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
        for kept in &replay.kept_windows {
            self.operation.check()?;
            self.validate_commitment(kept.window)?;
        }
        for ((window, _kind), source) in &replay.restore.sources {
            source.refresh(&self.connection, &self.windows[*window])?;
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
        for kept in &mut self.kept_windows {
            for observation in &mut kept.observations {
                operation.check()?;
                if observation.plotted {
                    if covered(&observation.header.selected)? {
                        observation.plotted = false;
                    }
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
        for kept in &self.kept_windows {
            for kept_observation in &kept.observations {
                operation.check()?;
                if kept_observation.plotted {
                    if kept_observation.header.selected == *observation {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Whether an observation of the replay is still to plot.
    fn plots(&self) -> bool {
        if self.readings.iter().any(|reading| reading.plotted) {
            return true;
        }

        self.kept_windows.iter().any(KeptWindow::plots)
    }

    pub(crate) fn reset(&mut self) {
        for reading in &mut self.readings {
            reading.plotted = true;
        }
        for kept in &mut self.kept_windows {
            for observation in &mut kept.observations {
                observation.plotted = true;
            }
        }
    }

    pub(crate) fn count(&self) -> usize {
        self.readings.len() + KeptWindow::observation_count(&self.kept_windows)
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
                let _observation =
                    self.source_scope(window, file.path.file_stem().and_then(|name| name.to_str()));
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
        if !replay.plots() {
            return Ok(true);
        }
        if !cache.prepare(available) {
            *cache = ResidentCache::new();
            return Ok(false);
        }

        let result = self
            .prepare_resident_pins(replay, cache, available)
            .and_then(|()| {
                self.visit_app_resident(replay, |saved| {
                    self.prepare_resident_capture(saved, cache, available)
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
        saved: &SavedResidents<'_>,
        cache: &mut ResidentCache,
        available: usize,
    ) -> Result<()> {
        let window = self.resident_cache_window(&saved.observation())?;
        let remaining = available
            .checked_sub(cache.bytes())
            .ok_or(Error::Bound("resident cache space"))?;
        let point = ResidentPoint::capture(saved, &window, remaining, &self.operation)?
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
        self.prepare_resident_pins_kept(replay, cache, available)?;
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

    /// Pins the parts of each window of `replay` that keeps them and gives a plotted
    /// observation, which the points taken from them stay pinned to.
    fn prepare_resident_pins_kept(
        &self,
        replay: &Replay,
        cache: &mut ResidentCache,
        available: usize,
    ) -> Result<()> {
        for kept in &replay.kept_windows {
            self.operation.check()?;
            if !kept.plots() {
                continue;
            }
            if cache.has_pin(&self.windows[kept.window].identity) {
                continue;
            }
            if available.saturating_sub(cache.bytes()) < PIN_BYTES_MAX {
                return Err(Error::Bound("resident cache space"));
            }
            let pin = self.kept_pin(kept);
            if pin.bytes() > PIN_BYTES_MAX {
                return Err(Error::Bound("resident cache pin bytes"));
            }
            cache.insert_pin(pin);
            assert!(cache.bytes() <= available);
        }
        Ok(())
    }
}
