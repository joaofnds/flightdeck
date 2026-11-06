//! How far each number of a trend point went over the readings of one stretch, so a chart that
//! draws one reading of the stretch still finds what the others read. A number a chart derives
//! from several takes its reach here too, reading by reading, since the reach of a sum is what the
//! sum read at one instant and not what its parts each reached at another.

use std::collections::BTreeMap;

use collector::recording::identity_text;
use collector::ring::Identity;
use serde_json::{Map, Value, json};

use crate::store::MachinePlace;

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

/// The objects, arrays and numbers one trend point can hold, which is far fewer.
const NODES_MAX: usize = 4096;

/// The lowest and highest reading of each number, by its path through the point, and the readings
/// taken in, which name where each highest was read.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Reach {
    leaves: BTreeMap<String, Leaf>,
    places: Vec<MachinePlace>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Leaf {
    low: f64,
    high: f64,
    /// Which of the reach's places read `high`.
    place: usize,
}

impl Reach {
    /// Takes in the reading `point`, taken at `place`.
    pub(super) fn add(&mut self, point: &Value, place: MachinePlace) {
        self.places.push(place);
        let mut pending: Vec<(String, &Value)> = vec![(String::new(), point)];
        for _ in 0..NODES_MAX {
            let Some((path, node)) = pending.pop() else {
                self.add_derived(point);
                return;
            };
            match node {
                Value::Number(number) => {
                    if let Some(value) = number.as_f64() {
                        self.take(path, value);
                    }
                }
                Value::Array(items) => {
                    let items = items.iter().enumerate();
                    pending.extend(items.map(|(at, item)| (format!("{path}/{at}"), item)));
                }
                Value::Object(fields) => reach_fields(&mut pending, &path, fields),
                _ => {}
            }
        }

        panic!("a trend point holds at most {NODES_MAX} values");
    }

    /// Takes in what the charts derive from several numbers of the reading `point`.
    fn add_derived(&mut self, point: &Value) {
        let bytes = |name: &str| point[name].as_u64();
        let memory = bytes("memory_anonymous_bytes")
            .zip(bytes("memory_purgeable_bytes"))
            .and_then(|(anonymous, purgeable)| anonymous.checked_sub(purgeable));
        if let Some(app) = memory {
            self.take("memory_app_bytes".to_owned(), model::rates::number(app));
            let used = bytes("memory_wired_bytes")
                .zip(bytes("memory_compressed_bytes"))
                .and_then(|(wired, compressed)| app.checked_add(wired)?.checked_add(compressed));
            if let Some(used) = used {
                self.take("memory_used_bytes".to_owned(), model::rates::number(used));
            }
        }

        let watts = |name: &str| point[name].as_f64();
        if let Some((system, gpu)) = watts("power_watts").zip(watts("gpu_watts")) {
            self.take("power_remainder_watts".to_owned(), system - gpu);
        }

        let fans = point["fan_rpm"].as_array().map_or(&[][..], Vec::as_slice);
        let speeds = || fans.iter().filter_map(Value::as_f64);
        if let Some(count) = u32::try_from(speeds().count())
            .ok()
            .filter(|count| *count > 0)
        {
            // The mean of a few fan speeds, none near where a double rounds.
            self.take(
                "fan_mean_rpm".to_owned(),
                speeds().sum::<f64>() / f64::from(count),
            );
        }
    }

    /// Takes in `value`, read at the last place taken in.
    fn take(&mut self, path: String, value: f64) {
        let place = self.places.len() - 1;
        self.take_leaf(
            path,
            Leaf {
                low: value,
                high: value,
                place,
            },
        );
    }

    fn take_leaf(&mut self, path: String, next: Leaf) {
        let leaf = self.leaves.entry(path).or_insert(next);
        leaf.low = leaf.low.min(next.low);
        if next.high > leaf.high {
            leaf.high = next.high;
            leaf.place = next.place;
        }
    }

    /// Takes in the reach of other readings of the stretch.
    pub(super) fn merge(&mut self, next: &Self) {
        let first = self.places.len();
        self.places.extend_from_slice(&next.places);
        for (path, leaf) in &next.leaves {
            let place = first + leaf.place;
            self.take_leaf(path.clone(), Leaf { place, ..*leaf });
        }
    }

    /// The reach as a trend point carries it: each path naming its lowest and highest reading and
    /// which of the places read that highest.
    pub(super) fn json(&self) -> Value {
        let mut leaves = Map::with_capacity(self.leaves.len());
        for (path, leaf) in &self.leaves {
            leaves.insert(path.clone(), json!([leaf.low, leaf.high, leaf.place]));
        }
        let places: Vec<Value> = self
            .places
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
}

/// Queues the fields of an object at `path`, leaving out those that only place a reading.
fn reach_fields<'point>(
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

    use super::{MachinePlace, Reach};

    /// The place of the reading at `index` of a test's readings.
    fn place_of(index: usize) -> MachinePlace {
        MachinePlace {
            publication: u64::try_from(index).unwrap(),
            ..MachinePlace::default()
        }
    }

    fn reach_of(points: &[serde_json::Value]) -> serde_json::Value {
        let mut reach = Reach::default();
        for (index, point) in points.iter().enumerate() {
            reach.add(point, place_of(index));
        }

        reach.json()["leaves"].clone()
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

        assert_eq!(
            reach,
            json!({"cpu_percent": [10.0, 95.0, 1], "storage/read_bps": [2.0, 5.0, 0],
                "cpu_level_percent/0": [1.0, 3.0, 1], "cpu_level_percent/1": [4.0, 4.0, 1]})
        );
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
            json!({"memory_anonymous_bytes": 12, "memory_purgeable_bytes": 2,
                "memory_wired_bytes": 1, "memory_compressed_bytes": 30,
                "power_watts": 20.0, "gpu_watts": 1.0, "fan_rpm": [400.0, null]}),
        ]);

        assert_eq!(reach["memory_app_bytes"], json!([10.0, 10.0, 0]));
        assert_eq!(reach["memory_used_bytes"], json!([41.0, 41.0, 0]));
        assert_eq!(reach["power_remainder_watts"], json!([2.0, 19.0, 1]));
        assert_eq!(reach["fan_mean_rpm"], json!([200.0, 400.0, 1]));
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
                left.add(point, place_of(index));
            }
            let mut right = Reach::default();
            for (index, point) in points[cut..].iter().enumerate() {
                right.add(point, place_of(cut + index));
            }

            left.merge(&right);

            assert_eq!(left.json()["leaves"], reach_of(&points), "cut {cut}");
        }
    }
}
