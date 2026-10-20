use persistence::Error;

use super::{NS, ObservationKey, Window, WindowValue};
use crate::apps::Stamp;
use crate::live_app_response::Observation;

/// A value of the bytes a test gives it, in a window of one hundred bytes.
struct Weighed {
    selected: Observation,
    bytes: usize,
}

impl WindowValue for Weighed {
    const BYTES_MAX: usize = 100;

    fn selected(&self) -> &Observation {
        &self.selected
    }

    fn bytes(&self) -> usize {
        self.bytes
    }
}

#[test]
fn holds_without_a_value_an_observation_whose_value_would_pass_the_window_bytes() {
    // Goal: a window keeps within its bytes, and still holds the observation whose value it
    // refused, so the replay does not run again for it.
    // Method: insert two values of sixty bytes into a window of one hundred.
    let mut window = Window::default();
    window.insert(weighed(1, 60)).unwrap();

    window.insert(weighed(2, 60)).unwrap();

    assert!(window.get(&key(1)).is_some());
    assert!(window.get(&key(2)).is_none());
    assert!(window.holds(&key(2)));
}

#[test]
fn frees_the_bytes_of_an_observation_it_drops() {
    // Goal: an observation dropped from a moved window leaves its bytes to the next value.
    // Method: drop a value of sixty bytes from a window of one hundred, then insert another.
    let mut window = Window::default();
    window.insert(weighed(1, 60)).unwrap();
    window.retain(2 * NS, 3 * NS);

    window.insert(weighed(2, 60)).unwrap();

    assert!(window.get(&key(2)).is_some());
}

#[test]
fn refuses_an_observation_past_its_bound() {
    // Goal: a window holds no more observations than a window and the live ones after it
    // number, and refuses the next with an error rather than growing.
    // Method: insert 4096 values of no bytes, then one more.
    let mut window = Window::default();
    fill(&mut window, 4096);

    let refused = window.insert(weighed(4097, 0));

    assert!(matches!(refused, Err(Error::Bound("window values"))));
}

/// Inserts a value of no bytes for each second from one through `count`.
fn fill(window: &mut Window<Weighed>, count: u64) {
    for second in 1..=count {
        window.insert(weighed(second, 0)).unwrap();
    }
}

/// A value of `bytes` for the observation at second `second`.
fn weighed(second: u64, bytes: usize) -> Weighed {
    Weighed {
        selected: Observation {
            recording: [1, 2],
            stamp: Stamp {
                boot: [3, 4],
                run: [5, 6],
                publication: second,
                continuous_ns: Some(second * NS),
                awake_ns: Some(second * NS),
            },
            run_tick: second,
            wall_ns: Some(second * NS),
        },
        bytes,
    }
}

fn key(second: u64) -> ObservationKey {
    ObservationKey {
        run: [5, 6],
        boot: [3, 4],
        publication: second,
    }
}
