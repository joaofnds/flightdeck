//! Immutable recording paths. Wall-clock bounds select candidates, never identity.
use std::io;
use std::path::{Path, PathBuf};

use collector::recording::discovery::{TimeBounds, WindowName};
use collector::recording::{identity_text, parse_identity};
use collector::ring::Identity;

use crate::WindowIdentity;

pub const WINDOWS_PER_DAY: u64 = 1440;

#[derive(Clone, Copy)]
pub enum Kind {
    Data,
    Commits,
}

pub struct DirectoryNames<'a> {
    pub run: &'a str,
    pub day: &'a str,
    pub window: &'a str,
}

pub struct WindowLocation {
    pub key: WindowKey,
    pub name: WindowName,
}

#[derive(Clone, Copy)]
pub struct WindowKey {
    pub run: Identity,
    pub window: u64,
}

impl From<&WindowIdentity> for WindowKey {
    fn from(identity: &WindowIdentity) -> Self {
        Self {
            run: identity.run.run,
            window: identity.window,
        }
    }
}

impl WindowKey {
    #[must_use]
    pub fn partition(&self, root: &Path, kind: Kind) -> PathBuf {
        run(root, kind, self.run).join(format!("{:020}", self.window / WINDOWS_PER_DAY))
    }

    #[must_use]
    pub fn marker(&self, root: &Path) -> PathBuf {
        self.partition(root, Kind::Commits)
            .join(format!("{:020}.seal", self.window))
    }

    #[must_use]
    pub fn ready_relative(&self, bounds: Option<TimeBounds>) -> PathBuf {
        self.partition(Path::new(""), Kind::Data).join(
            WindowName {
                window: self.window,
                bounds,
            }
            .to_string(),
        )
    }
}

impl WindowLocation {
    pub fn parse(names: &DirectoryNames<'_>) -> io::Result<Self> {
        validate_run(names.run)?;
        let day = validate_day(names.day)?;
        let name = WindowName::parse(names.window)?;
        if name.window / WINDOWS_PER_DAY != day {
            return Err(invalid());
        }
        Ok(Self {
            key: WindowKey {
                run: parse_identity(names.run)?,
                window: name.window,
            },
            name,
        })
    }

    #[must_use]
    pub fn ready_relative(&self) -> PathBuf {
        self.key.ready_relative(self.name.bounds)
    }
}

#[must_use]
pub fn run(root: &Path, kind: Kind, identity: Identity) -> PathBuf {
    base(root, kind).join(identity_text(identity).to_string())
}

#[must_use]
pub fn base(root: &Path, kind: Kind) -> PathBuf {
    let name = match kind {
        Kind::Data => "data",
        Kind::Commits => "commits",
    };
    root.join(name)
}

pub fn validate_run(name: &str) -> io::Result<()> {
    if name.len() != 32 {
        return Err(invalid());
    }
    if !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    if name.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(invalid());
    }
    parse_identity(name)?;
    Ok(())
}

pub fn validate_day(name: &str) -> io::Result<u64> {
    if name.len() != 20 {
        return Err(invalid());
    }
    if !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    name.parse::<u64>().map_err(|_| invalid())
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

/// Uses bounded caller-page storage and returns success only after actual directory EOF.
pub fn visit_names<E: From<io::Error>>(
    path: &Path,
    mut visit: impl FnMut(&str) -> Result<(), E>,
) -> Result<(), E> {
    use collector::recording::discovery::{
        DIRECTORY_BUFFER_WORDS, DirectoryCursor, DirectoryEntry,
    };
    collector::recording::private::verify_directory(path)?;
    let mut buffer: Box<[u64; DIRECTORY_BUFFER_WORDS]> = vec![0; DIRECTORY_BUFFER_WORDS]
        .into_boxed_slice()
        .try_into()
        .map_err(|_| E::from(invalid()))?;
    let mut cursor = DirectoryCursor::open(path, &mut buffer)?;
    for _entry in 0..u64::MAX {
        match cursor.next_name()? {
            DirectoryEntry::Complete => return Ok(()),
            DirectoryEntry::Name(name) => visit(name)?,
        }
    }
    Err(E::from(invalid()))
}
