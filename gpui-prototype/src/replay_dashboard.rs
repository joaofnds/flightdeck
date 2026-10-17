use std::time::Duration;

use gpui::{App, Context, Entity};
use serde_json::json;

use crate::dashboard::Dashboard;
use crate::live_timer::Timer;

pub fn start(entity: &Entity<Dashboard>, cx: &mut App) -> Result<(), String> {
    let started = entity.read(cx).replay.as_ref().map(|replay| {
        json!({"event":"replay_started", "schema":2, "duration_ms":replay.duration_ms(),
            "actions":replay.report()["actions"]})
    });
    let Some(started) = started else {
        return Ok(());
    };
    println!("{started}");
    let weak = entity.downgrade();
    let application = cx.to_async();
    let timer = Timer::with_interval(Duration::from_secs_f64(1.0 / 30.0), move || {
        if let Err(error) = application.update(|cx| {
            if let Err(error) = weak.update(cx, Dashboard::replay_tick) {
                eprintln!("Replay view update: {error}");
            }
        }) {
            eprintln!("Replay application update: {error}");
        }
    })?;
    entity.update(cx, |dashboard, _| dashboard.replay_timer = Some(timer));
    Ok(())
}

impl Dashboard {
    pub fn replay_tick(&mut self, cx: &mut Context<Self>) {
        let elapsed_ms = match u64::try_from(self.performance.started.elapsed().as_millis()) {
            Ok(elapsed) => elapsed,
            Err(error) => {
                eprintln!("Replay clock: {error}");
                cx.quit();
                return;
            }
        };
        let actions = match &mut self.replay {
            Some(replay) => replay.step(elapsed_ms),
            None => return,
        };
        let changed = !actions.is_empty();
        for action in actions {
            match self.replay_completed_action(&action) {
                Ok(event) => println!("{event}"),
                Err(error) => {
                    eprintln!("Replay action {}: {error}", action.index);
                    println!(
                        "{}",
                        json!({"event":"replay_error", "schema":2,
                        "index":action.index,"error":error})
                    );
                    cx.quit();
                    return;
                }
            }
        }
        let completed_ms = match u64::try_from(self.performance.started.elapsed().as_millis()) {
            Ok(elapsed) => elapsed,
            Err(error) => {
                eprintln!("Replay completion clock: {error}");
                cx.quit();
                return;
            }
        };
        if let Some(replay) = &self.replay {
            if replay.complete(completed_ms) {
                let mut report = replay.report();
                report["view_render_count"] = json!(self.performance.frames);
                println!("{report}");
                cx.quit();
                return;
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn replay_completed_action(
        &mut self,
        action: &crate::replay::Action,
    ) -> Result<serde_json::Value, String> {
        let started = self.performance.started;
        let (effective, completed_ms) = crate::replay::apply_and_time(
            action,
            |action| self.replay_apply(action),
            || u64::try_from(started.elapsed().as_millis()).map_err(|error| error.to_string()),
        )?;
        self.replay
            .as_mut()
            .ok_or("Replay was removed while applying an action")?
            .record_acceptance(action, completed_ms)?;
        Ok(action.acceptance(completed_ms, &effective))
    }
}
