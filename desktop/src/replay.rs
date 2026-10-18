use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub struct Replay {
    plan: Plan,
    next: usize,
    applied: usize,
    last_ms: u64,
    completed_ms: u64,
    lateness_ms_max: u64,
    lateness_ms_total: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    #[serde(skip)]
    pub index: u32,
    pub at_ms: u64,
    pub route: String,
    pub sample: Option<u16>,
    pub minutes: Option<u16>,
    pub scroll: Option<Scroll>,
    pub app: Option<String>,
    pub process: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scroll {
    Top,
    Hardware,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema: u16,
    duration_ms: u64,
    actions: Vec<Action>,
}

const FILE_BYTES_MAX: u64 = 4 * 1024 * 1024;
const ACTIONS_MAX: usize = 10_000;
const ACTIONS_PER_STEP_MAX: usize = 64;
const COMPLETION_GRACE_MS: u64 = 15_000;
const ROUTES: [&str; 11] = [
    "overview", "cpu", "apps", "gpu", "memory", "energy", "storage", "network", "battery",
    "computer", "self",
];
const _: () = {
    assert!(ACTIONS_PER_STEP_MAX <= ACTIONS_MAX);
    assert!(FILE_BYTES_MAX > 4096);
};

impl Replay {
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        file.take(FILE_BYTES_MAX + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        Self::from_json(&bytes)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        if u64::try_from(bytes.len()).map_err(|error| error.to_string())? > FILE_BYTES_MAX {
            return Err("Replay file exceeds 4 MiB".into());
        }
        let mut plan: Plan = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        plan.validate()?;
        for (index, action) in plan.actions.iter_mut().enumerate() {
            action.index = u32::try_from(index).map_err(|error| error.to_string())?;
        }
        Ok(Self {
            plan,
            next: 0,
            applied: 0,
            last_ms: 0,
            completed_ms: 0,
            lateness_ms_max: 0,
            lateness_ms_total: 0,
        })
    }

    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.plan.duration_ms
    }

    pub fn step(&mut self, elapsed_ms: u64) -> Vec<Action> {
        assert!(elapsed_ms >= self.last_ms);
        self.last_ms = elapsed_ms;
        let mut due = Vec::with_capacity(ACTIONS_PER_STEP_MAX);
        for _attempt in 0..ACTIONS_PER_STEP_MAX {
            let Some(action) = self.plan.actions.get(self.next) else {
                break;
            };
            if action.at_ms > elapsed_ms {
                break;
            }
            due.push(action.clone());
            self.next += 1;
        }
        due
    }

    pub fn record_acceptance(&mut self, action: &Action, completed_ms: u64) -> Result<(), String> {
        if self.applied >= self.next {
            return Err("Replay action was not dispatched".into());
        }
        if self.plan.actions.get(self.applied) != Some(action) {
            return Err("Replay completion is duplicated, changed or out of order".into());
        }
        if completed_ms < self.last_ms {
            return Err("Replay completion precedes dispatch".into());
        }
        if completed_ms < self.completed_ms {
            return Err("Replay completion clock moved backward".into());
        }
        if completed_ms > self.plan.duration_ms + COMPLETION_GRACE_MS {
            return Err("Replay completion exceeded the bounded shutdown grace".into());
        }
        let lateness = completed_ms
            .checked_sub(action.at_ms)
            .ok_or("Replay completion precedes its schedule")?;
        self.lateness_ms_total = self
            .lateness_ms_total
            .checked_add(lateness)
            .ok_or("Replay completion lateness overflow")?;
        self.lateness_ms_max = self.lateness_ms_max.max(lateness);
        self.completed_ms = completed_ms;
        self.applied += 1;
        assert!(self.applied <= self.next);
        Ok(())
    }

    #[must_use]
    pub fn complete(&self, elapsed_ms: u64) -> bool {
        if elapsed_ms < self.plan.duration_ms {
            return false;
        }
        if self.next != self.plan.actions.len() {
            return false;
        }
        self.applied == self.next
    }

    #[must_use]
    pub fn report(&self) -> Value {
        json!({"event":"replay_summary", "schema":2, "duration_ms":self.plan.duration_ms,
            "actions":self.plan.actions.len(), "dispatched":self.next,"accepted":self.applied,
            "last_completed_at_ms":self.completed_ms,
            "shows_hardware":self.shows_hardware(),
            "acceptance_lateness_ms_max":self.lateness_ms_max,
            "acceptance_lateness_ms_total":self.lateness_ms_total,
            "metric":"CPU model application completed; not layout, GPU completion or presentation"})
    }

    #[must_use]
    pub fn shows_hardware(&self) -> bool {
        self.plan
            .actions
            .iter()
            .any(|action| action.scroll == Some(Scroll::Hardware))
    }
}

pub fn apply_and_time<T>(
    action: &Action,
    apply: impl FnOnce(&Action) -> Result<T, String>,
    clock: impl FnOnce() -> Result<u64, String>,
) -> Result<(T, u64), String> {
    let effective = apply(action)?;
    let completed_ms = clock()?;
    Ok((effective, completed_ms))
}

impl Action {
    #[must_use]
    pub fn acceptance(&self, completed_ms: u64, effective: &Value) -> Value {
        assert!(completed_ms >= self.at_ms);
        json!({"event":"replay_action", "schema":2, "index":self.index,
            "scheduled_at_ms":self.at_ms, "accepted_at_ms":completed_ms,
            "acceptance_lateness_ms":completed_ms-self.at_ms,
            "requested":self, "effective":effective,
            "metric":"CPU model application completed; not layout or presentation"})
    }

    fn validate(&self, duration_ms: u64) -> Result<(), String> {
        if self.at_ms > duration_ms {
            return Err("Replay action exceeds duration".into());
        }
        if !ROUTES.contains(&self.route.as_str()) {
            return Err(format!("Unknown replay route {}", self.route));
        }
        if let Some(sample) = self.sample {
            if !(90..=120).contains(&sample) {
                return Err("Replay sample must be 90..120".into());
            }
        }
        if let Some(minutes) = self.minutes {
            if ![15, 60].contains(&minutes) {
                return Err("Replay minutes must be 15 or 60".into());
            }
        }
        if self.scroll == Some(Scroll::Hardware) {
            if self.route != "overview" {
                return Err("Hardware scroll requires Overview".into());
            }
        }
        for value in [&self.app, &self.process].into_iter().flatten() {
            if value.is_empty() {
                return Err("Replay selection is empty".into());
            }
            if value.len() > 4095 {
                return Err("Replay selection exceeds 4095 UTF-8 bytes".into());
            }
            if value.contains('\0') {
                return Err("Replay selection contains a null byte".into());
            }
        }
        Ok(())
    }
}

impl Plan {
    fn validate(&self) -> Result<(), String> {
        if self.schema != 1 {
            return Err("Unknown replay schema".into());
        }
        if !(1..=600_000).contains(&self.duration_ms) {
            return Err("Replay duration must be 1..600000 ms".into());
        }
        if self.actions.len() > ACTIONS_MAX {
            return Err("Replay exceeds 10000 actions".into());
        }
        let mut previous = 0;
        for action in &self.actions {
            action.validate(self.duration_ms)?;
            if action.at_ms < previous {
                return Err("Replay actions are not in scheduled order".into());
            }
            previous = action.at_ms;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatches_the_same_actions_when_the_clock_skips_render_frames() {
        // Scheduled work depends on elapsed time and retains all actions after a late tick.
        let mut replay = Replay::from_json(
            br#"{"schema":1,"duration_ms":1000,"actions":[
            {"at_ms":100,"route":"cpu","sample":90},
            {"at_ms":120,"route":"apps","sample":120},
            {"at_ms":900,"route":"overview","scroll":"hardware"}]}"#,
        )
        .unwrap();

        assert!(replay.step(99).is_empty());
        let actions = replay.step(850);

        assert_eq!(
            actions
                .iter()
                .map(|action| action.index)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(replay.report()["accepted"], 0);
        replay.record_acceptance(&actions[0], 850).unwrap();
        replay.record_acceptance(&actions[1], 850).unwrap();
        assert_eq!(replay.report()["acceptance_lateness_ms_max"], 750);
        assert!(!replay.complete(1000));
        let final_action = replay.step(1000).remove(0);
        assert_eq!(final_action.index, 2);
        assert!(!replay.complete(1000));
        replay.record_acceptance(&final_action, 1000).unwrap();
        assert!(replay.complete(1000));
    }

    #[test]
    fn bounds_catch_up_without_dropping_due_actions() {
        // A stalled timer handles a fixed batch then resumes at the next undispatched action.
        let plan = json!({"schema":1,"duration_ms":1000,
            "actions":vec![json!({"at_ms":0,"route":"cpu"});65]});
        let mut replay = Replay::from_json(&serde_json::to_vec(&plan).unwrap()).unwrap();

        let first = replay.step(1000);
        let second = replay.step(1000);

        assert_eq!(first.len(), 64);
        assert_eq!(second[0].index, 64);
        assert!(!replay.complete(1000));
        for action in first.iter().chain(&second) {
            replay.record_acceptance(action, 1000).unwrap();
        }
        assert!(replay.complete(1000));
    }

    #[test]
    fn includes_each_application_and_prior_batch_work_in_its_completion_clock() {
        // A clock advanced by application work catches sampling before apply or once per batch.
        let mut replay = Replay::from_json(
            br#"{"schema":1,"duration_ms":200,"actions":[
            {"at_ms":100,"route":"cpu","sample":90},
            {"at_ms":100,"route":"cpu","sample":120}]}"#,
        )
        .unwrap();
        let actions = replay.step(100);
        let clock = std::cell::Cell::new(100_u64);
        let selected = std::cell::Cell::new(0_u16);
        let apply = |action: &Action| {
            selected.set(action.sample.unwrap());
            clock.set(clock.get() + 25);
            Ok(json!({"sample":selected.get()}))
        };
        let read_clock = || Ok(clock.get());

        let (first, first_ms) = apply_and_time(&actions[0], apply, read_clock).unwrap();
        replay.record_acceptance(&actions[0], first_ms).unwrap();
        let first_event = actions[0].acceptance(first_ms, &first);
        let (second, second_ms) = apply_and_time(&actions[1], apply, read_clock).unwrap();
        replay.record_acceptance(&actions[1], second_ms).unwrap();
        let second_event = actions[1].acceptance(second_ms, &second);

        assert_eq!(first_event["schema"], 2);
        assert_eq!(first_event["effective"]["sample"], 90);
        assert_eq!(first_event["accepted_at_ms"], 125);
        assert_eq!(second_event["effective"]["sample"], 120);
        assert_eq!(second_event["accepted_at_ms"], 150);
        assert_eq!(replay.report()["acceptance_lateness_ms_total"], 75);
        assert_eq!(replay.report()["acceptance_lateness_ms_max"], 50);
        assert!(replay.complete(200));
    }

    #[test]
    fn refuses_completion_for_an_undispatched_action() {
        // A plan row cannot become an accepted action without passing through dispatch.
        let mut replay = Replay::from_json(
            br#"{"schema":1,"duration_ms":1000,"actions":[
            {"at_ms":100,"route":"cpu"}]}"#,
        )
        .unwrap();
        let action: Action = serde_json::from_value(json!({"at_ms":100,"route":"cpu"})).unwrap();

        assert!(replay.record_acceptance(&action, 100).is_err());
        assert_eq!(replay.report()["accepted"], 0);
    }

    #[test]
    fn refuses_repeated_completion_without_changing_the_summary() {
        // Duplicate acknowledgments cannot inflate accepted counts or timing totals.
        let mut replay = Replay::from_json(
            br#"{"schema":1,"duration_ms":1000,"actions":[
            {"at_ms":100,"route":"cpu"}]}"#,
        )
        .unwrap();
        let action = replay.step(100).remove(0);
        replay.record_acceptance(&action, 125).unwrap();
        let summary = replay.report();

        assert!(replay.record_acceptance(&action, 150).is_err());
        assert_eq!(replay.report(), summary);
    }

    #[test]
    fn refuses_out_of_order_completion() {
        // Dispatched siblings must complete in their fixed application order.
        let mut replay = Replay::from_json(
            br#"{"schema":1,"duration_ms":1000,"actions":[
            {"at_ms":100,"route":"cpu"},{"at_ms":100,"route":"apps"}]}"#,
        )
        .unwrap();
        let actions = replay.step(100);

        assert!(replay.record_acceptance(&actions[1], 125).is_err());
        assert_eq!(replay.report()["accepted"], 0);
    }

    #[test]
    fn refuses_completion_outside_the_finite_benchmark_clock() {
        // A suspended or corrupt clock cannot overflow summary timing counters.
        let mut replay = Replay::from_json(
            br#"{"schema":1,"duration_ms":1000,"actions":[
            {"at_ms":100,"route":"cpu"}]}"#,
        )
        .unwrap();
        let action = replay.step(100).remove(0);

        assert!(replay.record_acceptance(&action, 16_001).is_err());
        assert_eq!(replay.report()["accepted"], 0);
    }

    #[test]
    fn refuses_reordered_actions() {
        // Time reversal must fail validation instead of silently delaying earlier work.
        assert!(
            Replay::from_json(
                br#"{"schema":1,"duration_ms":100,"actions":[
            {"at_ms":50,"route":"cpu"},{"at_ms":49,"route":"cpu"}]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn refuses_unknown_contract_fields() {
        // Misspelled workloads cannot produce a successful run of an unintended action.
        assert!(
            Replay::from_json(
                br#"{"schema":1,"duration_ms":100,"actions":[
            {"at_ms":0,"route":"cpu","samples":90}]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn refuses_samples_outside_the_shared_captured_range() {
        // Both frontends must execute the shared sample rather than clamp it differently.
        assert!(
            Replay::from_json(
                br#"{"schema":1,"duration_ms":100,"actions":[
            {"at_ms":0,"route":"cpu","sample":89}]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn refuses_hardware_scroll_on_another_route() {
        // A workload cannot claim to animate hardware while another page is displayed.
        assert!(
            Replay::from_json(
                br#"{"schema":1,"duration_ms":100,"actions":[
            {"at_ms":0,"route":"memory","scroll":"hardware"}]}"#
            )
            .is_err()
        );
    }
}
