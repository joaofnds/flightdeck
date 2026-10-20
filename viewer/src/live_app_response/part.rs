//! One App's part of a sample, which keeps the App's detail at that second once the sample is
//! gone, so a hover can show it for any second of the window.
use persistence::{Error, Result};
use serde_json::Value;

use super::json::{self, AppMembers, Header};
use super::{App, KeyKind, Observation, Page, Process, Sample};

/// The App with one key at one second, its processes, and the facts of that second's sample
/// every page shows. An App that did not run at that second has no detail there, as a read of
/// that second has none.
pub struct AppPart {
    header: Header,
    kind: KeyKind,
    bytes: Box<[u8]>,
    present: Option<Present>,
}

/// The App's row and processes at a second it ran.
struct Present {
    app_index: usize,
    app: App,
    processes: Vec<Process>,
}

impl Sample {
    /// The part of the App with this key.
    pub fn app_part(&self, kind: KeyKind, bytes: &[u8]) -> Result<AppPart> {
        let index = self.apps.iter().position(|app| {
            if app.key.kind == kind {
                app.key.bytes == bytes
            } else {
                false
            }
        });
        let present = match index {
            Some(index) => {
                let process_app_index =
                    u32::try_from(index).map_err(|_| Error::Bound("live App index"))?;
                Some(Present {
                    app_index: index,
                    app: self.apps[index].clone(),
                    processes: self
                        .processes
                        .iter()
                        .filter(|process| process.app_index == Some(process_app_index))
                        .cloned()
                        .collect(),
                })
            }
            None => None,
        };

        Ok(AppPart {
            header: Header::of(self),
            kind,
            bytes: bytes.into(),
            present,
        })
    }
}

impl AppPart {
    /// The App's detail page at its second, which is null where the App did not run.
    pub fn detail(&self, page: Page) -> Result<Value> {
        let Some(present) = &self.present else {
            return Ok(Value::Null);
        };
        let processes: Vec<&Process> = present.processes.iter().collect();

        json::app_members_page(
            &AppMembers {
                header: &self.header,
                app: &present.app,
                app_index: present.app_index,
                processes: &processes,
            },
            page,
        )
    }

    /// The Apps page of its second without the Apps it lists, which is what pages them.
    pub fn apps_paging(&self, page: Page) -> Result<Value> {
        json::apps_paging(&self.header, page)
    }

    #[must_use]
    pub fn selected(&self) -> &Observation {
        self.header.selected()
    }

    #[must_use]
    pub fn is_of(&self, kind: KeyKind, bytes: &[u8]) -> bool {
        if self.kind != kind {
            return false;
        }

        *self.bytes == *bytes
    }

    /// Includes every retained vector and string capacity, plus the part itself.
    #[must_use]
    pub fn bytes(&self) -> usize {
        let mut bytes = size_of::<Self>() + self.bytes.len();
        if let Some(present) = &self.present {
            bytes += present.app.heap_bytes();
            bytes += present.processes.capacity() * size_of::<Process>();
            for process in &present.processes {
                bytes += process.heap_bytes();
            }
        }

        bytes
    }
}
