use std::io;

use super::{Kevent, Known, Tracker, WatchDiagnostics, WatchFailure, known_index, pid_of};
use crate::ring::ProcessCreation;

/// A live knote keeps its registration until its exit notice is consumed. Re-adding an
/// existing knote changes udata but preserves queued flags, which would relabel old notices.
#[derive(Clone, Copy, Default)]
pub(super) struct Watch {
    generation: u64,
    unique_id: u64,
    verified: bool,
    pub(super) active: bool,
}

const _: () = assert!(size_of::<Known>() == 40);

impl Tracker {
    pub(super) fn submit_watches_prepare(&mut self) -> io::Result<()> {
        self.changes
            .retain(|change| !self.known[change.ident].watch.active);
        for change in &mut self.changes {
            let pid = pid_of(change.ident)?;
            let generation = self.watch_generation.checked_add(1).ok_or_else(|| {
                io::Error::other("process watch registration generations exhausted")
            })?;
            self.watch_generation = generation;
            let address = usize::try_from(generation).map_err(io::Error::other)?;
            change.udata = std::ptr::without_provenance_mut(address);
            let creation = read_watch_creation(pid, &mut self.watch_diagnostics);
            self.known[change.ident].watch = match creation {
                Some(creation) => Watch {
                    generation,
                    unique_id: creation.unique_id,
                    verified: true,
                    active: true,
                },
                None => Watch {
                    generation,
                    active: true,
                    ..Watch::default()
                },
            };
        }

        Ok(())
    }

    pub(super) fn submit_watches_verify(&mut self, returned: usize) -> io::Result<()> {
        if returned != self.changes.len() {
            return Err(io::Error::other(
                "process watch registration omitted receipts",
            ));
        }

        for (change, receipt) in self.changes.iter().zip(&self.eventlist[..returned]) {
            if receipt.ident != change.ident {
                return Err(io::Error::other("process watch receipt changed its PID"));
            }

            if receipt.filter != change.filter {
                return Err(io::Error::other("process watch receipt changed its filter"));
            }

            if receipt.udata != change.udata {
                return Err(io::Error::other(
                    "process watch receipt changed its registration",
                ));
            }

            let watch = &mut self.known[change.ident].watch;
            assert!(watch.active);
            assert_eq!(
                watch.generation,
                u64::try_from(change.udata.addr()).map_err(io::Error::other)?
            );
            if receipt.data != 0 {
                watch.active = false;
                watch.verified = false;
                continue;
            }

            let creation = read_watch_creation(pid_of(change.ident)?, &mut self.watch_diagnostics);
            watch.verify(creation);
        }

        Ok(())
    }
}

impl Watch {
    fn verify(&mut self, creation: Option<ProcessCreation>) {
        if self.verified {
            self.verified = match creation {
                Some(creation) => creation.unique_id == self.unique_id,
                None => false,
            };
        }
    }
}

fn read_watch_creation(pid: i32, diagnostics: &mut WatchDiagnostics) -> Option<ProcessCreation> {
    let mut creation = Err(crate::programs::CreationError::NotRead);
    crate::programs::read_creation(pid, false, &mut creation);
    diagnostics.reads += 1;
    match creation {
        Ok(creation) => Some(creation),
        Err(error) => {
            diagnostics.unavailable += 1;
            diagnostics.last_failure = Some(WatchFailure { pid, error });
            None
        }
    }
}

pub(super) fn notice_creation(known: &[Known], notice: &Kevent) -> io::Result<Option<u64>> {
    let index = known_index(pid_of(notice.ident)?, known.len())?;
    let watch = &known[index].watch;
    let generation = u64::try_from(notice.udata.addr()).map_err(io::Error::other)?;
    if watch.active {
        if watch.verified {
            if generation == watch.generation {
                return Ok(Some(watch.unique_id));
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::super::{Event, EventCreation, exec_identity, watch};
    use super::*;
    use crate::ring::{IdentityUnavailable, ProcessIdentity};

    #[test]
    fn excludes_a_reused_pids_creation_from_an_older_exec_notice() {
        // Goal: a stable acquisition of a replacement PID never identifies an older notice.
        // Method: compare two distinct source creation IDs at the association boundary.
        let mut acquired = ProcessIdentity::Unavailable {
            creation: Some(ProcessCreation {
                unique_id: 22,
                pid_version: 3,
            }),
            reason: IdentityUnavailable::Exhausted,
        };

        exec_identity(Some(11), &mut acquired);

        assert_eq!(
            ProcessIdentity::Unavailable {
                creation: None,
                reason: IdentityUnavailable::Changed,
            },
            acquired
        );
    }

    #[test]
    fn excludes_a_readable_replacement_creation_through_actual_follow_and_drain() {
        // Goal: the production follow path cannot bypass rejection of a different watched creation.
        // Method: seed an older association, then exec a readable held child and drain its notice.
        let observation =
            super::super::tests::start_tests::captured_exec_after_watch_change(|tracker, pid| {
                let index = known_index(pid, tracker.known.len()).unwrap();
                let watch = &mut tracker.known[index].watch;
                assert!(watch.active);
                assert!(watch.verified);
                watch.unique_id ^= 1;
            });

        assert_eq!(
            Event::Exec {
                pid: observation.identity.pid
            },
            observation.event
        );
        assert_eq!(
            EventCreation::Program(ProcessIdentity::Unavailable {
                creation: None,
                reason: IdentityUnavailable::Changed,
            }),
            observation.identity.creation
        );
    }

    #[test]
    fn rejects_an_old_registration_after_a_new_one_occupies_the_pid_slot() {
        // Goal: an older queued token cannot borrow a newer registration's verified creation.
        // Method: look up an old udata generation against a replacement watch in the PID slot.
        let mut known = [Known::default(); 2];
        known[1].watch = verified_watch();
        let mut notice = watch(1);
        notice.udata = std::ptr::without_provenance_mut(6);

        assert_eq!(None, notice_creation(&known, &notice).unwrap());
        notice.udata = std::ptr::without_provenance_mut(7);
        assert_eq!(Some(11), notice_creation(&known, &notice).unwrap());
    }

    #[test]
    fn rejects_a_registration_that_changed_creation_during_installation() {
        // Goal: a successful receipt does not override a changed or missing identity bracket.
        // Method: verify a registration with a different post-install creation ID.
        let mut watch = verified_watch();

        watch.verify(Some(ProcessCreation {
            unique_id: 22,
            pid_version: 3,
        }));

        assert!(!watch.verified);
    }

    #[test]
    fn accepts_exec_version_change_within_the_same_process_creation() {
        // Goal: an exec changes the image epoch without invalidating the watched process.
        // Method: verify a stable unique ID with a new version and retain that acquired version.
        let mut watch = verified_watch();
        let creation = ProcessCreation {
            unique_id: 11,
            pid_version: 99,
        };
        watch.verify(Some(creation));
        let mut acquired = ProcessIdentity::Unavailable {
            creation: Some(creation),
            reason: IdentityUnavailable::Exhausted,
        };

        exec_identity(Some(watch.unique_id), &mut acquired);

        assert!(watch.verified);
        assert_eq!(
            ProcessIdentity::Unavailable {
                creation: Some(creation),
                reason: IdentityUnavailable::Exhausted,
            },
            acquired
        );
    }

    #[test]
    fn excludes_metadata_without_a_verified_source_registration() {
        // Goal: unavailable watch identity cannot retain a current PID's creation indirectly.
        // Method: provide readable acquisition metadata without a source creation association.
        let mut acquired = ProcessIdentity::Unavailable {
            creation: Some(ProcessCreation {
                unique_id: 22,
                pid_version: 3,
            }),
            reason: IdentityUnavailable::Exhausted,
        };

        exec_identity(None, &mut acquired);

        assert_eq!(
            ProcessIdentity::Unavailable {
                creation: None,
                reason: IdentityUnavailable::Changed
            },
            acquired
        );
    }

    #[test]
    fn excludes_inactive_and_unverified_registration_tokens() {
        // Goal: a matching token alone cannot prove installation or a live watch association.
        // Method: invalidate each independent registration precondition around the same token.
        let mut known = [Known::default(); 2];
        let mut notice = watch(1);
        notice.udata = std::ptr::without_provenance_mut(7);
        known[1].watch = verified_watch();
        known[1].watch.active = false;
        assert_eq!(None, notice_creation(&known, &notice).unwrap());

        known[1].watch.active = true;
        known[1].watch.verified = false;
        assert_eq!(None, notice_creation(&known, &notice).unwrap());
    }

    #[test]
    fn an_unreadable_installation_bracket_cannot_be_repaired_by_a_later_read() {
        // Goal: one readable endpoint cannot stand in for a two-sided installation bracket.
        // Method: fail the first or last identity read, then offer a matching creation afterward.
        let mut watch = verified_watch();
        watch.verify(None);
        assert!(!watch.verified);

        watch.verify(Some(ProcessCreation {
            unique_id: 11,
            pid_version: 99,
        }));
        assert!(!watch.verified);
    }

    #[test]
    fn retains_the_raw_creation_failure_in_bounded_watch_diagnostics() {
        // Goal: registration identity failure remains observable without an allocating message.
        // Method: read an impossible PID and inspect the fixed diagnostic storage.
        let mut diagnostics = WatchDiagnostics::default();

        assert_eq!(None, read_watch_creation(i32::MAX, &mut diagnostics));

        assert_eq!(1, diagnostics.reads);
        assert_eq!(1, diagnostics.unavailable);
        let failure = diagnostics.last_failure.unwrap();
        assert_eq!(i32::MAX, failure.pid);
        assert_eq!(crate::programs::CreationError::Gone(3), failure.error);
    }

    fn verified_watch() -> Watch {
        Watch {
            generation: 7,
            unique_id: 11,
            verified: true,
            active: true,
        }
    }
}
