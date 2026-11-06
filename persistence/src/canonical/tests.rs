use super::metadata::Body;
use super::*;
mod native;
mod review;
use crate::transaction::{CanonicalKind, LossTransfer, Predecessor};
use collector::recording::{
    bootstrap,
    private::{self, FileLock, PrivateDirectory},
};
use collector::ring::{Identity, RunIdentity};
use std::io::Write;
use std::{
    fs, io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    base: PathBuf,
    root: PathBuf,
    run: RunIdentity,
}
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "fd-frontier-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        private::create_private_dir(&base).unwrap();
        let root = base.join("recording");
        private::create_private_dir(&root).unwrap();
        let directory = PrivateDirectory::open(&root).unwrap();
        let lock = FileLock::try_lock_in(&directory, "registration.lock").unwrap();
        let recording = bootstrap::initialize(&directory, &lock, &mut [0; 4097]).unwrap();
        Self {
            base,
            root,
            run: RunIdentity {
                recording,
                run: Identity([8, 9]),
                boot: Identity([10, 11]),
            },
        }
    }
    fn store(&self) -> crate::Store {
        crate::Store::open(&self.root).unwrap()
    }
    fn metadata(&self, kind: CanonicalKind) -> CanonicalMetadata {
        let base = if kind == CanonicalKind::Terminal {
            RawBase::Recovery
        } else {
            RawBase::Data
        };
        CanonicalMetadata {
            run: self.run,
            transaction: Identity([30, 31]),
            kind,
            base,
            child: if kind == CanonicalKind::LossOnly {
                RawChild::Loss
            } else if kind == CanonicalKind::Fragment {
                RawChild::Fragment0
            } else {
                RawChild::Ready
            },
            window: 0,
            attempted_origin: u64::from(base != RawBase::Recovery),
            canonical_origin: 0,
            original_end: if base == RawBase::Recovery { 0 } else { 2 },
            source_layout: 5,
            binding: collector::ring::ObjectBinding {
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
                    publication_start: 1,
                    publication_inherited_end: 1,
                    event_start: 1,
                    event_inherited_end: 1,
                    publications: 0,
                    events: 0,
                    unknown: None,
                    prior_window: None,
                },
                publication_end: 2,
                event_end: 2,
                publications_new: 1,
                events_new: 1,
                unknown_flags: 0,
            },
            body: Body::LossOnly,
        }
    }
    fn publish(
        &self,
        metadata: &CanonicalMetadata,
        namespace: Namespace,
        components: &[&str],
    ) -> (PathBuf, CanonicalKey) {
        let raw = self
            .root
            .join(metadata.base.name())
            .join(collector::recording::identity_text(metadata.transaction).to_string())
            .join(metadata.child.name());
        private::create_private_dir(raw.parent().unwrap()).unwrap();
        private::create_private_dir(&raw).unwrap();
        metadata
            .write(&mut private::open_private_file(&raw.join("metadata"), true).unwrap())
            .unwrap_or_else(|error| panic!("metadata {metadata:?}: {error:?}"));
        let origin = match namespace {
            Namespace::Ordinary => {
                if metadata.kind == CanonicalKind::Ordinary {
                    metadata.canonical_origin
                } else {
                    metadata.attempted_origin
                }
            }
            Namespace::Terminal => 0,
            Namespace::Fragments => metadata.loss.inherited.event_start,
            _ => panic!(),
        };
        let key = CanonicalKey::source(namespace, &metadata.run, origin).unwrap();
        let mut leaf = self.root.join(namespace.name());
        if components.is_empty() {
            leaf.push(&key.file_name()[..1]);
            private::create_private_dir(&leaf).unwrap();
        }
        for component in components {
            leaf.push(component);
            private::create_private_dir(&leaf).unwrap();
        }
        fs::hard_link(raw.join("metadata"), leaf.join(key.file_name())).unwrap();
        (raw, key)
    }
    fn frontier(&self) -> (SourceFrontier, ReadWork) {
        let mut work = ReadWork::default();
        let memory = budget();
        let frontier = self
            .store()
            .source_frontier(&self.run, &memory, &mut work)
            .unwrap()
            .unwrap();
        eprintln!(
            concat!(
                "frontier-observation publication_end={} event_end={} ",
                "reservation_peak={} work={work:?}"
            ),
            frontier.publication_end(),
            frontier.event_end(),
            memory.used_peak(),
            work = work
        );
        (frontier, work)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.base).unwrap();
    }
}
fn budget() -> Arc<crate::resources::ByteBudget> {
    Arc::new(crate::resources::ByteBudget::new(256 * 1024 * 1024))
}
fn loss_fixture() -> (Fixture, CanonicalMetadata) {
    let f = Fixture::new();
    let metadata = f.metadata(CanonicalKind::LossOnly);
    (f, metadata)
}

#[test]
fn a_window_listing_a_derived_table_in_place_of_one_of_its_tables_is_refused() {
    // Goal: a derived table never stands in for a table every window holds.
    // Method: list app ranks in place of the events table, so the count of tables holds.
    let mut manifest = proof_manifest();
    assert_eq!("events", manifest.tables.pop().unwrap().name);
    manifest.tables.push(crate::TableManifest {
        name: "app_ranks",
        rows: 3,
    });

    let metadata = ready_fixture::ordinary_metadata(Identity([30, 31]), &manifest);

    assert_eq!(
        metadata.validate().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
#[test]
fn a_window_listing_a_derived_table_twice_is_refused() {
    // Goal: a window holds each derived table at most once.
    // Method: list app ranks twice in the manifest of a proof.
    let mut manifest = proof_manifest();
    for _copy in 0..2 {
        manifest.tables.push(crate::TableManifest {
            name: "app_ranks",
            rows: 3,
        });
    }

    let metadata = ready_fixture::ordinary_metadata(Identity([30, 31]), &manifest);

    assert_eq!(
        metadata.validate().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
/// A manifest a proof accepts as it stands.
fn proof_manifest() -> crate::Manifest {
    let tables = crate::schema::TABLES
        .iter()
        .map(|table| crate::TableManifest {
            name: table.name(),
            rows: if table.name() == "events" { 4 } else { 3 },
        })
        .collect();
    let mut manifest = crate::Manifest {
        identity: crate::WindowIdentity {
            run: RunIdentity {
                recording: Identity([1, 2]),
                run: Identity([3, 4]),
                boot: Identity([5, 6]),
            },
            window: 17,
        },
        publication_first: 20,
        publication_end: 23,
        events_first: 90,
        events_end: 94,
        wall_ns: None,
        continuous_ns: None,
        awake_ns: None,
        losses: crate::Losses::default(),
        data_directory: String::new(),
        tables,
        event_batches: 1,
    };
    manifest.data_directory = format!(
        "data/{}/ready",
        collector::recording::identity_text(Identity([30, 31]))
    );
    ready_fixture::ordinary_metadata(Identity([30, 31]), &manifest)
        .validate()
        .unwrap();

    manifest
}
/// The bytes of the manifest every window now proves, whose last word counts its event batches and
/// whose third word names its encoding.
fn manifest_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    proof_manifest().write(&mut bytes).unwrap();

    bytes
}

#[test]
fn a_manifest_of_the_current_encoding_reads_back() {
    // Goal: the refusals below turn on the words they change and on nothing else.
    // Method: read a manifest as it was written.
    let bytes = manifest_bytes();

    let manifest = crate::Manifest::read(&mut bytes.as_slice()).unwrap();

    assert_eq!(manifest, proof_manifest());
}

#[test]
fn a_manifest_naming_no_event_batches_is_refused() {
    // Goal: a window keeps its events in batches, so no manifest may say it holds one events file.
    // Method: write the word that used to mean one events file over the count of batches.
    let mut bytes = manifest_bytes();
    let count = bytes.len() - size_of::<u64>();
    bytes[count..].copy_from_slice(&u64::MAX.to_le_bytes());

    let error = crate::Manifest::read(&mut bytes.as_slice()).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn a_manifest_of_the_older_encoding_is_refused() {
    // Goal: the encoding that had no count of batches is no longer a way to read a window.
    // Method: rewrite a manifest as that encoding wrote it, naming it and leaving the count out.
    let mut bytes = manifest_bytes();
    bytes.truncate(bytes.len() - size_of::<u64>());
    bytes[16..24].copy_from_slice(&1_u64.to_le_bytes());

    let error = crate::Manifest::read(&mut bytes.as_slice()).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn loss_only_tail_reopens_with_idle_slots_and_no_derived_index() {
    // Goal: canonical gate metadata alone establishes exact ownership.
    // Method: install an encoded gate and query through Store without any accelerator.
    let (fixture, metadata) = loss_fixture();
    let (raw, _) = fixture.publish(&metadata, Namespace::Ordinary, &[]);
    let bytes = fs::read(raw.join("metadata")).unwrap();
    assert_eq!(bytes.len(), 512);
    let (frontier, work) = fixture.frontier();
    assert_eq!(frontier.publication_end(), 2);
    assert_eq!(frontier.event_end(), 2);
    assert_eq!(frontier.prior_window(), None);
    assert_eq!(
        frontier.predecessor(),
        &Predecessor::Canonical {
            transaction: metadata.transaction,
            kind: CanonicalKind::LossOnly,
            origin: 1
        }
    );
    assert_eq!(work.metadata_admissions, 1);
    assert_eq!(work.raw_admissions, 1);
    assert!(work.entries <= 128);
    assert!(work.namespace_attempts > work.node_attempts);
    assert_eq!(fs::read(raw.join("metadata")).unwrap(), bytes);
}
#[test]
fn exact_source_absence_ignores_unavailable_other_source_payload() {
    // Goal: another source's history cannot force a global-empty genesis refusal.
    // Method: retain an unrelated gate while deleting its raw source, then query a distinct boot.
    let (fixture, metadata) = loss_fixture();
    let (raw, _) = fixture.publish(&metadata, Namespace::Ordinary, &[]);
    fs::remove_dir_all(raw).unwrap();
    let mut run = fixture.run;
    run.boot = Identity([90, 91]);
    let mut work = ReadWork::default();
    assert!(
        fixture
            .store()
            .source_frontier(&run, &budget(), &mut work)
            .unwrap()
            .is_none()
    );
    assert_eq!(work.metadata_admissions, 0);
    assert_eq!(work.raw_admissions, 0);
}
#[test]
fn a_damaged_recording_is_explicit_corruption_and_not_another_format() {
    // Goal: a reader that takes `Unsupported` for an older format cannot read damage as no minutes.
    // Method: remove a directory every initialized recording holds, then open its catalog.
    let (fixture, _) = loss_fixture();
    fs::remove_dir(fixture.root.join("event-fragments")).unwrap();

    let opened = catalog::Catalog::open(&fixture.root, &budget());

    assert_eq!(opened.err().unwrap().kind(), io::ErrorKind::InvalidData);
}
macro_rules! a_root_no_writer_finished_setting_up_holds_no_recording {
    ($($name:ident: $prepare:expr,)+) => {$(
        #[test]
        fn $name() {
            // Goal: a reader can tell a recording still to come from one it must refuse.
            // Method: leave a root at one stage of its setup, then open its catalog.
            let root = std::env::temp_dir().join(format!(
                "fd-unfinished-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            private::create_private_dir(&root).unwrap();
            let prepare: fn(&std::path::Path) = $prepare;
            prepare(&root);

            let opened = catalog::Catalog::open(&root, &budget());

            assert_eq!(opened.err().unwrap().kind(), io::ErrorKind::NotFound);
            fs::remove_dir_all(root).unwrap();
        }
    )+};
}

a_root_no_writer_finished_setting_up_holds_no_recording! {
    a_root_with_nothing_in_it_holds_no_recording_yet: |_root| {},
    a_root_with_only_its_setup_record_holds_no_recording_yet: |root| {
        let directory = PrivateDirectory::open(root).unwrap();
        let mut bytes = [0; 64];
        bootstrap::codec::encode_bootstrap(Identity([1, 2]), &mut bytes).unwrap();
        private::atomic_write_in(&directory, "bootstrap", &bytes).unwrap();
    },
    a_root_whose_setup_stopped_after_its_identity_holds_no_recording_yet: |root| {
        let directory = PrivateDirectory::open(root).unwrap();
        let lock = FileLock::try_lock_in(&directory, "registration.lock").unwrap();
        let recording = bootstrap::initialize(&directory, &lock, &mut [0; 4097]).unwrap();
        let mut bytes = [0; 64];
        bootstrap::codec::encode_bootstrap(recording, &mut bytes).unwrap();
        private::atomic_write_in(&directory, "bootstrap", &bytes).unwrap();
    },
}

#[test]
fn a_recording_of_an_older_format_is_refused_as_unsupported() {
    // Goal: no reader of this version takes a recording it cannot read for an empty one.
    // Method: lay out a root as the previous format left it, then open its catalog.
    let root = std::env::temp_dir().join(format!(
        "fd-older-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    private::create_private_dir(&root).unwrap();
    let identity =
        b"flightdeck-recording-1\n00000001\n00000001\n00000000000000010000000000000002\n";
    private::atomic_write(&root, "identity", identity).unwrap();

    let opened = catalog::Catalog::open(&root, &budget());

    assert_eq!(opened.err().unwrap().kind(), io::ErrorKind::Unsupported);
    assert_eq!(fs::read(root.join("identity")).unwrap(), identity);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn raw_metadata_substitution_is_explicit_corruption() {
    // Goal: a path with equal bytes cannot replace the exact gate source inode.
    // Method: replace raw metadata with a private identical copy after linking its gate.
    let (fixture, metadata) = loss_fixture();
    let (raw, _) = fixture.publish(&metadata, Namespace::Ordinary, &[]);
    let path = raw.join("metadata");
    let bytes = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    private::open_private_file(&path, true)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    let mut work = ReadWork::default();
    assert_eq!(
        fixture
            .store()
            .source_frontier(&fixture.run, &budget(), &mut work)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(work.raw_admissions, 1);
}
#[test]
fn loss_metadata_rejects_corrupt_prefix_equations_and_reserved_bytes() {
    // Goal: separate prefix facts cannot disagree with canonical loss ownership.
    // Method: corrupt independent fields of an encoded object, then use real Store admission.
    let (fixture, metadata) = loss_fixture();
    let (raw, _) = fixture.publish(&metadata, Namespace::Ordinary, &[]);
    let bytes = fs::read(raw.join("metadata")).unwrap();
    for offset in [
        0, 8, 12, 16, 80, 84, 88, 120, 144, 152, 168, 172, 176, 180, 184, 188, 192, 264, 268, 304,
        312, 320, 328, 344, 352, 360, 368, 384, 424, 432, 436, 511,
    ] {
        let mut altered = bytes.clone();
        altered[offset] ^= 128;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(raw.join("metadata"))
            .unwrap();
        file.write_all(&altered).unwrap();
        assert!(
            fixture
                .store()
                .source_frontier(&fixture.run, &budget(), &mut ReadWork::default())
                .is_err(),
            "offset {offset}"
        );
    }
}
#[test]
fn terminal_loss_predecessor_uses_origin_zero() {
    // Goal: terminal loss is distinct from a fabricated ordinary publication origin.
    // Method: install kind4/base2 at the terminal key and read the typed predecessor.
    let (fixture, mut metadata) = loss_fixture();
    metadata.base = RawBase::Recovery;
    metadata.attempted_origin = 0;
    metadata.original_end = 0;
    fixture.publish(&metadata, Namespace::Terminal, &[]);
    let (frontier, _) = fixture.frontier();
    assert_eq!(
        frontier.predecessor(),
        &Predecessor::Canonical {
            transaction: metadata.transaction,
            kind: CanonicalKind::LossOnly,
            origin: 0
        }
    );
}

fn event_metadata(fixture: &Fixture, fragment: bool) -> CanonicalMetadata {
    let mut metadata = fixture.metadata(if fragment {
        CanonicalKind::Fragment
    } else {
        CanonicalKind::Terminal
    });
    metadata.loss.events_new = 0;
    metadata.body = Body::Events(crate::event_recovery::Manifest {
        recovery: crate::event_recovery::Recovery {
            run: fixture.run,
            events_first: 1,
            events_end: 2,
            rows: 1,
            losses: crate::Losses {
                publications: 1,
                events: 0,
                unknown: false,
            },
        },
        identities: false,
        batches: vec![crate::event_recovery::Batch {
            first: 1,
            end: 2,
            rows: 1,
        }],
    });
    metadata
}
fn event_frame(associated: bool) -> polars::prelude::DataFrame {
    event_frame_of(associated, 1)
}
fn event_frame_of(associated: bool, number: u64) -> polars::prelude::DataFrame {
    crate::event_recovery::batch::frame(
        &[crate::event_recovery::batch::Row {
            publication: associated.then_some(1),
            event: crate::window::EventRecord {
                number,
                tick: 1,
                pid: 41,
                kind: 1,
                coalition: None,
                usage: None,
                availability: 3,
                noticed: None,
            },
            identity: None,
            name: None,
            app: None,
        }],
        false,
    )
    .unwrap()
}
#[test]
fn terminal_and_associated_fragment_preserve_the_same_encoded_event_bytes() {
    // Goal: fragment admission preserves associated rows while terminal admission forbids them.
    // Method: encode real bounded event files and admit each through its proper canonical gate.
    for fragment in [false, true] {
        let fixture = Fixture::new();
        let metadata = event_metadata(&fixture, fragment);
        let (raw, _) = fixture.publish(
            &metadata,
            if fragment {
                Namespace::Fragments
            } else {
                Namespace::Terminal
            },
            &[],
        );
        let directory = PrivateDirectory::open(&raw).unwrap();
        crate::encoder::encode_write(
            &mut event_frame(fragment),
            directory.create_file("0000.parquet").unwrap(),
        )
        .unwrap();
        let before = fs::read(raw.join("0000.parquet")).unwrap();
        let (frontier, work) = fixture.frontier();
        assert_eq!(frontier.event_end(), 2);
        assert_eq!(work.raw_admissions, 2);
        assert_eq!(before, fs::read(raw.join("0000.parquet")).unwrap());
    }
}
#[test]
fn terminal_rejects_a_valid_associated_fragment_row() {
    // Goal: the shared validator does not silently erase terminal's null association rule.
    // Method: use identical event bytes behind an actual terminal metadata gate.
    let fixture = Fixture::new();
    let metadata = event_metadata(&fixture, false);
    let (raw, _) = fixture.publish(&metadata, Namespace::Terminal, &[]);
    let directory = PrivateDirectory::open(&raw).unwrap();
    crate::encoder::encode_write(
        &mut event_frame(true),
        directory.create_file("0000.parquet").unwrap(),
    )
    .unwrap();
    assert!(
        fixture
            .store()
            .source_frontier(&fixture.run, &budget(), &mut ReadWork::default())
            .is_err()
    );
}

fn ordinary(fixture: &Fixture) -> (CanonicalMetadata, PathBuf) {
    let mut metadata = fixture.metadata(CanonicalKind::Ordinary);
    metadata.canonical_origin = 13;
    metadata.attempted_origin = 12;
    metadata.original_end = 14;
    metadata.window = 10;
    metadata.loss.inherited.publication_start = 10;
    metadata.loss.inherited.publication_inherited_end = 12;
    metadata.loss.inherited.publications = 2;
    metadata.loss.publications_new = 1;
    metadata.loss.publication_end = 14;
    metadata.loss.inherited.event_start = 100;
    metadata.loss.inherited.event_inherited_end = 100;
    metadata.loss.event_end = 100;
    metadata.loss.events_new = 0;
    metadata.loss.inherited.prior_window = Some(9);
    metadata.loss.inherited.predecessor = Predecessor::Canonical {
        transaction: Identity([20, 21]),
        kind: CanonicalKind::Ordinary,
        origin: 1,
    };
    ordinary_encoded(fixture, metadata)
}
fn ordinary_encoded(
    fixture: &Fixture,
    mut metadata: CanonicalMetadata,
) -> (CanonicalMetadata, PathBuf) {
    let window = ordinary_window(&metadata);
    let identity = window.identity;
    let relative = format!(
        "data/{}/ready",
        collector::recording::identity_text(metadata.transaction)
    );
    let raw = fixture.root.join(&relative);
    private::create_private_dir(raw.parent().unwrap()).unwrap();
    private::create_private_dir(&raw).unwrap();
    let tables = crate::Encoder.encode(&window, &raw).unwrap();
    let events = PrivateDirectory::open(&raw)
        .unwrap()
        .create_directory("events")
        .unwrap();
    window
        .event_index
        .as_ref()
        .unwrap()
        .write_file(&mut events.create_file("index").unwrap())
        .unwrap();
    metadata.body = Body::Ordinary(crate::Manifest {
        identity,
        publication_first: metadata.canonical_origin,
        publication_end: metadata.original_end,
        events_first: metadata.loss.inherited.event_start,
        events_end: metadata.loss.event_end,
        wall_ns: None,
        continuous_ns: None,
        awake_ns: None,
        losses: crate::Losses {
            publications: metadata.loss.inherited.publications + metadata.loss.publications_new,
            events: metadata.loss.inherited.events + metadata.loss.events_new,
            unknown: false,
        },
        data_directory: relative,
        tables,
        event_batches: 0,
    });
    (metadata, raw)
}
#[test]
fn ordinary_admission_uses_full_owned_origin_and_actual_raw_tick_count() {
    // Goal: attempted origin, first retained tick and inherited loss remain distinct.
    // Method: encode an ordinary set with losses before p and between p/q, then read its tail.
    let fixture = Fixture::new();
    let (metadata, raw) = ordinary(&fixture);
    let before = fs::read(raw.join("ticks.parquet")).unwrap();
    fixture.publish(&metadata, Namespace::Ordinary, &[]);
    let (frontier, _) = fixture.frontier();
    assert_eq!(frontier.publication_end(), 14);
    assert_eq!(frontier.event_end(), 100);
    assert_eq!(frontier.prior_window(), Some(10));
    assert_eq!(
        frontier.predecessor(),
        &Predecessor::Canonical {
            transaction: metadata.transaction,
            kind: CanonicalKind::Ordinary,
            origin: 13
        }
    );
    assert_eq!(before, fs::read(raw.join("ticks.parquet")).unwrap());
    // An unrelated scalar table is not needed to establish source progress.
    fs::remove_file(raw.join("memory.parquet")).unwrap();
    assert_eq!(fixture.frontier().0.publication_end(), 14);
}
#[test]
fn gate_aliases_charge_one_owner_and_keep_last_saved_window_through_a_gap() {
    // Goal: canonical aliases do not duplicate loss ownership or advance a saved window.
    // Method: link one kind4 inode into its ordinary and event routes and read the source frontier.
    let (fixture, mut metadata) = loss_fixture();
    metadata.loss.inherited.predecessor = Predecessor::Canonical {
        transaction: Identity([20, 21]),
        kind: CanonicalKind::Ordinary,
        origin: 1,
    };
    metadata.loss.inherited.prior_window = Some(9);
    metadata.window = 10;
    let (raw, key) = fixture.publish(&metadata, Namespace::Ordinary, &[]);
    let event_key = CanonicalKey::source(Namespace::Fragments, &fixture.run, 1).unwrap();
    fs::hard_link(raw.join("metadata"), {
        let leaf = fixture
            .root
            .join("event-fragments")
            .join(&event_key.file_name()[..1]);
        private::create_private_dir(&leaf).unwrap();
        leaf.join(event_key.file_name())
    })
    .unwrap();
    let (frontier, work) = fixture.frontier();
    assert_eq!(frontier.prior_window(), Some(9));
    assert_eq!(frontier.publication_end(), 2);
    assert_eq!(frontier.event_end(), 2);
    assert_eq!(work.metadata_admissions, 2);
    assert_eq!(key.file_name().len(), 85);
}
#[test]
fn reader_snapshot_expires_after_a_real_namespace_state_replacement() {
    // Goal: a continuation cannot mix pages from different canonical snapshots.
    // Method: return a key, replace Quiet with a fresh inode, then ask for apparent End.
    let (fixture, metadata) = loss_fixture();
    fixture.publish(&metadata, Namespace::Ordinary, &[]);
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let mut state = [0; 256];
    let memory = budget();
    let mut cursor = CanonicalCursor::begin(
        &root,
        fixture.run.recording,
        Namespace::Ordinary,
        SeekRange::source_tail(Namespace::Ordinary, &fixture.run).unwrap(),
        &mut state,
        &memory,
    )
    .unwrap();
    assert!(matches!(cursor.step().unwrap(), ReadStep::Found(_)));
    let mut replacement = [0; 256];
    bootstrap::codec::encode_state(
        fixture.run.recording,
        &bootstrap::codec::NamespaceState::Quiet { sequence: 1 },
        &mut replacement,
    )
    .unwrap();
    private::atomic_write_in(&root, "namespace-state", &replacement).unwrap();
    assert_eq!(cursor.step().unwrap_err().kind(), io::ErrorKind::WouldBlock);
}
#[test]
fn deepest_known_key_lookup_continues_under_the_original_snapshot() {
    // Goal: traversal handles the full120-nibble shape without retaining ancestor descriptors.
    // Method: install single-digit wrappers and assert bounded passes across the reopen budget.
    let fixture = Fixture::new();
    let expected = CanonicalKey::from_bytes(Namespace::Known, &[0x22; 60]).unwrap();
    let path = fixture.root.join("observations-known");
    let mut leaf = path.clone();
    for _ in 0..120 {
        leaf.push("2");
        private::create_private_dir(&leaf).unwrap();
    }
    private::open_private_file(&leaf.join(expected.file_name()), true)
        .unwrap()
        .write_all(b"tree-only fixture")
        .unwrap();
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let memory = budget();
    let mut state = [0; 256];
    let mut cursor = CanonicalCursor::begin(
        &root,
        fixture.run.recording,
        Namespace::Known,
        SeekRange {
            first: expected,
            last: expected,
            direction: Direction::Reverse,
        },
        &mut state,
        &memory,
    )
    .unwrap();
    let mut continued = false;
    let mut found = false;
    for _ in 0..8 {
        let before = cursor.work();
        let step = cursor.step().unwrap();
        let after = cursor.work();
        assert!(after.node_attempts - before.node_attempts <= 4096);
        assert!(after.entries - before.entries <= 4224);
        match step {
            ReadStep::Continue => continued = true,
            ReadStep::Found(key) => {
                assert_eq!(key, expected);
                found = true;
                break;
            }
            ReadStep::End => panic!("false absence"),
        }
    }
    eprintln!("deep-key-observation work={:?}", cursor.work());
    assert!(continued);
    assert!(found);
}
#[test]
fn leaf_128_and_sixteen_way_branch_have_bounded_exact_ranges() {
    // Goal: stable boundary shapes return independently expected source keys.
    // Method: fill one128-link leaf and a separate sixteen-child branch, then seek both directions.
    let fixture = Fixture::new();
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let leaf = root
        .open_directory("ordinary")
        .unwrap()
        .create_directory("0")
        .unwrap();
    for origin in 1..=128 {
        let key = CanonicalKey::source(Namespace::Ordinary, &fixture.run, origin).unwrap();
        leaf.create_file(&key.file_name()).unwrap();
    }
    let first = CanonicalKey::source(Namespace::Ordinary, &fixture.run, 17).unwrap();
    let last = CanonicalKey::source(Namespace::Ordinary, &fixture.run, 23).unwrap();
    let memory = budget();
    let mut state = [0; 256];
    let mut cursor = CanonicalCursor::begin(
        &root,
        fixture.run.recording,
        Namespace::Ordinary,
        SeekRange {
            first,
            last,
            direction: Direction::Forward,
        },
        &mut state,
        &memory,
    )
    .unwrap();
    for origin in 17..=23 {
        let expected = CanonicalKey::source(Namespace::Ordinary, &fixture.run, origin).unwrap();
        assert!(matches!(cursor.step().unwrap(), ReadStep::Found(key) if key == expected));
    }
    assert!(matches!(cursor.step().unwrap(), ReadStep::End));
    assert_eq!(cursor.work().entries, 129);
    let branch = root.open_directory("observations-unknown").unwrap();
    for digit in 0_u8..16 {
        let name = format!("{digit:x}");
        let child = branch.create_directory(&name).unwrap();
        let key = CanonicalKey::from_bytes(Namespace::Unknown, &[digit << 4; 48]).unwrap();
        child.create_file(&key.file_name()).unwrap();
    }
    let first = CanonicalKey::from_bytes(Namespace::Unknown, &[0; 48]).unwrap();
    let last = CanonicalKey::from_bytes(Namespace::Unknown, &[255; 48]).unwrap();
    let mut state = [0; 256];
    let mut cursor = CanonicalCursor::begin(
        &root,
        fixture.run.recording,
        Namespace::Unknown,
        SeekRange {
            first,
            last,
            direction: Direction::Reverse,
        },
        &mut state,
        &memory,
    )
    .unwrap();
    let expected = CanonicalKey::from_bytes(Namespace::Unknown, &[240; 48]).unwrap();
    assert!(matches!(cursor.step().unwrap(), ReadStep::Found(key) if key == expected));
    assert!(cursor.work().entries <= 2112);
}

fn ordinary_window(metadata: &CanonicalMetadata) -> crate::Window {
    let shape = collector::ring::Shape {
        slot_count: 1,
        cpu_capacity: 1,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: 1,
        coalition_capacity: 1,
        event_capacity: 1,
    };
    let identity = crate::WindowIdentity {
        run: metadata.run,
        window: metadata.window,
    };
    let mut window = crate::Window::new(identity, crate::Limits::from_shape(&shape).unwrap());
    let mut tick = crate::tick_buffer(&shape).unwrap();
    tick.run_tick = metadata.window * 60;
    tick.events = metadata.loss.inherited.event_start..metadata.loss.event_end;
    window
        .push_tick(
            &crate::Observation {
                publication: metadata.canonical_origin,
                wall_ns: None,
                continuous_ns: None,
                awake_ns: None,
            },
            &tick,
            &[],
        )
        .unwrap();
    window.event_index = Some(crate::event_table::index::Index {
        identity,
        first: metadata.loss.inherited.event_start,
        end: metadata.loss.event_end,
        rows: 0,
        identities: false,
        batches: vec![],
    });
    window
}

#[test]
fn failed_raw_admission_cannot_be_retried_as_source_absence() {
    // Goal: a read error cannot advance a cursor past valid but unreadable canonical data.
    // Method: fail a selected raw open, repair it, and require a fresh snapshot query.
    let (fixture, metadata) = loss_fixture();
    let (raw, _) = fixture.publish(&metadata, Namespace::Ordinary, &[]);
    fs::rename(&raw, raw.with_file_name("hidden")).unwrap();
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let memory = budget();
    let mut state = [0; 256];
    let mut query = SourceFrontierRead::begin(&root, fixture.run, &mut state, &memory).unwrap();
    assert!(query.step().is_err());
    fs::rename(raw.with_file_name("hidden"), &raw).unwrap();
    assert!(query.step().is_err());
    assert!(query.result().is_err());
    assert_eq!(fixture.frontier().0.event_end(), 2);
}
#[test]
fn canonical_frontier_uses_descriptor_locators_at_the_longest_root() {
    // Goal: canonical raw admission retains the original1023-byte root limit.
    // Method: prepare a synthetic canonical set, relocate its root, then enter actual Store.
    let (mut fixture, metadata) = loss_fixture();
    fixture.publish(&metadata, Namespace::Ordinary, &[]);
    let mut target = fixture.base.canonicalize().unwrap();
    for _ in 0..8 {
        let remaining = 1023 - target.as_os_str().len();
        assert!(remaining > 1);
        let length = (remaining - 1).min(200);
        target.push("z".repeat(length));
        if target.as_os_str().len() == 1023 {
            break;
        }
        private::create_private_dir(&target).unwrap();
    }
    assert_eq!(target.as_os_str().len(), 1023);
    fs::rename(&fixture.root, &target).unwrap();
    fixture.root = target;
    assert_eq!(fixture.frontier().0.publication_end(), 2);
}
#[test]
fn overfull_leaf_counts_the_rejected_name_and_never_reports_end() {
    // Goal: an invalid stable leaf cannot bypass the128-reference bound.
    // Method: provide129 links and inspect the owning traversal's failed-operation counts.
    let fixture = Fixture::new();
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let leaf = root
        .open_directory("ordinary")
        .unwrap()
        .create_directory("0")
        .unwrap();
    for origin in 1..=129 {
        leaf.create_file(
            &CanonicalKey::source(Namespace::Ordinary, &fixture.run, origin)
                .unwrap()
                .file_name(),
        )
        .unwrap();
    }
    let memory = budget();
    let mut state = [0; 256];
    let mut cursor = CanonicalCursor::begin(
        &root,
        fixture.run.recording,
        Namespace::Ordinary,
        SeekRange::source_tail(Namespace::Ordinary, &fixture.run).unwrap(),
        &mut state,
        &memory,
    )
    .unwrap();
    assert_eq!(
        cursor.step().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(cursor.work().entries, 130);
    assert!(cursor.work().native_entries >= 130);
    assert!(cursor.work().directory_reads > 0);
}
#[test]
fn terminal_and_fragment_tails_cannot_cross_ownership_frontiers() {
    // Goal: independently typed tails cannot invent a combined frontier from conflicting domains.
    // Method: expose a publication-ahead tail and an event-ahead tail with overlapping ownership.
    let fixture = Fixture::new();
    let mut terminal = fixture.metadata(CanonicalKind::LossOnly);
    terminal.base = RawBase::Recovery;
    terminal.attempted_origin = 0;
    terminal.original_end = 0;
    terminal.loss.publication_end = 3;
    terminal.loss.publications_new = 2;
    fixture.publish(&terminal, Namespace::Terminal, &[]);
    let mut fragment = event_metadata(&fixture, true);
    fragment.transaction = Identity([32, 33]);
    fragment.loss.event_end = 3;
    fragment.loss.events_new = 1;
    if let Body::Events(inner) = &mut fragment.body {
        inner.recovery.events_end = 3;
        inner.recovery.losses.events = 1;
    }
    let (raw, _) = fixture.publish(&fragment, Namespace::Fragments, &[]);
    crate::encoder::encode_write(
        &mut event_frame(true),
        PrivateDirectory::open(&raw)
            .unwrap()
            .create_file("0000.parquet")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        fixture
            .store()
            .source_frontier(&fixture.run, &budget(), &mut ReadWork::default())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn nothing_follows_a_terminal_kept_without_its_source() {
    // Goal: a terminal kept when its source went ends its run as a certified one does, so a
    // frontier refuses an object past it rather than reading the run as going on.
    // Method: publish a kept terminal and a fragment that starts where it ends, and read the
    // frontier.
    let fixture = Fixture::new();
    let mut terminal = event_metadata(&fixture, false);
    terminal.evidence = EndpointEvidence::SurvivingRow;
    assert!(terminal.kept_terminal());
    let (raw, _) = fixture.publish(&terminal, Namespace::Terminal, &[]);
    crate::encoder::encode_write(
        &mut event_frame(false),
        PrivateDirectory::open(&raw)
            .unwrap()
            .create_file("0000.parquet")
            .unwrap(),
    )
    .unwrap();
    let mut fragment = event_metadata(&fixture, true);
    fragment.transaction = Identity([32, 33]);
    fragment.loss.inherited.publication_start = 2;
    fragment.loss.inherited.publication_inherited_end = 2;
    fragment.loss.inherited.event_start = 2;
    fragment.loss.inherited.event_inherited_end = 2;
    fragment.loss.publication_end = 3;
    fragment.loss.event_end = 3;
    if let Body::Events(inner) = &mut fragment.body {
        inner.recovery.events_first = 2;
        inner.recovery.events_end = 3;
        inner.batches[0].first = 2;
        inner.batches[0].end = 3;
    }
    let (raw, _) = fixture.publish(&fragment, Namespace::Fragments, &[]);
    crate::encoder::encode_write(
        &mut event_frame_of(true, 2),
        PrivateDirectory::open(&raw)
            .unwrap()
            .create_file("0000.parquet")
            .unwrap(),
    )
    .unwrap();

    let result = fixture
        .store()
        .source_frontier(&fixture.run, &budget(), &mut ReadWork::default());

    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn unknown_provenance_and_trailing_extent_never_certify_completion() {
    // Goal: numeric coverage is independent of unknown endpoint and provenance.
    // Method: admit a bounded terminal prefix with an earlier-generation unknown fact.
    let fixture = Fixture::new();
    let mut metadata = event_metadata(&fixture, false);
    metadata.trailing_unknown = true;
    metadata.loss.unknown_flags = 1;
    metadata.loss.inherited.unknown = Some(crate::transaction::UnknownProvenance {
        reason: crate::transaction::UnknownReason::Predecessor,
        run: Identity([70, 71]),
        boot: Identity([72, 73]),
    });
    if let Body::Events(inner) = &mut metadata.body {
        inner.recovery.losses.unknown = true;
    }
    let (raw, _) = fixture.publish(&metadata, Namespace::Terminal, &[]);
    crate::encoder::encode_write(
        &mut event_frame(false),
        PrivateDirectory::open(&raw)
            .unwrap()
            .create_file("0000.parquet")
            .unwrap(),
    )
    .unwrap();
    let (frontier, _) = fixture.frontier();
    assert_eq!(frontier.event_end(), 2);
    assert!(frontier.trailing_unknown());
    assert!(!frontier.complete());
    assert_eq!(
        frontier.unknown_provenance().unwrap().run,
        Identity([70, 71])
    );
}

#[test]
fn earlier_unknown_provenance_does_not_replace_or_erase_final_endpoint_evidence() {
    // Goal: an earlier unknown generation stays distinct from this source's certified final end.
    // Method: encode the independent fields and retain both through canonical admission.
    let fixture = Fixture::new();
    let mut metadata = event_metadata(&fixture, false);
    metadata.lifecycle = Lifecycle::AcquiredCompletion;
    metadata.loss.unknown_flags = 1;
    metadata.loss.inherited.unknown = Some(crate::transaction::UnknownProvenance {
        reason: crate::transaction::UnknownReason::Predecessor,
        run: Identity([70, 71]),
        boot: Identity([72, 73]),
    });
    if let Body::Events(inner) = &mut metadata.body {
        inner.recovery.losses.unknown = true;
    }
    let (raw, _) = fixture.publish(&metadata, Namespace::Terminal, &[]);
    crate::encoder::encode_write(
        &mut event_frame(false),
        PrivateDirectory::open(&raw)
            .unwrap()
            .create_file("0000.parquet")
            .unwrap(),
    )
    .unwrap();
    let (frontier, _) = fixture.frontier();
    assert!(frontier.complete());
    assert!(!frontier.trailing_unknown());
    assert!(frontier.unknown_provenance().is_some());
}

#[test]
fn source_frontier_continues_deep_routes_without_changing_its_snapshot() {
    // Goal: a source tail remains resumable when combined routes exceed one pass.
    // Method: use two80-level alias routes and drive the retained frontier read explicitly.
    let (fixture, metadata) = loss_fixture();
    let key = CanonicalKey::source(Namespace::Ordinary, &fixture.run, 1).unwrap();
    let text = key.file_name();
    let parts: Vec<_> = text[..80].chars().map(|digit| digit.to_string()).collect();
    let components: Vec<_> = parts.iter().map(String::as_str).collect();
    let (raw, _) = fixture.publish(&metadata, Namespace::Ordinary, &components);
    let mut leaf = fixture.root.join("event-fragments");
    for part in &parts {
        leaf.push(part);
        private::create_private_dir(&leaf).unwrap();
    }
    fs::hard_link(raw.join("metadata"), leaf.join(key.file_name())).unwrap();
    let mut work = ReadWork::default();
    assert_eq!(
        fixture
            .store()
            .source_frontier(&fixture.run, &budget(), &mut work)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(work.node_attempts <= 4096);
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let memory = budget();
    let mut state = [0; 256];
    let mut query = SourceFrontierRead::begin(&root, fixture.run, &mut state, &memory).unwrap();
    let mut complete = false;
    for _ in 0..8 {
        let before = query.work();
        let progress = query.step().unwrap();
        assert!(query.work().node_attempts - before.node_attempts <= 4096);
        if progress == FrontierStep::Complete {
            complete = true;
            break;
        }
    }
    assert!(complete);
    eprintln!("deep-frontier-observation work={:?}", query.work());
    assert!(query.work().node_attempts > 4096);
    assert_eq!(query.result().unwrap().unwrap().event_end(), 2);
    assert_eq!(query.work().metadata_admissions, 2);
}
#[test]
fn ordinary_inner_fields_and_index_origin_cannot_disagree_with_the_prefix() {
    // Goal: a valid inner codec cannot override the registered full ownership interval.
    // Method: alter independently encoded inner fields and then narrow the physical event index.
    let fixture = Fixture::new();
    let (metadata, _) = ordinary(&fixture);
    let (raw, _) = fixture.publish(&metadata, Namespace::Ordinary, &[]);
    let path = raw.join("metadata");
    let original = fs::read(&path).unwrap();
    for offset in [24, 72, 80, 88, 96, 104, 112, 120, 128] {
        let mut changed = original.clone();
        changed[512 + offset] ^= 1;
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(&changed).unwrap();
        assert!(
            fixture
                .store()
                .source_frontier(&fixture.run, &budget(), &mut ReadWork::default())
                .is_err(),
            "inner offset {offset}"
        );
    }
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .write_all(&original)
        .unwrap();
    let index = raw.join("events/index");
    let mut changed = fs::read(&index).unwrap();
    changed[72] ^= 1;
    fs::OpenOptions::new()
        .write(true)
        .open(index)
        .unwrap()
        .write_all(&changed)
        .unwrap();
    assert!(
        fixture
            .store()
            .source_frontier(&fixture.run, &budget(), &mut ReadWork::default())
            .is_err()
    );
}

#[test]
fn a_loss_only_frontier_does_not_reserve_a_row_decoder_workspace() {
    // Goal: fixed loss metadata remains a small durable fact under a small shared budget.
    // Method: read an actual loss gate with1MiB available, then prove all charges release.
    let (fixture, metadata) = loss_fixture();
    fixture.publish(&metadata, Namespace::Ordinary, &[]);
    let memory = Arc::new(crate::resources::ByteBudget::new(1024 * 1024));
    assert_eq!(
        fixture
            .store()
            .source_frontier(&fixture.run, &memory, &mut ReadWork::default())
            .unwrap()
            .unwrap()
            .event_end(),
        2
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn node_locator_matches_the_structural_literal_and_rejects_unused_nibbles() {
    // Goal: the read tree and later structural writer share one exact logical locator.
    // Method: compare independently written64-byte bytes, including an odd nibble boundary.
    let mut expected = [0; 64];
    expected[0] = 1;
    expected[1] = 3;
    expected[4] = 0x01;
    expected[5] = 0x20;
    let key = CanonicalKey::from_bytes(Namespace::Ordinary, &[0x01; 40]).unwrap();
    let decoded = NodeLocator::decode(&expected).unwrap();
    assert_eq!(decoded.encode(), expected);
    let locator = NodeLocator::from_key(Namespace::Ordinary, key, 0).unwrap();
    assert_eq!(locator.encode()[4..], [0; 60]);
    for offset in [0, 1, 2, 3, 5, 63] {
        let mut bad = expected;
        bad[offset] ^= if offset == 5 { 0x01 } else { 0x80 };
        assert!(NodeLocator::decode(&bad).is_err(), "offset {offset}");
    }
    assert!(NodeLocator::decode(&expected[..63]).is_err());
}

#[test]
fn known_original_publication_end_must_follow_the_attempted_origin() {
    // Goal: fragment provenance cannot certify an impossible original publication interval.
    // Method: reject a closed typed record before its codec writes any output bytes.
    let fixture = Fixture::new();
    let mut metadata = event_metadata(&fixture, true);
    metadata.original_end = metadata.attempted_origin;
    let mut bytes = Vec::new();
    assert_eq!(
        metadata.write(&mut bytes).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(bytes.is_empty());
}

mod insertion;
