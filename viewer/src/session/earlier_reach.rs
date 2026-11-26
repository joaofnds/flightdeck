//! How far each number of a trend point went over the readings of one stretch, so a chart that
//! draws one reading of the stretch still finds what the others read. A number a chart derives
//! from several takes its reach here too, reading by reading, since the reach of a sum is what the
//! sum read at one instant and not what its parts each reached at another.

use std::collections::BTreeMap;

use collector::recording::{identity_text, parse_identity};
use collector::ring::Identity;
use model::summary::{GaugeSummary, MachinePlace};
use persistence::{Error, Result};
use serde_json::{Map, Value, json};

/// What places a reading rather than measures it, which no chart plots.
const PLACE_FIELDS: [&str; 7] = [
    "publication",
    "run_tick",
    "wall_ns",
    "continuous_ns",
    "awake_ns",
    "segment",
    "observation",
];

/// More than the objects, arrays and numbers one trend point can hold.
const NODES_MAX: usize = 4096;

/// How far one number of a trend point went over the readings of its stretch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumberReach {
    pub low: f64,
    pub high: f64,
    /// The reading that read `high`.
    pub peak: MachinePlace,
}

/// Where a trend point's reach keeps the number `key`, inside `resource` where the point groups it
/// and at `channel` where the number has several.
#[must_use]
pub fn number_path(resource: Option<&str>, key: &str, channel: Option<usize>) -> String {
    match (resource, channel) {
        (Some(resource), Some(channel)) => format!("{resource}/{key}/{channel}"),
        (Some(resource), None) => format!("{resource}/{key}"),
        (None, Some(channel)) => format!("{key}/{channel}"),
        (None, None) => key.to_owned(),
    }
}

/// How far the number at `path` of the trend point's `reach` went, or `None` where the number did
/// not move over its stretch and the point the reach rides on reads it at that value.
///
/// # Errors
///
/// Returns `Error::Bound` when `reach` is not what a trend point carries.
pub fn number_reach(reach: &Value, path: &str) -> Result<Option<NumberReach>> {
    let number = &reach["leaves"][path];
    if number.is_null() {
        return Ok(None);
    }

    let refused = || Error::Bound("trend point reach");
    let at = number[2].as_u64().ok_or_else(refused)?;
    let at = usize::try_from(at).map_err(|_| refused())?;
    let place = &reach["places"][at];

    Ok(Some(NumberReach {
        low: number[0].as_f64().ok_or_else(refused)?,
        high: number[1].as_f64().ok_or_else(refused)?,
        peak: MachinePlace {
            run: number_reach_identity(&place[0])?,
            boot: number_reach_identity(&place[1])?,
            publication: place[2].as_u64().ok_or_else(refused)?,
            wall_ns: place[3].as_u64().ok_or_else(refused)?,
        },
    }))
}

fn number_reach_identity(text: &Value) -> Result<[u64; 2]> {
    let text = text.as_str().ok_or(Error::Bound("trend point reach"))?;

    Ok(parse_identity(text)?.0)
}

/// The reach the older tail writes for a stretch of `readings`, each a trend point with the
/// reading it was taken at, on the point `point`, so a test outside the viewer can write the
/// points the tail writes.
#[cfg(any(test, feature = "fixtures"))]
#[must_use]
pub fn reach_of_readings(readings: &[(Value, MachinePlace)], point: ReachPoint) -> Value {
    let mut reach = Reach::default();
    for (reading, place) in readings {
        reach.add(reading, place);
    }

    reach.json(point)
}

/// The point a reach rides on, which says what a number that did not move over the stretch reads
/// there, so the reach can leave that number out.
#[derive(Clone, Copy, Debug)]
pub enum ReachPoint<'point> {
    /// A point of the older tail, before its reach is attached.
    TailPoint(&'point Value),
    /// The first full-detail point after the join, which lies outside the older tail, so the reach
    /// cannot know its readings and keeps every number.
    AfterJoin,
}

/// The readings of each number, by its path through the point.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Reach {
    leaves: BTreeMap<String, GaugeSummary<MachinePlace>>,
}

impl Reach {
    /// Takes in the reading `point`, taken at `place`.
    pub(super) fn add(&mut self, point: &Value, place: &MachinePlace) {
        let mut pending: Vec<(String, &Value)> = vec![(String::new(), point)];
        for _ in 0..NODES_MAX {
            let Some((path, node)) = pending.pop() else {
                self.add_derived(point, place);
                return;
            };
            match node {
                Value::Number(number) => {
                    if let Some(value) = number.as_f64() {
                        self.take(path, value, place);
                    }
                }
                Value::Array(items) => {
                    let items = items.iter().enumerate();
                    pending.extend(items.map(|(at, item)| (format!("{path}/{at}"), item)));
                }
                Value::Bool(flag) => self.take(path, f64::from(u8::from(*flag)), place),
                Value::Object(fields) => add_fields(&mut pending, &path, fields),
                _ => {}
            }
        }

        panic!("a trend point holds at most {NODES_MAX} values");
    }

    /// Takes in what the charts derive from several numbers of the reading `point`.
    fn add_derived(&mut self, point: &Value, place: &MachinePlace) {
        let bytes = |name: &str| point[name].as_u64();
        let memory = bytes("memory_anonymous_bytes")
            .zip(bytes("memory_purgeable_bytes"))
            .and_then(|(anonymous, purgeable)| anonymous.checked_sub(purgeable));
        if let Some(app) = memory {
            let app_bytes = model::rates::number(app);
            self.take("memory_app_bytes".to_owned(), app_bytes, place);
            let used = bytes("memory_wired_bytes")
                .zip(bytes("memory_compressed_bytes"))
                .and_then(|(wired, compressed)| app.checked_add(wired)?.checked_add(compressed));
            if let Some(used) = used {
                let used_bytes = model::rates::number(used);
                self.take("memory_used_bytes".to_owned(), used_bytes, place);
            }
        }

        let watts = |name: &str| point[name].as_f64();
        if let Some((system, gpu)) = watts("power_watts").zip(watts("gpu_watts")) {
            self.take("power_remainder_watts".to_owned(), system - gpu, place);
        }

        let fans = point["fan_rpm"].as_array().map_or(&[][..], Vec::as_slice);
        let (total, count) = fans
            .iter()
            .filter_map(Value::as_f64)
            .fold((0.0, 0_u32), |(total, count), speed| {
                (total + speed, count + 1)
            });
        if count > 0 {
            // The mean of a few fan speeds, none near where a double rounds.
            self.take("fan_mean_rpm".to_owned(), total / f64::from(count), place);
        }
    }

    /// Takes in `value`, read at `place`. A reach draws no line, so a break between its readings
    /// changes nothing it says.
    fn take(&mut self, path: String, value: f64, place: &MachinePlace) {
        self.leaves
            .entry(path)
            .or_default()
            .push(Some(value), true, *place);
    }

    /// Takes in the reach of other readings of the stretch.
    pub(super) fn merge(&mut self, next: &Self) {
        for (path, summary) in &next.leaves {
            self.leaves.entry(path.clone()).or_default().merge(summary);
        }
    }

    /// The reach as `point` carries it: each number, naming its lowest and highest reading and
    /// which of the places read that highest. A number that did not move and that `point` reads
    /// at the same value says nothing the point does not, so it stays out, and so does a reading
    /// no number peaked at.
    pub(super) fn json(&self, point: ReachPoint) -> Value {
        let point_reading = Self::json_point_reading(point);

        let mut leaves = Map::new();
        let mut places: Vec<MachinePlace> = Vec::new();
        for (path, summary) in &self.leaves {
            let Some(measured) = summary.measured.as_ref() else {
                continue;
            };

            let moved = measured.minimum.value < measured.maximum.value;
            let read_there = point_reading
                .as_ref()
                .is_some_and(|reading| reading.json_reads(path, measured.maximum.value));
            if moved || !read_there {
                let peak = measured.maximum.place;
                let at = places.iter().position(|place| *place == peak);
                let at = at.unwrap_or(places.len());
                if at == places.len() {
                    places.push(peak);
                }
                let (low, high) = (measured.minimum.value, measured.maximum.value);
                leaves.insert(path.clone(), json!([low, high, at]));
            }
        }

        let places: Vec<Value> = places
            .iter()
            .map(|place| {
                json!([
                    identity_text(Identity(place.run)).to_string(),
                    identity_text(Identity(place.boot)).to_string(),
                    place.publication,
                    place.wall_ns,
                ])
            })
            .collect();

        json!({"leaves": leaves, "places": places})
    }

    /// What `point` itself reads, its derived numbers included, as the reach of that one reading,
    /// and `None` after the join, where the reach cannot know the point's readings.
    fn json_point_reading(point: ReachPoint) -> Option<Self> {
        match point {
            ReachPoint::TailPoint(point) => {
                // A reach walked with the point would grow the walk toward `NODES_MAX`, whose
                // panic aborts a release build.
                assert!(point.get("reach").is_none());
                assert!(point.get("reach_after").is_none());
                let mut reading = Self::default();
                reading.add(point, &MachinePlace::default());

                Some(reading)
            }
            ReachPoint::AfterJoin => None,
        }
    }

    /// Whether the one reading this reach took in read exactly `value` at `path`.
    fn json_reads(&self, path: &str, value: f64) -> bool {
        let read = self
            .leaves
            .get(path)
            .and_then(|summary| summary.measured.as_ref());
        let Some(read) = read else {
            return false;
        };

        assert_eq!(read.count, 1);

        read.maximum.value.to_bits() == value.to_bits()
    }
}

/// Queues the fields of an object at `path`, leaving out those that only place a reading.
fn add_fields<'point>(
    pending: &mut Vec<(String, &'point Value)>,
    path: &str,
    fields: &'point Map<String, Value>,
) {
    for (name, value) in fields {
        if path.is_empty() {
            if !PLACE_FIELDS.contains(&name.as_str()) {
                pending.push((name.clone(), value));
            }
        } else {
            pending.push((format!("{path}/{name}"), value));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use serde_json::Value;

    use super::{MachinePlace, NumberReach, Reach, ReachPoint, number_path, number_reach};

    /// The place of the reading at `index` of a test's readings.
    fn place_of(index: usize) -> MachinePlace {
        MachinePlace {
            publication: u64::try_from(index).unwrap(),
            ..MachinePlace::default()
        }
    }

    /// The reach of `points`, a stretch whose first reading is its point, as the older tail
    /// writes it.
    fn reach_of(points: &[Value]) -> Value {
        let mut reach = Reach::default();
        for (index, point) in points.iter().enumerate() {
            reach.add(point, &place_of(index));
        }

        reach.json(ReachPoint::TailPoint(&points[0]))
    }

    /// The low, the high and the index of the reading that read the high of the number at `path`.
    fn read(reach: &Value, path: &str) -> (f64, f64, u64) {
        let number = number_reach(reach, path).unwrap().unwrap();

        (number.low, number.high, number.peak.publication)
    }

    #[test]
    fn a_reach_names_each_number_by_its_path_and_no_place() {
        // Goal: a chart finds a number's reach by where the number sits in a point, and nothing
        // that only says when a reading was taken takes a reach.
        // Method: two readings with numbers at each depth.
        let reach = reach_of(&[
            json!({"wall_ns": 1, "publication": 1, "cpu_percent": 10.0,
                "storage": {"read_bps": 5.0}, "cpu_level_percent": [1.0, null]}),
            json!({"wall_ns": 2, "publication": 2, "cpu_percent": 95.0,
                "storage": {"read_bps": 2.0}, "cpu_level_percent": [3.0, 4.0]}),
        ]);

        assert_eq!(read(&reach, "cpu_percent"), (10.0, 95.0, 1));
        assert_eq!(read(&reach, "storage/read_bps"), (2.0, 5.0, 0));
        assert_eq!(read(&reach, "cpu_level_percent/0"), (1.0, 3.0, 1));
        let leaves = reach["leaves"].as_object().unwrap();
        assert_eq!(leaves.get("wall_ns"), None);
        assert_eq!(leaves.get("publication"), None);
    }

    #[test]
    fn a_number_that_did_not_move_names_no_reach() {
        // Goal: a number every reading of a stretch agrees on says nothing its point does not, so
        // a point carries no reach for it.
        // Method: two readings, one number the same in both and one that moved.
        let reach = reach_of(&[
            json!({"cpu_percent": 10.0, "gpu_active_percent": 5.0}),
            json!({"cpu_percent": 10.0, "gpu_active_percent": 6.0}),
        ]);

        let unmoved = number_reach(&reach, "cpu_percent").unwrap();

        assert_eq!(unmoved, None);
        assert_eq!(read(&reach, "gpu_active_percent"), (5.0, 6.0, 1));
    }

    #[test]
    fn a_number_its_point_does_not_read_names_its_reach_though_it_did_not_move() {
        // Goal: a number the point leaves out says nothing the point says, though the other
        // readings of its stretch agree on it, so the reach names it at the earliest reading
        // that read it, as the glossary's Point keeps an older reading's highest. Method: a
        // stretch whose first reading, its point, reads no GPU activity, which the other two
        // read the same.
        let reach = reach_of(&[
            json!({"cpu_percent": 10.0}),
            json!({"cpu_percent": 12.0, "gpu_active_percent": 5.0}),
            json!({"cpu_percent": 11.0, "gpu_active_percent": 5.0}),
        ]);

        let unread = number_reach(&reach, "gpu_active_percent").unwrap();

        assert_eq!(
            unread,
            Some(NumberReach {
                low: 5.0,
                high: 5.0,
                peak: place_of(1),
            })
        );
    }

    #[test]
    fn a_number_its_point_reads_at_another_value_names_its_reach_though_it_did_not_move() {
        // Goal: a number the point reads at a value its stretch's readings did not read says
        // nothing the point says, so the reach names it though those readings agree on it.
        // Method: a reach of two readings that both read GPU activity at 5%, made on a point
        // that reads 6%.
        let mut reach = Reach::default();
        reach.add(&json!({"gpu_active_percent": 5.0}), &place_of(0));
        reach.add(&json!({"gpu_active_percent": 5.0}), &place_of(1));
        let point = json!({"gpu_active_percent": 6.0});

        let reach = reach.json(ReachPoint::TailPoint(&point));

        assert_eq!(
            number_reach(&reach, "gpu_active_percent").unwrap(),
            Some(NumberReach {
                low: 5.0,
                high: 5.0,
                peak: place_of(0),
            })
        );
    }

    #[test]
    fn a_reach_names_only_the_readings_its_numbers_peaked_at() {
        // Goal: a point's size follows what its stretch peaked at and not how many readings it
        // holds.
        // Method: three readings of which one holds the peak of both numbers.
        let reach = reach_of(&[
            json!({"cpu_percent": 10.0, "gpu_active_percent": 5.0}),
            json!({"cpu_percent": 90.0, "gpu_active_percent": 50.0}),
            json!({"cpu_percent": 20.0, "gpu_active_percent": 6.0}),
        ]);

        assert_eq!(reach["places"].as_array().unwrap().len(), 1);
        assert_eq!(read(&reach, "cpu_percent"), (10.0, 90.0, 1));
        assert_eq!(read(&reach, "gpu_active_percent"), (5.0, 50.0, 1));
    }

    #[test]
    fn a_chart_reads_what_the_reach_wrote_at_every_form_of_a_numbers_path() {
        // Goal: the writer and the reader of a reach agree on each form a number's path takes and
        // on which identity is the run's and which the boot's, which a transposition would swap.
        // Method: a reading that holds a number of each form and peaks at a place of distinct
        // identities, read through the path a chart names each number by.
        let peak = MachinePlace {
            boot: [1, 2],
            run: [3, 4],
            publication: 7,
            wall_ns: 9,
        };
        let mut reach = Reach::default();
        let point = json!({"cpu_percent": 1.0, "cpu_level_percent": [2.0],
            "storage": {"read_bps": 3.0, "queue": [4.0, 5.0]}});
        reach.add(&point, &MachinePlace::default());
        reach.add(
            &json!({"cpu_percent": 11.0, "cpu_level_percent": [12.0],
                "storage": {"read_bps": 13.0, "queue": [14.0, 15.0]}}),
            &peak,
        );
        let reach = reach.json(ReachPoint::TailPoint(&point));
        let number = |path: String| number_reach(&reach, &path).unwrap();
        let reached = |low, high| Some(NumberReach { low, high, peak });

        let plain = number(number_path(None, "cpu_percent", None));
        let channel = number(number_path(None, "cpu_level_percent", Some(0)));
        let resource = number(number_path(Some("storage"), "read_bps", None));
        let both = number(number_path(Some("storage"), "queue", Some(1)));

        assert_eq!(plain, reached(1.0, 11.0));
        assert_eq!(channel, reached(2.0, 12.0));
        assert_eq!(resource, reached(3.0, 13.0));
        assert_eq!(both, reached(5.0, 15.0));
    }

    #[test]
    fn a_reach_that_names_a_reading_it_does_not_hold_is_refused() {
        // Goal: a point the reach writer did not write is an error and no made-up place.
        // Method: a number whose place lies past the places, and one with no high.
        let past = json!({"leaves": {"cpu_percent": [1.0, 2.0, 1]}, "places": []});
        let short = json!({"leaves": {"cpu_percent": [1.0]}, "places": []});

        assert!(number_reach(&past, "cpu_percent").is_err());
        assert!(number_reach(&short, "cpu_percent").is_err());
    }

    #[test]
    fn a_reach_whose_number_is_not_a_list_is_refused() {
        // Goal: a number the reach writer did not write is an error, not a number that did not
        // move, which a chart would draw as flat.
        // Method: a number written as a plain value.
        let plain = json!({"leaves": {"cpu_percent": 1.0}, "places": []});

        assert!(number_reach(&plain, "cpu_percent").is_err());
    }

    #[test]
    fn a_reach_takes_a_flag_as_zero_or_one() {
        // Goal: a chart draws a flag as the state it names, so a stretch that charged for one
        // second reaches that state and names the reading that read it.
        // Method: a reading of each value of the flag.
        let reach = reach_of(&[
            json!({"battery": {"charging": false}}),
            json!({"battery": {"charging": true}}),
        ]);

        assert_eq!(read(&reach, "battery/charging"), (0.0, 1.0, 1));
    }

    #[test]
    fn a_derived_numbers_reach_is_what_it_read_at_one_reading() {
        // Goal: the charts' sums and differences reach what they read at one instant, which adding
        // up their parts' separate peaks would overstate.
        // Method: wired peaks at one reading and compressed at another, with power and GPU.
        let reach = reach_of(&[
            json!({"memory_anonymous_bytes": 12, "memory_purgeable_bytes": 2,
                "memory_wired_bytes": 30, "memory_compressed_bytes": 1,
                "power_watts": 10.0, "gpu_watts": 8.0, "fan_rpm": [100.0, 300.0]}),
            json!({"memory_anonymous_bytes": 15, "memory_purgeable_bytes": 2,
                "memory_wired_bytes": 1, "memory_compressed_bytes": 20,
                "power_watts": 20.0, "gpu_watts": 1.0, "fan_rpm": [400.0, null]}),
        ]);

        assert_eq!(read(&reach, "memory_app_bytes"), (10.0, 13.0, 1));
        assert_eq!(read(&reach, "memory_used_bytes"), (34.0, 41.0, 0));
        assert_eq!(read(&reach, "power_remainder_watts"), (2.0, 19.0, 1));
        assert_eq!(read(&reach, "fan_mean_rpm"), (200.0, 400.0, 1));
    }

    #[test]
    fn a_derived_number_that_did_not_move_names_a_reach_only_where_its_point_lacks_it() {
        // Goal: a number the charts derive follows the rule its parts do, so a flat one the point
        // reads stays out and a flat one the point cannot derive names its reach. Method: two
        // stretches whose app memory reads 10 at every reading whose parts it can be derived
        // from, though anonymous and purgeable memory move, one whose point reads purgeable
        // memory and one whose point does not.
        let read_at_its_point = reach_of(&[
            json!({"memory_anonymous_bytes": 12, "memory_purgeable_bytes": 2}),
            json!({"memory_anonymous_bytes": 13, "memory_purgeable_bytes": 3}),
        ]);
        let unread_at_its_point = reach_of(&[
            json!({"memory_anonymous_bytes": 12}),
            json!({"memory_anonymous_bytes": 13, "memory_purgeable_bytes": 3}),
        ]);

        let read = number_reach(&read_at_its_point, "memory_app_bytes").unwrap();
        let unread = number_reach(&unread_at_its_point, "memory_app_bytes").unwrap();

        assert_eq!(read, None);
        assert_eq!(
            unread,
            Some(NumberReach {
                low: 10.0,
                high: 10.0,
                peak: place_of(1),
            })
        );
    }

    #[test]
    fn merging_reaches_cut_anywhere_equals_taking_in_their_readings() {
        // Goal: a stretch that two minutes share reaches what it would in one minute.
        // Method: three readings cut each way, against all three taken in at once.
        let points = [
            json!({"cpu_percent": 10.0}),
            json!({"cpu_percent": 95.0}),
            json!({"cpu_percent": 5.0}),
        ];
        for cut in 0..=points.len() {
            let mut left = Reach::default();
            for (index, point) in points[..cut].iter().enumerate() {
                left.add(point, &place_of(index));
            }
            let mut right = Reach::default();
            for (index, point) in points[cut..].iter().enumerate() {
                right.add(point, &place_of(cut + index));
            }

            left.merge(&right);

            let merged = left.json(ReachPoint::TailPoint(&points[0]));
            assert_eq!(merged, reach_of(&points), "cut {cut}");
        }
    }
}
