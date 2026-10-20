use super::{BTreeMap, Frame, Key, Observation, Reading, Restore, Session, Source};
use crate::app_process_groups_history::AppProcessGroupsHistory;
use crate::app_process_groups_history::cache::{Cache, Point};
use crate::live_app_history::{PIN_BYTES_MAX, ResidentFile, ResidentPin};
use persistence::{Error, Result};

pub(crate) struct Replay {
    restore: Restore,
    readings: Vec<Reading>,
    context: Vec<Key>,
}

struct Projection {
    previous: Option<Box<Frame>>,
    reading: Option<Reading>,
}

impl Session {
    pub(crate) fn with_process_groups_replay(
        &self,
        selected: &Observation,
        consume: impl FnOnce(&mut Replay) -> Result<Box<AppProcessGroupsHistory>>,
    ) -> Result<Box<AppProcessGroupsHistory>> {
        self.operation.attach(&self.connection)?;
        self.operation.check()?;
        if collector::recording::read_identity(&self.root)?.0 != selected.recording {
            return Err(Error::Bound("process cohort saved recording"));
        }

        let range = self.app_history_range(selected)?;
        let mut restore = Restore {
            readings: BTreeMap::new(),
            sources: BTreeMap::new(),
            source_bytes: 0,
            plotted: 0,
        };

        self.app_history_gather(&range, &mut restore)?;
        let context = restore.readings.keys().copied().collect();
        self.app_history_predecessors(&mut restore)?;
        let readings = Vec::with_capacity(super::READINGS_MAX);
        let mut replay = Replay {
            restore,
            readings,
            context,
        };

        self.process_groups_validate(&replay)?;

        let result = consume(&mut replay)?;
        self.process_groups_validate(&replay)?;

        Ok(result)
    }

    pub(crate) fn visit_process_groups(
        &self,
        replay: &mut Replay,
        mut consume: impl FnMut(&Frame, Option<&Frame>, Option<&Observation>) -> Result<()>,
    ) -> Result<()> {
        let mut projection = Projection {
            previous: None,
            reading: None,
        };

        let mut index = 0;
        for _batch in 0..super::READINGS_MAX {
            self.operation.check()?;
            if index == replay.readings.len() {
                return self.process_groups_validate(replay);
            }

            let count = replay.readings[index..]
                .iter()
                .take_while(|reading| reading.window == replay.readings[index].window)
                .count();
            let end = index + count;
            self.visit_process_groups_batch(replay, index..end, &mut projection, &mut consume)?;
            index = end;
        }

        Err(Error::Bound("process cohort saved batches"))
    }

    fn visit_process_groups_batch(
        &self,
        replay: &mut Replay,
        range: std::ops::Range<usize>,
        projection: &mut Projection,
        consume: &mut impl FnMut(&Frame, Option<&Frame>, Option<&Observation>) -> Result<()>,
    ) -> Result<()> {
        let readings = &replay.readings[range];
        if readings.len() > super::TICKS_PER_WINDOW_MAX {
            return Err(Error::Bound("process cohort saved endpoints"));
        }

        let first = readings
            .first()
            .ok_or(Error::Bound("process cohort empty batch"))?;
        self.validate_commitment(first.window)?;
        let predecessors: Vec<_> = readings
            .iter()
            .map(|reading| {
                let key = Key::new(&reading.observation);
                key.publication
                    .checked_sub(1)
                    .and_then(|publication| {
                        replay.restore.readings.get(&Key { publication, ..key })
                    })
                    .map(|reading| reading.observation)
            })
            .collect();
        let endpoints: Vec<_> = readings.iter().map(Reading::endpoint).collect();
        let source = self.app_history_source(&mut replay.restore, first.window, Source::Apps)?;
        let mut index = 0;
        Frame::committed_many(
            &endpoints,
            &self.connection,
            source,
            &self.operation,
            |frame| {
                #[cfg(test)]
                FRAME_DECODES.with(|count| count.set(count.get() + 1));
                let reading = readings
                    .get(index)
                    .ok_or(Error::Bound("process cohort batch association"))?;
                if frame.stamp() != &reading.observation.stamp {
                    return Err(Error::Bound("process cohort saved association"));
                }

                self.visit_process_groups_batch_capture(
                    projection,
                    reading,
                    frame,
                    predecessors[index].as_ref(),
                    consume,
                )?;
                index += 1;

                Ok(())
            },
        )?;
        assert_eq!(index, readings.len());
        self.validate_commitment(first.window)
    }

    fn visit_process_groups_batch_capture(
        &self,
        projection: &mut Projection,
        reading: &Reading,
        current: Box<Frame>,
        predecessor: Option<&Observation>,
        consume: &mut impl FnMut(&Frame, Option<&Frame>, Option<&Observation>) -> Result<()>,
    ) -> Result<()> {
        if reading.plotted {
            let previous = super::sample_predecessor(
                projection.reading.as_ref(),
                projection.previous.as_deref(),
                reading,
                &current,
            );

            consume(&current, previous, predecessor)?;
            #[cfg(test)]
            if let Some(checkpoint) = self.after_process_groups_frame.borrow_mut().take() {
                checkpoint();
            }

            self.operation.check()?;
        }

        projection.previous = Some(current);
        projection.reading = Some(*reading);

        Ok(())
    }

    fn process_groups_validate(&self, replay: &Replay) -> Result<()> {
        self.operation.check()?;
        for (window, _source) in replay.restore.sources.keys() {
            self.validate_commitment(*window)?;
        }

        for source in replay.restore.sources.values() {
            source.refresh(&self.connection)?;
        }

        self.operation.check()
    }
}

#[cfg(test)]
thread_local! {
    static FRAME_DECODES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
impl Replay {
    pub(crate) fn test_frame_decodes() -> u64 {
        FRAME_DECODES.with(std::cell::Cell::get)
    }
}

impl Session {
    pub(crate) fn process_groups_plan(
        &self,
        replay: &mut Replay,
        mut excluded: impl FnMut(&Observation) -> Result<bool>,
    ) -> Result<()> {
        for reading in replay.restore.readings.values_mut() {
            reading.plotted = false;
        }

        for key in &replay.context {
            self.operation.check()?;
            let reading = replay
                .restore
                .readings
                .get_mut(key)
                .ok_or(Error::Bound("process group context association"))?;
            reading.plotted = !excluded(&reading.observation)?;
        }

        replay.readings.clear();
        for (key, reading) in &replay.restore.readings {
            self.operation.check()?;
            let predecessor_needed = key
                .publication
                .checked_add(1)
                .and_then(|publication| {
                    replay.restore.readings.get(&Key {
                        publication,
                        ..*key
                    })
                })
                .is_some_and(|successor| successor.plotted);
            if !reading.plotted {
                if !predecessor_needed {
                    continue;
                }
            }

            if replay.readings.len() == super::READINGS_MAX {
                return Err(Error::Bound("process group replay endpoints"));
            }

            replay.readings.push(*reading);
        }

        Ok(())
    }

    pub(crate) fn process_groups_cache_needed(
        &self,
        replay: &Replay,
        point: &Point,
    ) -> Result<bool> {
        self.operation.check()?;
        let key = Key::new(&point.observation);
        if replay.context.binary_search(&key).is_err() {
            return Ok(false);
        }

        let reading = replay
            .restore
            .readings
            .get(&key)
            .ok_or(Error::Bound("process group cache source observation"))?;
        if reading.observation != point.observation {
            return Err(Error::Bound("process group cache observation conflict"));
        }

        let predecessor = key
            .publication
            .checked_sub(1)
            .and_then(|publication| replay.restore.readings.get(&Key { publication, ..key }))
            .map(|reading| &reading.observation);

        Ok(predecessor == point.predecessor.as_ref())
    }

    pub(crate) fn process_groups_cache_validate(&self, cache: &Cache) -> Result<()> {
        for pin in cache.pins() {
            self.operation.check()?;
            let window = self
                .windows
                .iter()
                .position(|manifest| manifest.identity == pin.manifest.identity)
                .ok_or(Error::Bound("process group cache commitment missing"))?;
            if self.windows[window] != pin.manifest {
                return Err(Error::Bound("process group cache commitment changed"));
            }

            self.validate_commitment(window)?;
            for file in &pin.files {
                if self.connection.parquet_source(&file.path)? != file.alias {
                    return Err(Error::Bound("changed committed App history source"));
                }
            }
        }

        self.operation.check()
    }

    pub(crate) fn process_groups_cache_pins(
        &self,
        replay: &Replay,
        cache: &mut Cache,
        available: usize,
    ) -> Result<bool> {
        for (window, source) in replay.restore.sources.keys() {
            self.operation.check()?;
            if !matches!(source, Source::Apps) {
                continue;
            }

            let Some(pin) = self.process_groups_cache_pins_pin(replay, *window)? else {
                return Ok(false);
            };

            if !cache.pin(pin, available) {
                return Ok(false);
            }
        }

        Ok(true)
    }

    fn process_groups_cache_pins_pin(
        &self,
        replay: &Replay,
        window: usize,
    ) -> Result<Option<Box<ResidentPin>>> {
        let mut pin = Box::new(ResidentPin {
            manifest: self.windows[window].clone(),
            files: Vec::with_capacity(3),
        });

        for source in [Source::Apps, Source::Ticks] {
            let source = replay
                .restore
                .sources
                .get(&(window, source))
                .ok_or(Error::Bound("process group cache source pin"))?;
            for file in &source.files {
                pin.files.push(ResidentFile {
                    path: file.path.clone(),
                    alias: file.alias.clone(),
                });
            }
        }

        if pin.bytes() > PIN_BYTES_MAX {
            return Ok(None);
        }

        Ok(Some(pin))
    }
}
