//! Compatibility JSON is a bounded presentation of one immutable accepted context.
use std::sync::Arc;

use collector::recording::{identity_text, parse_identity};
use collector::ring::Identity;
use persistence::{Error, Result};
use serde_json::{Map, Value, json};

use super::native::Origin;
use super::response::{JSON_BYTES_MAX, json_charge};
use super::{ByteReservation, ChartPoints, QueryContext, QuerySecond, Response};
use crate::live_app_response::{Observation, RESPONSE_BYTES_MAX, serialized_bytes};
use crate::session::ContextRange;
use crate::{Cancellation, ChartPoint, ObservationKey, Selection, TrendPoint, Trends};

/// Room for the selection's summary point and the alternatives at its second, which also holds
/// the chart's next point while it is built, before it is charged.
const POINT_BYTES_MAX: usize = 8 * 1024 * 1024;
/// The chart's points take what one JSON value may hold beside the summary and alternatives,
/// since a selection is built as one.
const CONTEXT_BYTES_MAX: usize = JSON_BYTES_MAX - POINT_BYTES_MAX;
/// The chart counts its list of points against its bound as a list of JSON points.
const _: () = assert!(size_of::<TrendPoint>() <= size_of::<Value>());

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

    /// The selection of the selected second, which shares no chart point with another reading.
    pub fn selection(
        &self,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Selection>> {
        let observation = self.selected().observation;

        self.selection_for(
            &observation,
            false,
            &mut ChartPoints::default(),
            cancellation,
            epoch,
        )
    }

    /// The selection of `observation`. A selection that keeps the context the window holds has
    /// no chart points and leaves `chart_points` as it was. Any other shares each chart point
    /// `chart_points` holds of a second whose complete reading the point was built from, builds
    /// the rest, and leaves `chart_points` holding the points of its chart alone. A reading that
    /// fails leaves `chart_points` as it was.
    pub fn selection_for(
        &self,
        observation: &Observation,
        keep_context: bool,
        chart_points: &mut ChartPoints,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Selection>> {
        let operation = Arc::clone(cancellation).operation(epoch)?;
        operation.check()?;
        let selected = self
            .inspect(observation)
            .map_err(|_| Error::Bound("selected observation"))?;
        let mut reservation = self.reserve_response(POINT_BYTES_MAX)?;
        let selected_identity = identity(observation, selected.apps().origin());
        if keep_context {
            let mut summary = point(selected);
            let object = summary.as_object_mut().ok_or(Error::Bound("point shape"))?;
            object.remove("segment");
            let response = Response::json(
                reservation,
                json!({"second":observation.wall_ns
                .map(|wall| wall / 1_000_000_000), "selected":selected_identity,
                "summary":summary}),
            )?;

            return Ok(response.map(Selection::from_json));
        }
        if *observation != self.selected().observation {
            return Err(Error::Bound("selection context owner"));
        }
        let (trends, kept) = self.trends(chart_points, &mut reservation, &operation)?;
        let summary = trends
            .iter()
            .find(|point| matches_point(point.point(), &selected_identity))
            .ok_or(Error::Bound("selected context point"))?
            .json();
        let alternatives: Vec<_> = self
            .seconds()
            .iter()
            .filter(|second| {
                super::selection::at_second(&second.observation, self.selected_second())
            })
            .map(|second| identity(&second.observation, second.apps().origin()))
            .collect();
        operation.check()?;
        let fields = object([
            ("second", json!(self.selected_second())),
            ("context_last_second", json!(self.context_last_second())),
            ("selected", selected_identity),
            ("alternatives", Value::Array(alternatives)),
            ("summary", summary),
            ("context_seconds", json!(900)),
            ("context_error", Value::Null),
        ]);
        let response = selection_for_response(reservation, fields, trends)?;
        *chart_points = kept;

        Ok(response)
    }

    /// The chart's points and the ones to keep for the next reading. A point `chart_points`
    /// holds of a second whose complete reading it was built from is shared, and the rest are
    /// built, each charged to a reservation of its own. `reservation` is charged for the list
    /// of points and for each point's header.
    fn trends(
        &self,
        chart_points: &ChartPoints,
        reservation: &mut ByteReservation,
        operation: &crate::cancellation::Operation,
    ) -> Result<(Trends, ChartPoints)> {
        let range = if self.followed() {
            ContextRange::charted_ending_at(self.context_last_second())?
        } else {
            if self.has_committed_history() {
                ContextRange::charted_ending_at(self.context_last_second())?
            } else {
                ContextRange::ending_at(self.context_last_second())?
            }
        };
        assert!(self.seconds().len() <= super::SECONDS_MAX);
        trends_charge(reservation, self.seconds().len() * size_of::<TrendPoint>())?;
        let mut points = Vec::with_capacity(self.seconds().len());
        let mut kept = ChartPoints::default();
        // The chart counts against its bound as the list of JSON points it was before its
        // points were shared, a list at least as large as the list of shared points.
        let mut bytes = self.seconds().len() * size_of::<Value>();
        for second in self.seconds() {
            operation.check()?;
            if second
                .observation
                .wall_ns
                .is_some_and(|wall| (range.first_ns..range.end_ns).contains(&wall))
            {
                let key = ObservationKey::of(&second.observation);
                let complete = second.machine().complete_shared();
                let point = match complete.and_then(|complete| chart_points.get(&key, complete)) {
                    Some(point) => Arc::clone(point),
                    None => self.trends_point(second)?,
                };
                bytes = bytes
                    .checked_add(trends_point_charge(&point))
                    .ok_or(Error::Bound("context presentation bytes"))?;
                if bytes > CONTEXT_BYTES_MAX {
                    return Err(Error::Bound("context presentation bytes"));
                }
                trends_charge(reservation, ChartPoint::HEADER_BYTES)?;
                if let Some(complete) = complete {
                    kept.place(key, complete, &point);
                }
                points.push(point);
            }
        }

        Ok((trends_numbered(points)?, kept))
    }

    /// The chart point of `second`, built now, without its segment, and charged to a
    /// reservation of its own, so it stays charged for as long as any reading holds it. It
    /// charges what it charged as it was built, with its `"segment"` key in it, as a chart's
    /// point charged before the Store shared it: a few bytes more than it holds without the key.
    fn trends_point(&self, second: &QuerySecond) -> Result<Arc<ChartPoint>> {
        let mut value = point(second);
        let charge = json_charge(&value)?;
        let object = value.as_object_mut().ok_or(Error::Bound("point shape"))?;
        object.remove("segment");
        let serialized = serialized_bytes(&value, RESPONSE_BYTES_MAX)?;
        let reservation = self.reserve_response(charge)?;

        Ok(Arc::new(ChartPoint::admitted(
            value,
            reservation,
            serialized,
        )))
    }
}

/// A JSON object of `entries` in their order, which takes each value where `json!` would copy it
/// through `Serialize`, since a selection's chart is the largest value a Store response holds.
fn object<const COUNT: usize>(entries: [(&str, Value); COUNT]) -> Map<String, Value> {
    let mut object = Map::with_capacity(COUNT);
    for (key, value) in entries {
        object.insert(key.to_owned(), value);
    }

    object
}

/// The bytes the selection's chart adds to its JSON once written out, past its points: the
/// comma before its key, the quoted key, the colon and the list's brackets.
const TRENDS_SERIALIZED_BYTES: usize = ",\"trends\":[]".len();

/// The selection of `fields` and `trends`, bounded as the one JSON value it stands for, which
/// is how a selection was bounded when it was built as one: its size written out, and what the
/// fields, the list of points and each point charge together. `reservation` charges the
/// response, the fields, the list of points and each point's header, since each point charges
/// its own content.
fn selection_for_response(
    mut reservation: ByteReservation,
    fields: Map<String, Value>,
    trends: Trends,
) -> Result<Response<Selection>> {
    let fields = Value::Object(fields);
    let serialized = selection_for_serialized_bytes(&fields, &trends)?;
    if serialized > RESPONSE_BYTES_MAX {
        return Err(Error::Bound("live App response bytes"));
    }
    let fields_charge = json_charge(&fields)?;
    selection_for_retained_bytes(fields_charge, &trends)?;
    let bytes = size_of::<Response<Selection>>()
        + fields_charge
        + trends.list_bytes()
        + trends.len() * ChartPoint::HEADER_BYTES;
    if bytes > reservation.bytes() {
        return Err(Error::Bound("Store presentation admission bytes"));
    }
    reservation.shrink_to(bytes);
    let Value::Object(fields) = fields else {
        unreachable!("the selection's fields are the object made of them above");
    };

    Ok(Response::reserved(
        reservation,
        Selection::of(fields, trends),
    ))
}

/// The bytes the selection of `fields` and `trends` takes written out as one JSON value: the
/// fields, the chart's key and brackets, each point with its segment, and a comma between two
/// points.
fn selection_for_serialized_bytes(fields: &Value, trends: &Trends) -> Result<usize> {
    let mut bytes = serialized_bytes(fields, RESPONSE_BYTES_MAX)?
        + TRENDS_SERIALIZED_BYTES
        + trends.len().saturating_sub(1);
    for point in trends {
        let point_bytes = point
            .serialized_bytes()
            .unwrap_or_else(|| unreachable!("the Store builds every chart point it bounds"));
        bytes = bytes
            .checked_add(point_bytes)
            .ok_or(Error::Bound("live App response bytes"))?;
    }

    Ok(bytes)
}

/// Refuses a selection whose fields, which charge `fields_charge`, list of points and points
/// together charge more than `JSON_BYTES_MAX`, as one JSON value of it charged at most that.
/// The count of values and their nesting are bounded for the fields and for each point apart.
fn selection_for_retained_bytes(fields_charge: usize, trends: &Trends) -> Result<()> {
    let mut bytes = fields_charge + trends.list_bytes();
    for point in trends {
        let charge = point
            .charge()
            .unwrap_or_else(|| unreachable!("the Store charges every chart point it builds"));
        bytes = bytes
            .checked_add(charge)
            .ok_or(Error::Bound("Store JSON retained bytes"))?;
    }
    if bytes > JSON_BYTES_MAX {
        return Err(Error::Bound("Store JSON retained bytes"));
    }

    Ok(())
}

/// The chart points `points` in time order, each numbered with its segment by the one rule a
/// chart's segments follow.
fn trends_numbered(points: Vec<Arc<ChartPoint>>) -> Result<Trends> {
    let values: Vec<&Value> = points.iter().map(|point| point.point()).collect();
    let segments = crate::metrics::context_segments(&values)?;

    Ok(Trends::of(
        points
            .into_iter()
            .zip(segments)
            .map(|(point, segment)| TrendPoint::shared(point, segment))
            .collect(),
    ))
}

/// The bytes the point `point`, which the Store built, charges of its own.
fn trends_point_charge(point: &ChartPoint) -> usize {
    point
        .charge()
        .unwrap_or_else(|| unreachable!("the Store charges every chart point it builds"))
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
