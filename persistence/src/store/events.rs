use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use collector::recording::private;
use collector::ring::RunIdentity;

use super::{CommitBoundary, StagingPermit, Store};
use crate::event_recovery::{self as events, Manifest};
use crate::metadata::invalid;

pub(crate) enum EventBegin {
    Committed(Manifest),
    Pending(PendingEvents),
}

pub(crate) struct PendingEvents {
    pub path: PathBuf,
    run: RunIdentity,
    permit: StagingPermit,
}

impl Store {
    pub(crate) fn begin_events(&self, run: &RunIdentity) -> io::Result<EventBegin> {
        self.require_composed_catalog()?;
        self.validate_identity(&crate::WindowIdentity {
            run: *run,
            window: 0,
        })?;
        if self
            .staging
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let permit = StagingPermit(Arc::clone(&self.staging));
        if let Some(manifest) = self.existing_events(run)? {
            return Ok(EventBegin::Committed(manifest));
        }
        private::create_private_dir(&events::base(&self.root))?;
        private::sync_directory(&self.root)?;
        let directory = events::directory(&self.root, run);
        private::create_private_dir(&directory)?;
        private::sync_directory(&events::base(&self.root))?;
        let ready = directory.join("data");
        match private::verify_directory(&ready) {
            Ok(()) => {
                let manifest = read_manifest(&ready.join("metadata"), run)?;
                validate_files(&ready, &manifest, true)?;
                private::sync_directory(&directory)?;
                publish_marker(&directory, &manifest, &mut |_| Ok(()))?;
                return Ok(EventBegin::Committed(manifest));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let path = directory.join("pending");
        remove_pending(&path)?;
        private::create_private_dir(&path)?;
        Ok(EventBegin::Pending(PendingEvents {
            path,
            run: *run,
            permit,
        }))
    }

    pub(crate) fn existing_events(&self, run: &RunIdentity) -> io::Result<Option<Manifest>> {
        self.validate_identity(&crate::WindowIdentity {
            run: *run,
            window: 0,
        })?;
        let existing = read_committed(&self.root, run)?;
        if existing.is_some() {
            private::sync_directory(&events::directory(&self.root, run))?;
        }
        Ok(existing)
    }

    pub(crate) fn commit_events(
        &self,
        pending: PendingEvents,
        manifest: Manifest,
        observe: &mut impl FnMut(CommitBoundary) -> io::Result<()>,
    ) -> io::Result<Manifest> {
        self.validate_identity(&crate::WindowIdentity {
            run: pending.run,
            window: 0,
        })?;
        if !Arc::ptr_eq(&pending.permit.0, &self.staging) || pending.run != manifest.recovery.run {
            return Err(invalid("terminal pending owner"));
        }
        manifest.validate()?;
        if let Some(existing) = self.existing_events(&pending.run)? {
            if existing != manifest {
                return Err(invalid("terminal canonical conflict"));
            }
            return Ok(existing);
        }
        observe(CommitBoundary::BeforeFilesSync)?;
        for index in 0..manifest.batches.len() {
            let path = pending.path.join(events::batch_name(index));
            private::make_file_private(&path)?;
            private::full_sync(&private::open_private_file(&path, false)?)?;
        }
        validate_files(&pending.path, &manifest, false)?;
        observe(CommitBoundary::FilesSynced)?;
        observe(CommitBoundary::BeforeMetadataWrite)?;
        let mut file = private::open_private_file(&pending.path.join("metadata"), true)?;
        manifest.write(&mut file)?;
        private::full_sync(&file)?;
        drop(file);
        observe(CommitBoundary::MetadataSynced)?;
        observe(CommitBoundary::BeforePendingSync)?;
        private::sync_directory(&pending.path)?;
        observe(CommitBoundary::PendingSynced)?;
        let directory = events::directory(&self.root, &pending.run);
        let ready = directory.join("data");
        if ready.try_exists()? {
            return Err(invalid("terminal ready exists"));
        }
        observe(CommitBoundary::BeforeReadyRename)?;
        fs::rename(&pending.path, &ready)?;
        observe(CommitBoundary::ReadyRenamed)?;
        observe(CommitBoundary::BeforeReadySync)?;
        private::sync_directory(&directory)?;
        observe(CommitBoundary::ReadySynced)?;
        publish_marker(&directory, &manifest, observe)?;
        drop(pending);
        Ok(manifest)
    }
}

fn publish_marker(
    directory: &Path,
    manifest: &Manifest,
    observe: &mut impl FnMut(CommitBoundary) -> io::Result<()>,
) -> io::Result<()> {
    let source = directory.join("data/metadata");
    let target = directory.join("seal");
    observe(CommitBoundary::BeforeMarkerLink)?;
    match fs::hard_link(&source, &target) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = read_manifest(&target, &manifest.recovery.run)?;
            if existing != *manifest {
                return Err(invalid("terminal canonical conflict"));
            }
            validate_link(&source, &private::open_private_file(&target, false)?)?;
        }
        Err(error) => return Err(error),
    }
    observe(CommitBoundary::MarkerLinked)?;
    observe(CommitBoundary::BeforeMarkerSync)?;
    private::sync_directory(directory)?;
    observe(CommitBoundary::MarkerSynced)
}

pub(crate) fn read_committed(root: &Path, run: &RunIdentity) -> io::Result<Option<Manifest>> {
    if collector::recording::read_identity(root)? != run.recording {
        return Err(invalid("terminal recording identity"));
    }
    let directory = events::directory(root, run);
    for path in [events::base(root), directory.clone()] {
        match private::verify_directory(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    let mut marker = match private::open_private_file(&directory.join("seal"), false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let manifest = Manifest::read(&mut marker)?;
    if manifest.recovery.run != *run {
        return Err(invalid("terminal canonical identity"));
    }
    let ready = directory.join("data");
    validate_files(&ready, &manifest, true)?;
    validate_link(&ready.join("metadata"), &marker)?;
    Ok(Some(manifest))
}

fn read_manifest(path: &Path, run: &RunIdentity) -> io::Result<Manifest> {
    let manifest = Manifest::read(&mut private::open_private_file(path, false)?)?;
    if manifest.recovery.run != *run {
        return Err(invalid("terminal metadata identity"));
    }
    Ok(manifest)
}

fn validate_link(path: &Path, marker: &File) -> io::Result<()> {
    let source = fs::symlink_metadata(path)?;
    let target = marker.metadata()?;
    if source.dev() != target.dev() || source.ino() != target.ino() {
        return Err(invalid("terminal marker inode"));
    }
    Ok(())
}

fn validate_files(path: &Path, manifest: &Manifest, metadata: bool) -> io::Result<()> {
    private::verify_directory(path)?;
    let expected = manifest.batches.len() + usize::from(metadata);
    let mut entries = fs::read_dir(path)?;
    for count in 0..=expected {
        let Some(entry) = entries.next() else {
            return if count == expected {
                Ok(())
            } else {
                Err(invalid("terminal missing file"))
            };
        };
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| invalid("terminal file name"))?;
        if !(metadata && name == "metadata") {
            let digits = name
                .strip_suffix(".parquet")
                .ok_or_else(|| invalid("terminal file suffix"))?;
            let index = digits.parse::<usize>().map_err(invalid)?;
            if index >= manifest.batches.len() || name != events::batch_name(index) {
                return Err(invalid("terminal batch name"));
            }
        }
        private::verify_file(&entry.path())?;
    }
    Err(invalid("terminal extra file"))
}

fn remove_pending(path: &Path) -> io::Result<()> {
    match private::verify_directory(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let mut entries = fs::read_dir(path)?;
    for _index in 0..=events::metadata::BATCHES_MAX + 1 {
        let Some(entry) = entries.next() else {
            fs::remove_dir(path)?;
            return Ok(());
        };
        let path = entry?.path();
        private::verify_file(&path)?;
        fs::remove_file(path)?;
    }
    Err(invalid("terminal pending files"))
}
