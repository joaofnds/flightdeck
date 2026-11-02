use std::fs;
use std::io;
use std::path::Path;

use collector::recording::private;

use crate::Manifest;
use crate::event_recovery::{batch_name, metadata::BATCHES_MAX};
use crate::metadata::invalid;

pub(super) fn validate(path: &Path, manifest: &Manifest, sync: bool) -> io::Result<()> {
    private::verify_directory(path)?;
    let index =
        crate::event_table::index::Index::read(&path.join("index"), manifest).map_err(invalid)?;
    let expected = index.batches.len() + 1;
    let mut entries = fs::read_dir(path)?;
    for count in 0..=expected {
        let Some(entry) = entries.next() else {
            return if count == expected {
                Ok(())
            } else {
                Err(invalid("missing event part"))
            };
        };
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| invalid("event part name"))?;
        if name != "index" {
            let part = part_number(name)?;
            if part >= index.batches.len() {
                return Err(invalid("unexpected event part"));
            }
        }
        private::verify_file(&entry.path())?;
        if sync {
            private::full_sync(&private::open_private_file(&entry.path(), false)?)?;
        }
    }
    Err(invalid("extra event part"))
}

pub(super) fn sync(path: &Path, manifest: &Manifest) -> io::Result<()> {
    validate(path, manifest, true)?;
    private::sync_directory(path)
}

pub(super) fn remove_pending(path: &Path) -> io::Result<()> {
    private::verify_directory(path)?;
    let mut entries = fs::read_dir(path)?;
    for _index in 0..=BATCHES_MAX + 1 {
        let Some(entry) = entries.next() else {
            fs::remove_dir(path)?;
            return Ok(());
        };
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| invalid("pending event name"))?;
        if name != "index" {
            part_number(name)?;
        }
        private::verify_file(&entry.path())?;
        fs::remove_file(entry.path())?;
    }
    Err(invalid("pending event parts"))
}

fn part_number(name: &str) -> io::Result<usize> {
    let number = name
        .strip_suffix(".parquet")
        .ok_or_else(|| invalid("event part suffix"))?
        .parse::<usize>()
        .map_err(invalid)?;
    if number >= BATCHES_MAX || name != batch_name(number) {
        return Err(invalid("event part number"));
    }
    Ok(number)
}
