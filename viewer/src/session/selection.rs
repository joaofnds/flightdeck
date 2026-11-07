use super::{NS, Observation, Session, WINDOWS_MAX};
use collector::recording::identity_text;
use persistence::{Error, Result};
use serde_json::{Value, json};

impl Session {
    pub(crate) fn restore_selection(&mut self, selected: &Value) -> Result<()> {
        assert!(self.selected.is_none());
        self.operation.check()?;
        let publication = selected["publication"]
            .as_u64()
            .ok_or(Error::Bound("selected publication"))?;
        let wall_ns = selected["wall_ns"]
            .as_u64()
            .ok_or(Error::Bound("selected wall time"))?;
        self.load_span(wall_ns / NS)?;
        let Some(window) = self.restore_selection_window(selected, publication)? else {
            return Ok(());
        };

        let ticks = self.reader().minute_ticks(window)?;
        let index = ticks
            .binary_search_by_key(&publication, |tick| tick.publication)
            .map_err(|_| Error::Bound("missing selected tick"))?;
        let observation = Observation::of_tick(&ticks[index], window)
            .ok_or(Error::Bound("missing tick field"))?;
        self.selected_retained(&observation)?;
        for (name, value) in [
            ("publication", observation.publication),
            ("run_tick", observation.run_tick),
            ("wall_ns", observation.wall),
        ] {
            if selected[name].as_u64() != Some(value) {
                return Err(Error::Bound("changed selected tick identity"));
            }
        }
        if let Some(continuous) = selected.get("continuous_ns") {
            if *continuous != json!(observation.continuous) {
                return Err(Error::Bound("changed selected tick clock"));
            }
        }

        self.operation.check()?;
        self.selected = Some(observation);

        Ok(())
    }

    fn restore_selection_window(
        &self,
        selected: &Value,
        publication: u64,
    ) -> Result<Option<usize>> {
        assert!(self.windows.len() <= WINDOWS_MAX);
        let run = selected["run"]
            .as_str()
            .ok_or(Error::Bound("selected run"))?;
        let boot = selected["boot"]
            .as_str()
            .ok_or(Error::Bound("selected boot"))?;
        let mut chosen = None;
        for (window, manifest) in self.windows.iter().enumerate() {
            self.operation.check()?;
            if identity_text(manifest.identity.run.run).to_string() != run {
                continue;
            }

            if identity_text(manifest.identity.run.boot).to_string() != boot {
                continue;
            }

            if (manifest.publication_first..manifest.publication_end).contains(&publication) {
                if chosen.is_some() {
                    return Err(Error::Bound("duplicate selected commitment"));
                }

                chosen = Some(window);
            }
        }

        Ok(chosen)
    }
}
