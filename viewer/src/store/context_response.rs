//! Compatibility JSON is a bounded presentation of one immutable accepted context.
use std::sync::Arc;

use collector::recording::{identity_text, parse_identity};
use collector::ring::Identity;
use persistence::{Error, Result};
use serde_json::{Value, json};

use super::native::Origin;
use super::response::{JSON_BYTES_MAX, json_charge};
use super::{ByteReservation, QueryContext, QuerySecond, Response};
use crate::Cancellation;
use crate::live_app_response::Observation;
use crate::session::ContextRange;

/// Room for the selection's summary point and the alternatives at its second, which also holds
/// the chart's next point while it is built, before it is charged.
const POINT_BYTES_MAX: usize = 8 * 1024 * 1024;
/// The chart's points take what one JSON value may hold beside the summary and alternatives,
/// since a selection is built as one.
const CONTEXT_BYTES_MAX: usize = JSON_BYTES_MAX - POINT_BYTES_MAX;

#[cfg(test)]
#[path = "context_response_tests.rs"]
pub(super) mod tests;

impl QueryContext {
    #[must_use]
    pub fn catalog(&self) -> Value {
        let catalog = self.catalog_metadata();
        let first = self
            .seconds()
            .iter()
            .filter_map(|second| second.observation.wall_ns)
            .map(|wall| wall / 1_000_000_000)
            .chain(catalog.and_then(|value| value.first_known_second))
            .min();
        let last = self
            .seconds()
            .iter()
            .filter_map(|second| second.observation.wall_ns)
            .map(|wall| wall / 1_000_000_000)
            .chain(catalog.and_then(|value| value.last_known_second))
            .max();
        let (latest, origin) = self.latest();

        json!({"first_second":first,"last_second":last,
            "windows":catalog.map(|value| value.windows),
            "unknown_clock_ticks":catalog.and_then(|value| value.unknown_clock_ticks),
            "live":true,"latest_observation":identity(latest, origin),
            "collector_completed":self.source_status().map(|status| status.completed),
            "source_generation":self.source.generation})
    }

    /// Whether the context holds a reading of `second`, which a chart's older readings lie past.
    #[must_use]
    pub fn retains_second(&self, second: u64) -> bool {
        self.seconds()
            .iter()
            .any(|entry| super::selection::at_second(&entry.observation, second))
    }

    pub fn observation_for_identity(&self, second: u64, value: &Value) -> Result<Observation> {
        let run = parse_identity(value["run"].as_str().ok_or(Error::Bound("selected run"))?)?;
        let boot = parse_identity(
            value["boot"]
                .as_str()
                .ok_or(Error::Bound("selected boot"))?,
        )?;
        let publication = value["publication"]
            .as_u64()
            .ok_or(Error::Bound("selected publication"))?;
        let selected = self
            .seconds()
            .iter()
            .find(|entry| {
                let observation = &entry.observation;
                observation.stamp.run == run.0
                    && observation.stamp.boot == boot.0
                    && observation.stamp.publication == publication
                    && super::selection::at_second(observation, second)
            })
            .ok_or(Error::Bound("selected observation is not retained"))?;
        for (name, expected) in [
            ("run_tick", Some(selected.observation.run_tick)),
            ("wall_ns", selected.observation.wall_ns),
            ("continuous_ns", selected.observation.stamp.continuous_ns),
            ("awake_ns", selected.observation.stamp.awake_ns),
        ] {
            if let Some(supplied) = value.get(name) {
                if supplied.as_u64() != expected {
                    return Err(Error::Bound("changed selected clock"));
                }
            }
        }

        Ok(selected.observation)
    }

    pub fn selection(
        &self,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Value>> {
        self.selection_for(&self.selected().observation, false, cancellation, epoch)
    }

    pub fn selection_for(
        &self,
        observation: &Observation,
        keep_context: bool,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Value>> {
        let operation = Arc::clone(cancellation).operation(epoch)?;
        operation.check()?;
        let selected = self
            .inspect(observation)
            .map_err(|_| Error::Bound("selected observation"))?;
        let mut reservation = self.reserve_response(POINT_BYTES_MAX)?;
        let selected_identity = identity(observation, selected.apps().metadata.origin);
        if keep_context {
            let mut summary = point(selected);
            let object = summary.as_object_mut().ok_or(Error::Bound("point shape"))?;
            object.remove("segment");

            return Response::json(
                reservation,
                json!({"second":observation.wall_ns
                .map(|wall| wall / 1_000_000_000), "selected":selected_identity,
                "summary":summary}),
            );
        }
        if *observation != self.selected().observation {
            return Err(Error::Bound("selection context owner"));
        }
        let mut trends = self.trends(&mut reservation, &operation)?;
        crate::metrics::context_segments(&mut trends)?;
        let summary = trends
            .iter()
            .find(|value| matches_point(value, &selected_identity))
            .ok_or(Error::Bound("selected context point"))?
            .clone();
        let alternatives: Vec<_> = self
            .seconds()
            .iter()
            .filter(|second| {
                super::selection::at_second(&second.observation, self.selected_second())
            })
            .map(|second| identity(&second.observation, second.apps().metadata.origin))
            .collect();
        operation.check()?;

        Response::json(
            reservation,
            json!({"second":self.selected_second(),
            "context_last_second":self.context_last_second(),"selected":selected_identity,
            "alternatives":alternatives,
            "summary":summary,"trends":trends,"context_seconds":900,"context_error":null}),
        )
    }

    /// The chart's points, each charged to `reservation` once built, so a chart takes room for
    /// the points it holds rather than for the most it may hold.
    fn trends(
        &self,
        reservation: &mut ByteReservation,
        operation: &crate::cancellation::Operation,
    ) -> Result<Vec<Value>> {
        let range = if self.followed() {
            ContextRange::charted_ending_at(self.context_last_second())?
        } else {
            if self.has_committed_history() {
                ContextRange::charted_ending_at(self.context_last_second())?
            } else {
                ContextRange::ending_at(self.context_last_second())?
            }
        };
        let mut bytes = self.seconds().len() * size_of::<Value>();
        trends_charge(reservation, bytes)?;
        let mut points = Vec::with_capacity(self.seconds().len());
        for second in self.seconds() {
            operation.check()?;
            if second
                .observation
                .wall_ns
                .is_some_and(|wall| (range.first_ns..range.end_ns).contains(&wall))
            {
                let value = point(second);
                let charge = json_charge(&value)?;
                bytes = bytes
                    .checked_add(charge)
                    .ok_or(Error::Bound("context presentation bytes"))?;
                if bytes > CONTEXT_BYTES_MAX {
                    return Err(Error::Bound("context presentation bytes"));
                }
                trends_charge(reservation, charge)?;
                points.push(value);
            }
        }

        Ok(points)
    }
}

fn trends_charge(reservation: &mut ByteReservation, bytes: usize) -> Result<()> {
    if !reservation.grow(bytes) {
        return Err(Error::Bound("Store presentation admission bytes"));
    }

    Ok(())
}

pub(super) fn identity(observation: &Observation, origin: Origin) -> Value {
    let mut value = json!({"run":identity_text(Identity(observation.stamp.run)).to_string(),
        "boot":identity_text(Identity(observation.stamp.boot)).to_string(),
        "publication":observation.stamp.publication,"run_tick":observation.run_tick,
        "wall_ns":observation.wall_ns,"window_losses":null});
    match origin {
        Origin::Unknown => value["reader_losses"] = Value::Null,
        Origin::Live(losses) => value["reader_losses"] = losses_json(losses),
        Origin::Saved(losses) => value["window_losses"] = losses_json(losses),
    }

    value
}

fn point(second: &QuerySecond) -> Value {
    let machine = second.machine();
    let mut value = match machine.complete() {
        Some(complete) => complete.summary(machine),
        None => crate::metrics::point::Point::default().json(
            &crate::session::Observation {
                window: 0,
                publication: machine.publication,
                run_tick: machine.run_tick,
                wall: machine.wall_ns,
                continuous: machine.continuous_ns,
            },
            machine.segment,
        ),
    };
    value["run"] = json!(identity_text(Identity(second.observation.stamp.run)).to_string());
    value["boot"] = json!(identity_text(Identity(second.observation.stamp.boot)).to_string());

    value
}

fn matches_point(value: &Value, observation: &Value) -> bool {
    value["publication"] == observation["publication"]
        && value["run"] == observation["run"]
        && value["boot"] == observation["boot"]
}

fn losses_json(losses: persistence::Losses) -> Value {
    json!({"publications":losses.publications,"events":losses.events,"unknown":losses.unknown})
}
