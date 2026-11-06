//! A window's ready directory and its proof, for tests of readers over tables no ring could
//! publish, such as tables that a test made wrong on purpose.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::recording::{identity_text, private};
use collector::ring::{Identity, ObjectBinding};
use polars::prelude::SortMultipleOptions;

use super::metadata::{Body, CanonicalLoss, CanonicalMetadata, EndpointEvidence, Lifecycle};
use super::{CanonicalKey, Namespace, RawBase, RawChild};
use super::{ReadWork, rows};
use crate::event_recovery::Batch;
use crate::event_table::index::Index;
use crate::resources::ByteBudget;
use crate::transaction::{CanonicalKind, LossTransfer, Predecessor};
use crate::worker::{Derivation, Derive};
use crate::{Encoder, Error, Manifest, Result, TableManifest, Window};

static NEXT: AtomicU64 = AtomicU64::new(1);

pub struct ReadyWindow {
    directory: PathBuf,
    transaction: Identity,
}

impl ReadyWindow {
    pub fn create(root: &Path) -> Result<Self> {
        let transaction = Identity([
            u64::from(std::process::id()),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ]);
        let attempt = root
            .join("data")
            .join(identity_text(transaction).to_string());
        private::create_private_dir(&attempt)?;
        let directory = attempt.join("ready");
        private::create_private_dir(&directory)?;
        Ok(Self {
            directory,
            transaction,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.directory
    }

    /// Moves the events a table holds to one batch in the order of their numbers, as every window
    /// keeps them.
    fn events_to_batch(&self, window: &Window, rows: u64) -> Result<()> {
        let events = self.directory.join("events");
        private::create_private_dir(&events)?;
        let single = self.directory.join("events.parquet");
        let range = publish_events(window);
        let (identities, batches) = if rows == 0 {
            fs::remove_file(&single)?;
            (
                window.ticks.iter().any(|tick| tick.identities.is_some()),
                Vec::new(),
            )
        } else {
            let mut ordered = crate::engine::scan(&single)?
                .sort(["event"], SortMultipleOptions::new())
                .collect()?;
            crate::encoder::encode_write(
                &mut ordered,
                private::open_private_file(
                    &events.join(crate::event_recovery::batch_name(0)),
                    true,
                )?,
            )?;
            fs::remove_file(&single)?;
            (
                ordered.column("identity_version").is_ok(),
                vec![Batch {
                    first: range.start,
                    end: range.end,
                    rows,
                }],
            )
        };
        Index {
            identity: window.identity,
            first: range.start,
            end: range.end,
            rows,
            identities,
            batches,
        }
        .write(&events.join("index"))
    }

    /// Writes the proof of `manifest`, which has no publication or event lost, and gives the
    /// manifest as a reader lists it. Nothing lists the window until `publish` links it, so a
    /// second name beside the window stands for the catalog name a read asks its proof to have.
    pub fn prove(&self, manifest: Manifest) -> Result<Manifest> {
        let manifest = self.prove_metadata(manifest)?.0;
        fs::hard_link(
            self.directory.join("metadata"),
            self.directory.with_file_name("proof"),
        )?;
        Ok(manifest)
    }

    /// Writes the proof of `manifest` and links it where a reader of the recording finds it, from
    /// the wall clocks of the saved ticks.
    pub fn publish(&self, root: &Path, manifest: Manifest) -> Result<Manifest> {
        let (manifest, metadata) = self.prove_metadata(manifest)?;
        let proof = self.directory.join("metadata");
        let budget = Arc::new(ByteBudget::new(1 << 28));
        let directory = private::PrivateDirectory::open(&self.directory)?;
        let mut keys = rows::observation_keys(
            &directory,
            &metadata,
            &budget,
            &mut ReadWork::default(),
            None,
        )?;
        keys.push((
            Namespace::Ordinary,
            CanonicalKey::source(
                Namespace::Ordinary,
                &metadata.run,
                metadata.canonical_origin,
            )?,
        ));
        for (namespace, key) in keys {
            let name = key.file_name();
            let leaf = root.join(namespace.name()).join(&name[..1]);
            match private::create_private_dir(&leaf) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            fs::hard_link(&proof, leaf.join(name))?;
        }

        Ok(manifest)
    }

    fn prove_metadata(&self, mut manifest: Manifest) -> Result<(Manifest, CanonicalMetadata)> {
        manifest.data_directory = format!("data/{}/ready", identity_text(self.transaction));
        let metadata = ordinary_metadata(self.transaction, &manifest);
        metadata.write(&mut private::open_private_file(
            &self.directory.join("metadata"),
            true,
        )?)?;

        Ok((manifest, metadata))
    }
}

/// The proof of `manifest`, which has no publication or event lost.
pub(crate) fn ordinary_metadata(transaction: Identity, manifest: &Manifest) -> CanonicalMetadata {
    CanonicalMetadata {
        run: manifest.identity.run,
        transaction,
        kind: CanonicalKind::Ordinary,
        base: RawBase::Data,
        child: RawChild::Ready,
        window: manifest.identity.window,
        attempted_origin: manifest.publication_first,
        canonical_origin: manifest.publication_first,
        original_end: manifest.publication_end,
        source_layout: 5,
        binding: ObjectBinding {
            identity: Identity([17, 18]),
            generation: 1,
            publication_first: 1,
            event_first: 1,
        },
        source_origin: Some(0),
        evidence: EndpointEvidence::FixedSource,
        lifecycle: Lifecycle::Active,
        captured: false,
        trailing_unknown: false,
        loss: CanonicalLoss {
            inherited: LossTransfer {
                predecessor: Predecessor::Genesis,
                publication_start: manifest.publication_first,
                publication_inherited_end: manifest.publication_first,
                event_start: manifest.events_first,
                event_inherited_end: manifest.events_first,
                publications: 0,
                events: 0,
                unknown: None,
                prior_window: None,
            },
            publication_end: manifest.publication_end,
            event_end: manifest.events_end,
            publications_new: 0,
            events_new: 0,
            unknown_flags: 0,
        },
        body: Body::Ordinary(manifest.clone()),
    }
}

/// Saves `window` where a reader of the recording at `root` finds it, as the persister would save
/// a window with these tables. `extend` may change the tables first, and what it says they hold.
pub fn publish_window(
    root: &Path,
    window: &Window,
    extend: impl FnOnce(&Path, &mut Vec<TableManifest>),
) -> Result<Manifest> {
    publish_window_extended(root, window, |path, tables| {
        extend(path, tables);
        Ok(())
    })
}

/// Derives through a derivation of its own each time, as a persister restarted before each minute.
pub struct Restarted;

impl Derive for Restarted {
    fn derive(&self, derivation: &Derivation<'_>) -> Result<Vec<TableManifest>> {
        crate::sealed_apps::SealedApps::default().derive(derivation)
    }
}

/// `publish_window` with the tables `derive` derives from the window.
pub fn publish_window_derived(
    root: &Path,
    window: &Window,
    derive: &dyn Derive,
) -> Result<Manifest> {
    publish_window_extended(root, window, |path, tables| {
        let directory = private::PrivateDirectory::open(path)?;
        tables.extend(derive.derive(&Derivation {
            window,
            pending: &directory,
            saved: crate::worker::SavedRecording {
                root,
                budget: &Arc::new(ByteBudget::new(256 * 1024 * 1024)),
            },
        })?);
        Ok(())
    })
}

fn publish_window_extended(
    root: &Path,
    window: &Window,
    extend: impl FnOnce(&Path, &mut Vec<TableManifest>) -> Result<()>,
) -> Result<Manifest> {
    let ready = ReadyWindow::create(root)?;
    let mut tables = Encoder.encode(window, ready.path())?;
    extend(ready.path(), &mut tables)?;
    let events = tables
        .iter()
        .find(|table| table.name == "events")
        .ok_or(Error::Bound("ready window without an events table"))?
        .rows;
    ready.events_to_batch(window, events)?;
    let mut manifest = crate::worker::seal_manifest(
        window,
        tables,
        crate::Losses::default(),
        u32::from(events != 0),
    );
    let range = publish_events(window);
    manifest.events_first = range.start;
    manifest.events_end = range.end;
    ready.publish(root, manifest)
}

/// The window's events, whose numbers start at one, as a tick that named none leaves them.
fn publish_events(window: &Window) -> std::ops::Range<u64> {
    let events = window.event_range();
    if events == (0..0) { 1..1 } else { events }
}
