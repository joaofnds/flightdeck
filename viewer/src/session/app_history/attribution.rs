use super::{BTreeMap, BTreeSet, Frame, Key, Observation, Reading, Restore, Session, Source};
use crate::app_attribution_history::cache::Cache;
use crate::live_app_history::{ResidentFile, ResidentPin};
use persistence::{Error, Result, WindowIdentity};

struct Projection {
    previous: Option<Box<Frame>>,
    reading: Option<Reading>,
}

impl Session {
    pub(crate) fn visit_app_attribution(
        &self,
        selected: &Observation,
        mut excluded: impl FnMut(&Observation) -> Result<bool>,
        mut consume: impl FnMut(&Frame, Option<&Frame>, [Option<WindowIdentity>; 2]) -> Result<()>,
        mut pin: impl FnMut(ResidentPin) -> Result<()>,
    ) -> Result<usize> {
        self.operation.attach(&self.connection)?;
        self.operation.check()?;
        if collector::recording::read_identity(&self.root)?.0 != selected.recording {
            return Err(Error::Bound("attribution saved recording"));
        }
        let range = self.app_history_range(selected)?;
        let mut restore = Restore {
            readings: BTreeMap::new(),
            sources: BTreeMap::new(),
            source_bytes: 0,
            plotted: 0,
        };
        self.app_history_gather(&range, &BTreeSet::new(), &mut restore)?;
        let count = restore.plotted;
        for reading in restore.readings.values_mut() {
            self.operation.check()?;
            if excluded(&reading.observation)? {
                reading.plotted = false;
            }
        }
        let needed = self.attribution_readings(&mut restore)?;
        let readings: Vec<_> = restore
            .readings
            .values()
            .filter(|reading| needed.contains(&Key::new(&reading.observation)))
            .copied()
            .collect();
        let mut projection = Projection {
            previous: None,
            reading: None,
        };
        let mut index = 0;
        for _batch in 0..super::READINGS_MAX {
            self.operation.check()?;
            if index == readings.len() {
                self.attribution_pins(&restore, &mut pin)?;
                return Ok(count);
            }
            let count = readings[index..]
                .iter()
                .take_while(|reading| reading.window == readings[index].window)
                .count();
            let end = index + count;
            self.attribution_batch(
                &mut restore,
                &readings[index..end],
                &mut projection,
                &mut consume,
                &mut pin,
            )?;
            index = end;
        }
        Err(Error::Bound("attribution saved batches"))
    }

    fn attribution_readings(&self, restore: &mut Restore) -> Result<BTreeSet<Key>> {
        let needed = self.attribution_required_keys(restore)?;
        if !needed.is_empty() {
            self.app_history_predecessors(restore)?;
        }
        Ok(needed)
    }

    fn attribution_batch(
        &self,
        restore: &mut Restore,
        readings: &[Reading],
        projection: &mut Projection,
        consume: &mut impl FnMut(&Frame, Option<&Frame>, [Option<WindowIdentity>; 2]) -> Result<()>,
        pin: &mut impl FnMut(ResidentPin) -> Result<()>,
    ) -> Result<()> {
        if readings.len() > super::TICKS_PER_WINDOW_MAX {
            return Err(Error::Bound("attribution saved endpoints"));
        }
        let first = readings
            .first()
            .ok_or(Error::Bound("attribution empty batch"))?;
        self.validate_commitment(first.window)?;
        let endpoints: Vec<_> = readings.iter().map(Reading::endpoint).collect();
        self.app_history_source(restore, first.window, Source::Apps)?;
        pin(self.attribution_pin(restore, first.window)?)?;
        let source = self.app_history_source(restore, first.window, Source::Apps)?;
        let mut index = 0;
        Frame::committed_many(
            &endpoints,
            &self.connection,
            source,
            &self.operation,
            |frame| {
                let reading = readings
                    .get(index)
                    .ok_or(Error::Bound("attribution saved output"))?;
                if frame.stamp() != &reading.observation.stamp {
                    return Err(Error::Bound("attribution saved association"));
                }
                self.attribution_capture(projection, reading, frame, consume)?;
                index += 1;
                Ok(())
            },
        )?;
        assert_eq!(index, readings.len());
        self.validate_commitment(first.window)
    }

    fn attribution_capture(
        &self,
        projection: &mut Projection,
        reading: &Reading,
        current: Box<Frame>,
        consume: &mut impl FnMut(&Frame, Option<&Frame>, [Option<WindowIdentity>; 2]) -> Result<()>,
    ) -> Result<()> {
        if reading.plotted {
            let previous = super::sample_predecessor(
                projection.reading.as_ref(),
                projection.previous.as_deref(),
                reading,
                &current,
            );
            let previous_window = if previous.is_some() {
                projection
                    .reading
                    .map(|reading| self.windows[reading.window].identity)
            } else {
                None
            };
            consume(
                &current,
                previous,
                [Some(self.windows[reading.window].identity), previous_window],
            )?;
        }
        projection.previous = Some(current);
        projection.reading = Some(*reading);
        Ok(())
    }

    fn attribution_pins(
        &self,
        restore: &Restore,
        pin: &mut impl FnMut(ResidentPin) -> Result<()>,
    ) -> Result<()> {
        for (window, kind) in restore.sources.keys() {
            self.operation.check()?;
            self.validate_commitment(*window)?;
            if *kind != Source::Apps {
                continue;
            }
            pin(self.attribution_pin(restore, *window)?)?;
        }
        self.operation.check()
    }

    fn attribution_pin(&self, restore: &Restore, window: usize) -> Result<ResidentPin> {
        let mut files = Vec::with_capacity(3);
        for kind in [Source::Apps, Source::Ticks] {
            let source = restore
                .sources
                .get(&(window, kind))
                .ok_or(Error::Bound("attribution pin source"))?;
            source.refresh(&self.connection)?;
            files.extend(source.files.iter().map(|file| ResidentFile {
                path: file.path.clone(),
                alias: file.alias.clone(),
            }));
        }

        Ok(ResidentPin {
            manifest: self.windows[window].clone(),
            files,
        })
    }

    pub(crate) fn validate_attribution_cache(&self, cache: &Cache) -> Result<()> {
        for pin in cache.pins() {
            self.operation.check()?;
            let window = self
                .windows
                .iter()
                .position(|manifest| manifest.identity == pin.manifest.identity)
                .ok_or(Error::Bound("attribution cache commitment missing"))?;
            if self.windows[window] != pin.manifest {
                return Err(Error::Bound("attribution cache commitment changed"));
            }
            self.validate_commitment(window)?;
            for file in &pin.files {
                if self.connection.parquet_source(&file.path)? != file.alias {
                    return Err(Error::Bound("attribution cache file changed"));
                }
            }
        }
        self.operation.check()
    }
}

pub(crate) struct AttributionReplay {
    restore: Restore,
    context: Vec<Key>,
    readings: Vec<Reading>,
    prepared: Option<BTreeSet<Key>>,
}

const REPLAY_LEDGER_BYTES_MAX: usize = size_of::<AttributionReplay>()
    + super::super::OBSERVATIONS_MAX * size_of::<Key>()
    + super::READINGS_MAX * (size_of::<Reading>() + 2 * size_of::<Key>());
const _: () = {
    assert!(REPLAY_LEDGER_BYTES_MAX < 4 * 1024 * 1024);
};

impl Session {
    pub(crate) fn with_app_attribution_replay<T>(
        &self,
        selected: &Observation,
        consume: impl FnOnce(&mut AttributionReplay) -> Result<T>,
    ) -> Result<T> {
        self.operation.attach(&self.connection)?;
        self.operation.check()?;
        if collector::recording::read_identity(&self.root)?.0 != selected.recording {
            return Err(Error::Bound("attribution saved recording"));
        }

        let range = self.app_history_range(selected)?;
        let mut replay = Box::new(AttributionReplay {
            restore: Restore {
                readings: BTreeMap::new(),
                sources: BTreeMap::new(),
                source_bytes: 0,
                plotted: 0,
            },
            context: Vec::with_capacity(super::super::OBSERVATIONS_MAX),
            readings: Vec::with_capacity(super::READINGS_MAX),
            prepared: None,
        });
        self.app_history_gather(&range, &BTreeSet::new(), &mut replay.restore)?;
        for key in replay.restore.readings.keys() {
            replay.context.push(*key);
        }
        self.attribution_replay_validate(&replay)?;
        let response = consume(&mut replay)?;
        self.attribution_replay_validate(&replay)?;

        Ok(response)
    }

    pub(crate) fn attribution_replay_plan(
        &self,
        replay: &mut AttributionReplay,
        mut excluded: impl FnMut(&Observation) -> Result<bool>,
    ) -> Result<()> {
        assert!(replay.context.len() <= super::super::OBSERVATIONS_MAX);
        assert!(replay.restore.readings.len() <= super::READINGS_MAX);
        for reading in replay.restore.readings.values_mut() {
            self.operation.check()?;
            reading.plotted = false;
        }
        for key in &replay.context {
            self.operation.check()?;
            let reading = replay
                .restore
                .readings
                .get_mut(key)
                .ok_or(Error::Bound("attribution replay context"))?;
            reading.plotted = !excluded(&reading.observation)?;
        }
        let needed = match &replay.prepared {
            None => self.attribution_readings(&mut replay.restore)?,
            Some(prepared) => {
                let needed = self.attribution_required_keys(&replay.restore)?;
                if !needed.is_subset(prepared) {
                    return Err(Error::Bound("attribution replay expanded"));
                }

                needed
            }
        };
        replay.readings.clear();
        for reading in replay.restore.readings.values() {
            self.operation.check()?;
            if needed.contains(&Key::new(&reading.observation)) {
                if replay.readings.len() == super::READINGS_MAX {
                    return Err(Error::Bound("attribution replay endpoints"));
                }

                replay.readings.push(*reading);
            }
        }
        if replay.prepared.is_none() {
            replay.prepared = Some(needed);
        }

        Ok(())
    }

    fn attribution_required_keys(&self, restore: &Restore) -> Result<BTreeSet<Key>> {
        let mut needed = BTreeSet::new();
        for reading in restore.readings.values() {
            self.operation.check()?;
            if reading.plotted {
                let key = Key::new(&reading.observation);
                needed.insert(key);
                if let Some(publication) = key.publication.checked_sub(1) {
                    needed.insert(Key { publication, ..key });
                }
            }
        }
        assert!(needed.len() <= super::READINGS_MAX);

        Ok(needed)
    }

    pub(crate) fn visit_attribution_replay(
        &self,
        replay: &mut AttributionReplay,
        mut consume: impl FnMut(&Frame, Option<&Frame>, [Option<WindowIdentity>; 2]) -> Result<()>,
        mut pin: impl FnMut(ResidentPin) -> Result<()>,
    ) -> Result<()> {
        let mut projection = Projection {
            previous: None,
            reading: None,
        };
        let mut index = 0;
        for _batch in 0..super::READINGS_MAX {
            self.operation.check()?;
            if index == replay.readings.len() {
                self.attribution_pins(&replay.restore, &mut pin)?;
                return self.attribution_replay_validate(replay);
            }
            let count = replay.readings[index..]
                .iter()
                .take_while(|reading| reading.window == replay.readings[index].window)
                .count();
            let end = index + count;
            self.attribution_batch(
                &mut replay.restore,
                &replay.readings[index..end],
                &mut projection,
                &mut consume,
                &mut pin,
            )?;
            index = end;
        }

        Err(Error::Bound("attribution saved batches"))
    }

    fn attribution_replay_validate(&self, replay: &AttributionReplay) -> Result<()> {
        self.operation.check()?;
        for (window, _source) in replay.restore.sources.keys() {
            self.operation.check()?;
            self.validate_commitment(*window)?;
        }
        for source in replay.restore.sources.values() {
            self.operation.check()?;
            source.refresh(&self.connection)?;
        }

        self.operation.check()
    }
}

impl AttributionReplay {
    pub(crate) fn count(&self) -> usize {
        self.context.len()
    }
}
