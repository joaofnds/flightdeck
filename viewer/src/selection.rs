//! A selection's chart points travel apart from its other fields, each behind an `Arc`, so a
//! later reading can share the point of a second with the reading before it.
use std::ops::Index;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::byte_budget::ByteReservation;

/// The key of a selection's chart points in its JSON, which `Selection` holds as `Trends`.
const TRENDS_KEY: &str = "trends";
/// The key of a chart point's segment in its JSON, which `TrendPoint` holds beside the point.
const SEGMENT_KEY: &str = "segment";

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;

/// A selection: its chart points as `Trends`, and every other field as JSON. Index it by key as
/// the JSON object it was, except for `"trends"`, which only `trends` reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    fields: Value,
    trends: Option<Trends>,
}

impl Selection {
    /// The selection of a page with no reading, whose every field reads as null.
    pub const NULL: &'static Self = &Self {
        fields: Value::Null,
        trends: None,
    };

    /// The selection of `value`, which takes its chart points out of the JSON and each point's
    /// segment out of the point. A value with no `"trends"` key has no chart points, which is
    /// apart from chart points that are an empty list. It moves the fields and the points'
    /// content rather than copying them, and the order of keys is not kept.
    #[must_use]
    pub fn from_json(mut value: Value) -> Self {
        let trends = value
            .as_object_mut()
            .and_then(|object| object.remove(TRENDS_KEY))
            .map(Trends::from_json);

        Self {
            fields: value,
            trends,
        }
    }

    /// The selection of the fields `fields`, which have no `"trends"` key, and the chart points
    /// `trends`, as the Store builds one.
    pub(crate) fn of(fields: Map<String, Value>, trends: Trends) -> Self {
        assert!(!fields.contains_key(TRENDS_KEY));

        Self {
            fields: Value::Object(fields),
            trends: Some(trends),
        }
    }

    /// The JSON the selection was made from, its chart points and their segments back in it,
    /// equal to that JSON as `Value` compares it, which ignores the order of keys. It copies
    /// every point, so only output and tests call it.
    #[must_use]
    pub fn json(&self) -> Value {
        let mut value = self.fields.clone();
        let Some(trends) = &self.trends else {
            return value;
        };
        let Some(object) = value.as_object_mut() else {
            unreachable!("A selection with chart points is a JSON object");
        };
        let replaced = object.insert(TRENDS_KEY.to_owned(), trends.json());
        assert!(
            replaced.is_none(),
            "A selection's chart points were written into its fields"
        );

        value
    }

    /// The selection's chart points, or `None` where its JSON had no `"trends"` key.
    #[must_use]
    pub fn trends(&self) -> Option<&Trends> {
        self.trends.as_ref()
    }

    /// Takes the selection's chart points out of it, which leaves it with none.
    pub fn take_trends(&mut self) -> Option<Trends> {
        self.trends.take()
    }

    /// The field `key` of the selection, as `Value::get` reads it.
    #[must_use]
    #[track_caller]
    pub fn get(&self, key: &str) -> Option<&Value> {
        selection_key_assert(key);

        self.fields.get(key)
    }

    #[must_use]
    pub fn is_null(&self) -> bool {
        self.fields.is_null()
    }

    /// The selection's fields other than its chart points, where the selection is a JSON
    /// object, for an edit that adds or removes fields.
    pub fn fields_mut(&mut self) -> Option<SelectionFields<'_>> {
        let fields = self.fields.as_object_mut()?;

        Some(SelectionFields { fields })
    }

    /// The chart points as the JSON list they were made from, or null where the selection has
    /// none, as a test reads them.
    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn trends_json(&self) -> Value {
        self.trends().map_or(Value::Null, Trends::json)
    }

    /// Edits the chart points of a fixture's selection as their JSON list, which `edit` may
    /// replace, since a shared point cannot be edited where it is.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn trends_edit(&mut self, edit: impl FnOnce(&mut Value)) {
        let mut points = self
            .trends
            .take()
            .map_or(Value::Null, |trends| trends.json());
        edit(&mut points);
        self.trends = Some(Trends::from_json(points));
    }
}

/// The fields of a selection, which refuse `"trends"` as a key, since a selection holds its
/// chart points apart from its fields, so reading or writing them here would miss them.
pub struct SelectionFields<'selection> {
    fields: &'selection mut Map<String, Value>,
}

impl SelectionFields<'_> {
    #[track_caller]
    pub fn insert(&mut self, key: String, value: Value) {
        selection_key_assert(&key);

        drop(self.fields.insert(key, value));
    }

    #[track_caller]
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        selection_key_assert(key);

        self.fields.remove(key)
    }

    /// Adds every field of `map`, as `Map::extend` does.
    #[track_caller]
    pub fn extend(&mut self, map: Map<String, Value>) {
        for (key, value) in map {
            self.insert(key, value);
        }
    }
}

/// Refuses `"trends"` as a field of a selection, since its chart points are not among its
/// fields and reading them there would read nothing.
#[track_caller]
fn selection_key_assert(key: &str) {
    assert!(
        key != TRENDS_KEY,
        "A selection's chart points are read through Selection::trends"
    );
}

impl Index<&str> for Selection {
    type Output = Value;

    #[track_caller]
    fn index(&self, key: &str) -> &Value {
        selection_key_assert(key);

        &self.fields[key]
    }
}

#[cfg(any(test, feature = "fixtures"))]
impl std::ops::IndexMut<&str> for Selection {
    #[track_caller]
    fn index_mut(&mut self, key: &str) -> &mut Value {
        selection_key_assert(key);

        &mut self.fields[key]
    }
}

#[cfg(any(test, feature = "fixtures"))]
impl PartialEq<Value> for Selection {
    fn eq(&self, other: &Value) -> bool {
        self.json() == *other
    }
}

/// A selection's chart points in time order, each shared behind an `Arc` with its segment
/// beside it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trends {
    points: Vec<TrendPoint>,
}

impl Trends {
    /// The chart points of the JSON list `value`, each point's segment taken out of the point.
    #[must_use]
    pub fn from_json(value: Value) -> Self {
        let Value::Array(points) = value else {
            unreachable!("A selection's chart points are a JSON list");
        };

        Self {
            points: points.into_iter().map(TrendPoint::from_json).collect(),
        }
    }

    /// The chart points `points`, in time order, as the Store builds them.
    pub(crate) fn of(points: Vec<TrendPoint>) -> Self {
        Self { points }
    }

    /// The JSON list of the points, each with its segment back where it stood.
    #[must_use]
    pub fn json(&self) -> Value {
        Value::Array(self.points.iter().map(TrendPoint::json).collect())
    }

    /// The bytes the list of points takes, apart from the points it shares.
    pub(crate) fn list_bytes(&self) -> usize {
        self.points.capacity() * size_of::<TrendPoint>()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, TrendPoint> {
        self.points.iter()
    }

    #[must_use]
    pub fn first(&self) -> Option<&TrendPoint> {
        self.points.first()
    }

    #[must_use]
    pub fn last(&self) -> Option<&TrendPoint> {
        self.points.last()
    }

    #[must_use]
    pub fn as_slice(&self) -> &[TrendPoint] {
        &self.points
    }
}

// Clippy requires a type with `iter` to iterate by reference in a `for` loop as well.
impl<'trends> IntoIterator for &'trends Trends {
    type Item = &'trends TrendPoint;
    type IntoIter = std::slice::Iter<'trends, TrendPoint>;

    fn into_iter(self) -> Self::IntoIter {
        self.points.iter()
    }
}

impl Index<usize> for Trends {
    type Output = TrendPoint;

    fn index(&self, index: usize) -> &TrendPoint {
        &self.points[index]
    }
}

/// One chart point without its segment, which depends on the points beside it, and the segment.
#[derive(Clone, Debug, PartialEq)]
pub struct TrendPoint {
    point: Arc<ChartPoint>,
    segment: Option<u64>,
}

impl TrendPoint {
    /// The point of `value` and its segment, moved into one `Arc` without copying the point. A
    /// point with no `"segment"` key has no segment, as a fixture's point may, and a reader that
    /// needs one refuses it as it did the JSON.
    fn from_json(mut value: Value) -> Self {
        let Some(point) = value.as_object_mut() else {
            unreachable!("A chart point is a JSON object");
        };
        let segment = point.remove(SEGMENT_KEY).map(|segment| {
            let Some(segment) = segment.as_u64() else {
                unreachable!("A chart point's segment is an integer");
            };
            segment
        });

        Self {
            point: Arc::new(ChartPoint {
                value,
                admission: None,
            }),
            segment,
        }
    }

    /// The shared point `point` at the segment `segment`, as the Store numbered it.
    pub(crate) fn shared(point: Arc<ChartPoint>, segment: u64) -> Self {
        Self {
            point,
            segment: Some(segment),
        }
    }

    /// The point, which has no `"segment"` key, since `segment` gives it.
    #[must_use]
    pub fn point(&self) -> &Value {
        &self.point.value
    }

    #[must_use]
    pub fn segment(&self) -> Option<u64> {
        self.segment
    }

    /// The point's JSON with its segment back in it, equal to the JSON it was made from as
    /// `Value` compares it.
    #[must_use]
    pub fn json(&self) -> Value {
        let mut value = self.point.value.clone();
        let Some(segment) = self.segment else {
            return value;
        };
        let Some(point) = value.as_object_mut() else {
            unreachable!("A chart point is a JSON object");
        };
        point.insert(SEGMENT_KEY.to_owned(), Value::from(segment));

        value
    }

    /// The bytes the point's own reservation charges, where the Store built the point.
    pub(crate) fn charge(&self) -> Option<usize> {
        self.point.charge()
    }

    /// The bytes the point's JSON takes once written out with its segment in it, where the
    /// Store built the point.
    pub(crate) fn serialized_bytes(&self) -> Option<usize> {
        let admission = self.point.admission.as_ref()?;
        let segment = self.segment?;

        Some(admission.serialized_bytes + SEGMENT_SERIALIZED_BYTES + digit_count(segment))
    }

    /// Whether the point and `other` share one allocation, as two readings share the point of
    /// a second the Store built once.
    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn shares_point_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.point, &other.point)
    }

    #[cfg(test)]
    pub(crate) fn shared_point(&self) -> &Arc<ChartPoint> {
        &self.point
    }
}

/// The bytes a point's segment adds to its JSON once written out, past the digits of its
/// number: the comma before it, its quoted key and the colon.
const SEGMENT_SERIALIZED_BYTES: usize = ",\"segment\":".len();
const _: () = assert!(SEGMENT_SERIALIZED_BYTES == 1 + SEGMENT_KEY.len() + 2 + 1);

/// The count of decimal digits of `number`.
fn digit_count(number: u64) -> usize {
    match number.checked_ilog10() {
        Some(log) => {
            usize::try_from(log).unwrap_or_else(|_| unreachable!("a count of digits fits a usize"))
                + 1
        }
        None => 1,
    }
}

/// The chart point of one second, without its segment, which every reading of the second
/// shares. A point the Store built holds its own charge and its size written out, so the budget
/// counts it for as long as anything holds it. A point split from a JSON value is charged and
/// bounded with that value.
pub(crate) struct ChartPoint {
    value: Value,
    admission: Option<PointAdmission>,
}

struct PointAdmission {
    reservation: ByteReservation,
    serialized_bytes: usize,
}

impl ChartPoint {
    /// The bytes a shared point takes outside its JSON's content, which the reading that holds
    /// it is charged: the point and the `Arc`'s two counts.
    pub(crate) const HEADER_BYTES: usize = size_of::<Self>() + 2 * size_of::<usize>();

    /// The point `value`, without its segment, charged by `reservation`, which `serialized_bytes`
    /// it takes once written out.
    pub(crate) fn admitted(
        value: Value,
        reservation: ByteReservation,
        serialized_bytes: usize,
    ) -> Self {
        assert!(value.get(SEGMENT_KEY).is_none());

        Self {
            value,
            admission: Some(PointAdmission {
                reservation,
                serialized_bytes,
            }),
        }
    }

    /// The bytes the point's own reservation charges, where the Store built the point.
    pub(crate) fn charge(&self) -> Option<usize> {
        self.admission
            .as_ref()
            .map(|admission| admission.reservation.bytes())
    }

    /// The point, which has no `"segment"` key.
    pub(crate) fn point(&self) -> &Value {
        &self.value
    }
}

/// Two points are equal where they hold the same JSON, whatever charges them, since a reading
/// compares what it shows.
impl PartialEq for ChartPoint {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

/// A point prints its JSON alone, since its reservation would print the budget's live counts,
/// and the read benchmark compares readings by how they print.
impl std::fmt::Debug for ChartPoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ChartPoint")
            .field(&self.value)
            .finish()
    }
}
