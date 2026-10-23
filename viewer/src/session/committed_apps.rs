use super::{Session, WINDOWS_MAX};
use crate::apps::Stamp;
use crate::live_app_response::{Observation, SAMPLE_BYTES_MAX, Sample};
use crate::live_apps::{Frame, committed::Endpoint};
use collector::recording::identity_text;
use collector::ring::Identity;
use persistence::engine::Row;
use persistence::{Error, Result};
use serde_json::Value;

pub(super) struct Cache {
    window: usize,
    predecessor_window: Option<usize>,
    sample: Box<Sample>,
}

struct FrameSource {
    window: usize,
    query: String,
}

// Sample::bytes includes its box payload; Cache retains only fixed metadata and a box pointer.
const CACHE_BYTES_MAX: usize = SAMPLE_BYTES_MAX + size_of::<Cache>();

impl Session {
    pub(crate) fn with_app_sample(
        &self,
        selected: &Value,
        consume: impl FnOnce(&Sample) -> Result<Value>,
    ) -> Result<Value> {
        let result = self.operation.attach(&self.connection).and_then(|()| {
            self.operation.run(|| {
                self.operation.check()?;
                let mut cache = self.app_sample_cache.borrow_mut();
                let hit = cache
                    .as_ref()
                    .is_some_and(|cache| with_app_sample_matches(selected, &cache.sample.selected));

                if hit {
                    let current = cache.as_ref().ok_or(Error::Bound("committed App cache"))?;
                    let valid = self.with_app_sample_validate(current);
                    if let Err(error) = valid {
                        *cache = None;
                        return Err(error);
                    }
                } else {
                    *cache = None;
                    let restored = self.with_app_sample_restore(selected)?;
                    #[cfg(test)]
                    if let Some(checkpoint) = self.before_app_sample_install.borrow_mut().take() {
                        checkpoint();
                    }
                    self.operation.check()?;
                    *cache = Some(restored);
                }

                self.operation.check()?;
                let sample = &cache
                    .as_ref()
                    .ok_or(Error::Bound("committed App cache output"))?
                    .sample;

                consume(sample)
            })
        });

        if result.is_err() {
            *self.app_sample_cache.borrow_mut() = None;
        }

        result
    }

    fn with_app_sample_validate(&self, cache: &Cache) -> Result<()> {
        self.validate_commitment(cache.window)?;
        if let Some(window) = cache.predecessor_window {
            self.validate_commitment(window)?;
        }

        self.operation.check()
    }

    fn with_app_sample_restore(&self, selected: &Value) -> Result<Cache> {
        let publication = selected["publication"]
            .as_u64()
            .ok_or(Error::Bound("committed App selection publication"))?;
        let window = self
            .with_app_sample_window(selected, publication)?
            .ok_or(Error::Bound(
                "committed App selected commitment unavailable",
            ))?;
        let endpoint = self
            .with_app_sample_endpoint(window, publication)?
            .ok_or(Error::Bound("committed App selected tick unavailable"))?;

        with_app_sample_selection(selected, &endpoint)?;
        let mut current = None;
        let mut source = None;
        self.with_app_sample_frame(window, &endpoint, &mut source, &mut current)?;

        let predecessor_window = if let Some(previous) = publication.checked_sub(1) {
            self.with_app_sample_window(selected, previous)?
        } else {
            None
        };
        let mut previous = None;
        if let Some(window) = predecessor_window {
            if let Some(publication) = publication.checked_sub(1) {
                if let Some(endpoint) = self.with_app_sample_endpoint(window, publication)? {
                    if current
                        .as_deref()
                        .is_some_and(|current| current.takes_rates_from(&endpoint))
                    {
                        self.with_app_sample_frame(window, &endpoint, &mut source, &mut previous)?;
                    }
                }
            }
        }

        let mut sample = None;
        Sample::capture(
            current
                .as_deref()
                .ok_or(Error::Bound("committed App frame output"))?,
            previous.as_deref(),
            &mut sample,
        )?;

        let sample = sample.ok_or(Error::Bound("committed App sample output"))?;
        assert!(sample.bytes() <= SAMPLE_BYTES_MAX);
        assert!(sample.bytes() + size_of::<Cache>() <= CACHE_BYTES_MAX);

        Ok(Cache {
            window,
            predecessor_window,
            sample,
        })
    }

    fn with_app_sample_window(&self, selected: &Value, publication: u64) -> Result<Option<usize>> {
        assert!(self.windows.len() <= WINDOWS_MAX);

        let mut found = None;
        for (index, manifest) in self.windows.iter().enumerate() {
            self.operation.check()?;
            if selected["run"] != identity_text(manifest.identity.run.run).to_string() {
                continue;
            }

            if selected["boot"] != identity_text(manifest.identity.run.boot).to_string() {
                continue;
            }

            if (manifest.publication_first..manifest.publication_end).contains(&publication) {
                if found.replace(index).is_some() {
                    return Err(Error::Bound("duplicate committed App commitment"));
                }
            }
        }

        Ok(found)
    }

    fn with_app_sample_endpoint(
        &self,
        window: usize,
        publication: u64,
    ) -> Result<Option<Endpoint>> {
        let source = self.source(window, "ticks")?;
        let frame = self.connection.query(&format!(
            "SELECT run_tick, wall_ns, continuous_ns, awake_ns, processes_refused, \
            processes_gone, events_first, events_end FROM {source} \
            WHERE publication={publication} LIMIT 2"
        ))?;
        if frame.height() == 0 {
            return Ok(None);
        }
        if frame.height() > 1 {
            return Err(Error::Bound("duplicate committed App tick"));
        }
        let row = Row::new(&frame, 0)?;

        let identity = self.windows[window].identity.run;
        let endpoint = Endpoint {
            identity,
            stamp: Stamp {
                boot: identity.boot.0,
                run: identity.run.0,
                publication,
                continuous_ns: row.get(2)?,
                awake_ns: row.get(3)?,
            },
            run_tick: row
                .get::<Option<u64>>(0)?
                .ok_or(Error::Bound("committed App run tick"))?,
            wall_ns: row.get(1)?,
            events_first: with_app_sample_endpoint_event_bound(&row, 6)?,
            events_end: with_app_sample_endpoint_event_bound(&row, 7)?,
            usage_refused: row
                .get::<Option<u32>>(4)?
                .ok_or(Error::Bound("committed App refused count"))?,
            usage_gone: row
                .get::<Option<u32>>(5)?
                .ok_or(Error::Bound("committed App gone count"))?,
        };

        self.operation.check()?;

        Ok(Some(endpoint))
    }

    fn with_app_sample_frame(
        &self,
        window: usize,
        endpoint: &Endpoint,
        source: &mut Option<FrameSource>,
        output: &mut Option<Box<Frame>>,
    ) -> Result<()> {
        if source.as_ref().is_none_or(|source| source.window != window) {
            let processes = self.source(window, "processes")?;
            let events = self.source(window, "events")?;
            *source = Some(FrameSource {
                window,
                query: crate::live_apps::committed::Sources {
                    processes: &processes,
                    events: &events,
                }
                .query(),
            });
        }

        let source = source
            .as_ref()
            .ok_or(Error::Bound("committed App source"))?;
        Frame::committed(
            endpoint,
            &self.connection,
            &source.query,
            &self.operation,
            output,
        )
    }
}

fn with_app_sample_endpoint_event_bound(row: &Row<'_>, index: usize) -> Result<u64> {
    row.get::<Option<u64>>(index)?
        .ok_or(Error::Bound("committed App event range"))
}

fn with_app_sample_matches(selected: &Value, observation: &Observation) -> bool {
    if selected["run"] != identity_text(Identity(observation.stamp.run)).to_string() {
        return false;
    }

    if selected["boot"] != identity_text(Identity(observation.stamp.boot)).to_string() {
        return false;
    }

    if selected["publication"].as_u64() != Some(observation.stamp.publication) {
        return false;
    }

    if selected["run_tick"].as_u64() != Some(observation.run_tick) {
        return false;
    }

    if selected["wall_ns"].as_u64() != observation.wall_ns {
        return false;
    }

    for (field, expected) in [
        ("continuous_ns", observation.stamp.continuous_ns),
        ("awake_ns", observation.stamp.awake_ns),
    ] {
        if let Some(value) = selected.get(field) {
            if value.as_u64() != expected {
                return false;
            }
        }
    }

    true
}

fn with_app_sample_selection(selected: &Value, endpoint: &Endpoint) -> Result<()> {
    if selected["run_tick"].as_u64() != Some(endpoint.run_tick) {
        return Err(Error::Bound("committed App selected run tick"));
    }

    let wall = selected["wall_ns"]
        .as_u64()
        .ok_or(Error::Bound("committed App selected wall time"))?;
    if endpoint.wall_ns != Some(wall) {
        return Err(Error::Bound("committed App selected wall time"));
    }

    for (field, expected) in [
        ("continuous_ns", endpoint.stamp.continuous_ns),
        ("awake_ns", endpoint.stamp.awake_ns),
    ] {
        if let Some(value) = selected.get(field) {
            if value.as_u64() != expected {
                return Err(Error::Bound("committed App selected clock"));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;
