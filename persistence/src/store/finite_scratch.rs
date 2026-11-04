//! Admission and retirement of the same two finite count-zero candidate universes.
use super::{BatchFact, FileIdentity, PublicationLedger, RegisteredAttempt};
use crate::resources::{ByteBudget, ByteReservation};
use collector::recording::private::{self, PrivateDirectory, PrivateNames};
use std::{io, os::unix::fs::MetadataExt, sync::Arc};

pub(super) struct FiniteScratch {
    facts: Vec<BatchFact>,
    _reservation: ByteReservation,
    budget: Arc<ByteBudget>,
    ledger: PublicationLedger,
    names: Option<PrivateNames>,
    names_candidate: usize,
    name_count: usize,
    dots: usize,
    position: usize,
    cleanup: usize,
    retired: usize,
}
impl FiniteScratch {
    pub fn new(
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
        facts: Option<(Vec<BatchFact>, ByteReservation)>,
    ) -> io::Result<Self> {
        let (facts, reservation, position) = if let Some((facts, reservation)) = facts {
            if facts.len() != 8192
                || facts.iter().any(|fact| {
                    matches!(
                        fact,
                        BatchFact::Complete { .. }
                            | BatchFact::Unprobed
                            | BatchFact::Malformed { framed: true, .. }
                    )
                })
            {
                return Err(invalid());
            }
            (facts, reservation, 8192)
        } else {
            let reservation = budget
                .reserve(8192 * size_of::<BatchFact>())
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
            (vec![BatchFact::Unprobed; 8192], reservation, 0)
        };
        Ok(Self {
            facts,
            _reservation: reservation,
            budget: Arc::clone(budget),
            ledger: ledger.clone(),
            names: None,
            names_candidate: 0,
            name_count: 0,
            dots: 0,
            position,
            cleanup: 0,
            retired: 0,
        })
    }
    pub fn admitted(&self) -> bool {
        self.names_candidate == 2 && self.position == 8192
    }
    pub fn cleaned(&self) -> bool {
        self.retired == 2
    }
    fn object(&self, attempt: &RegisteredAttempt<'_>) -> io::Result<PrivateDirectory> {
        super::inventory::census::io_owner::object(attempt, &self.ledger)
    }
    fn candidate(
        &self,
        object: &PrivateDirectory,
        candidate: usize,
    ) -> io::Result<Option<PrivateDirectory>> {
        self.ledger.update(|work| work.inventory.open_attempts += 1);
        match object.open_directory(&format!("fragment-{candidate}")) {
            Ok(directory) => Ok(Some(directory)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
    pub fn admit(&mut self, attempt: &RegisteredAttempt<'_>) -> io::Result<()> {
        let object = self.object(attempt)?;
        if self.names_candidate < 2 {
            return self.names(&object);
        }
        let candidate = self.position / 4096;
        if candidate == 2 {
            return Ok(());
        }
        let directory = self.candidate(&object, candidate)?;
        let limit = (self.position + 128).min((candidate + 1) * 4096);
        for position in self.position..limit {
            self.ledger.update(|work| work.inventory.positions += 1);
            let fact = if let Some(directory) = &directory {
                self.admit_file(directory, position % 4096, attempt.input.captured)?
            } else {
                BatchFact::Missing
            };
            self.facts[position] = fact;
            self.position = position + 1;
        }
        Ok(())
    }
    fn names(&mut self, object: &PrivateDirectory) -> io::Result<()> {
        let mut work = private::DirectoryWork::default();
        if self.names.is_none() {
            let Some(directory) = self.candidate(object, self.names_candidate)? else {
                self.names_candidate += 1;
                return Ok(());
            };
            let result = directory.names(&mut work);
            self.names_work(work);
            self.names = Some(result?);
            work = private::DirectoryWork::default();
        }
        let result = (|| {
            for _ in 0..128 {
                let mut dot = false;
                let found = self
                    .names
                    .as_mut()
                    .ok_or_else(invalid)?
                    .step(&mut work, |name| {
                        dot = matches!(name, "." | "..");
                        if dot {
                            if self.dots >= 2 {
                                return Err(invalid());
                            }
                        } else {
                            if self.name_count >= 4096 {
                                return Err(invalid());
                            }
                            super::attempt::batch_number(name)?;
                        }
                        Ok(())
                    })?;
                if !found {
                    self.names = None;
                    self.names_candidate += 1;
                    self.name_count = 0;
                    self.dots = 0;
                    return Ok(());
                }
                if dot {
                    self.dots += 1;
                } else {
                    self.name_count += 1;
                }
            }
            Ok(())
        })();
        self.names_work(work);
        result
    }
    fn names_work(&self, directory: private::DirectoryWork) {
        self.ledger.record(
            &super::PublicationWork {
                inventory: super::InventoryWork {
                    directory,
                    ..Default::default()
                },
                ..Default::default()
            },
            &mut super::PublicationWork::default(),
        );
    }
    fn admit_file(
        &self,
        directory: &PrivateDirectory,
        ordinal: usize,
        captured: bool,
    ) -> io::Result<BatchFact> {
        self.ledger.update(|work| work.inventory.open_attempts += 1);
        let mut file = match directory.open_file(&crate::event_recovery::batch_name(ordinal)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BatchFact::Missing),
            Err(error) => return Err(error),
        };
        self.ledger
            .update(|work| work.inventory.raw_admissions += 1);
        let identity = FileIdentity::read(&file)?;
        if file.metadata()?.nlink() != 1 {
            return Err(invalid());
        }
        let framed = super::inventory::parquet_framing(&mut file, identity.bytes())?;
        if FileIdentity::read(&file)? != identity {
            return Err(invalid());
        }
        if framed {
            let _workspace = self
                .budget
                .reserve(crate::event_recovery::batch::WORKING_BYTES)
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
            self.ledger.update(|work| {
                work.inventory.raw_file_clones += 1;
                work.proof_operations += 1;
            });
            let mut decoded = 0;
            let result = crate::event_recovery::read::read_original_batch_file(
                file.try_clone()?,
                captured,
                true,
                &self.budget,
                &mut decoded,
            );
            self.ledger
                .update(|work| work.inventory.decoded_bytes += decoded);
            result.map_err(crate::canonical::rows::source_error)?;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "surviving candidate contradicts finite count zero",
            ));
        }
        Ok(BatchFact::Malformed {
            file: identity,
            framed: false,
        })
    }
    pub fn cleanup(&mut self, attempt: &RegisteredAttempt<'_>) -> io::Result<()> {
        assert!(self.admitted());
        let object = self.object(attempt)?;
        let candidate = self.cleanup / 4096;
        if self.cleanup < 8192 {
            let directory = self.candidate(&object, candidate)?;
            let limit = (self.cleanup + 128).min((candidate + 1) * 4096);
            for position in self.cleanup..limit {
                self.ledger.update(|work| work.cleanup_positions += 1);
                if let Some(directory) = &directory {
                    self.remove(directory, position)?;
                }
            }
            if let Some(directory) = &directory {
                self.ledger.update(|work| work.full_syncs += 1);
                directory.full_sync()?;
            }
            self.cleanup = limit;
            return Ok(());
        }
        if self.retired < 2 {
            let name = format!("fragment-{}", self.retired);
            if let Some(directory) = self.candidate(&object, self.retired)? {
                let mut work = private::DirectoryWork::default();
                let result = directory.visit_names_counted(0, &mut work, |_| Err(invalid()));
                self.names_work(work);
                result?;
                self.ledger.update(|work| work.full_syncs += 1);
                directory.full_sync()?;
                self.ledger.update(|work| {
                    work.cleanup_directory_attempts += 1;
                    work.cleanup_removals += 1;
                });
                object.remove_directory(&name)?;
            }
            self.ledger.update(|work| work.full_syncs += 1);
            object.full_sync()?;
            self.retired += 1;
        }
        Ok(())
    }
    fn remove(&self, directory: &PrivateDirectory, position: usize) -> io::Result<()> {
        let name = crate::event_recovery::batch_name(position % 4096);
        self.ledger.update(|work| work.cleanup_file_attempts += 1);
        let mut file = match directory.open_file(&name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let BatchFact::Malformed {
            file: expected,
            framed: false,
        } = self.facts[position]
        else {
            return Err(invalid());
        };
        if FileIdentity::read(&file)? != expected
            || file.metadata()?.nlink() != 1
            || super::inventory::parquet_framing(&mut file, expected.bytes())?
        {
            return Err(invalid());
        }
        self.ledger.update(|work| work.cleanup_file_attempts += 1);
        if FileIdentity::read(&directory.open_file(&name)?)? != expected {
            return Err(invalid());
        }
        self.ledger.update(|work| work.cleanup_removals += 1);
        directory.remove_file(&name)
    }
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
