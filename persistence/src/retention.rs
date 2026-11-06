//! How long a recording keeps its readings, and the clock that measures their age.
//!
//! The clock counts elapsed time and reads no reading's wall clock, so a wall clock that steps or
//! freezes changes no reading's age. Within a boot it follows the continuous clock, which counts
//! sleep. Across a reboot only the wall clock can say how long the Mac was away, and a wall clock
//! that went back counts as no time.

use collector::recording::private::PrivateDirectory;
use collector::ring::Identity;
use std::io::{self, Read};
use std::path::Path;

const CLOCK_MAGIC: &[u8; 8] = b"FDRETCK1";
const CLOCK_NAME: &str = "retention-clock";
const CLOCK_SIZE: usize = 48;
const PERIOD_NAME: &str = "retention-period";
const PERIOD_DIGITS_MAX: usize = 20;
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

const _: () = assert!(
    Period::SECONDS_MAX
        .checked_mul(NANOSECONDS_PER_SECOND)
        .is_some()
);
const _: () = assert!(Period::SECONDS_MIN <= Period::DEFAULT.seconds);
const _: () = assert!(Period::DEFAULT.seconds <= Period::SECONDS_MAX);

/// The retention period, the only value a recording's policy holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Period {
    seconds: u64,
}

impl Period {
    pub const DEFAULT: Self = Self { seconds: 86_400 };
    const SECONDS_MIN: u64 = 60;
    const SECONDS_MAX: u64 = 10 * 366 * 86_400;

    /// A period of `seconds`, none outside the bounds, which keep a damaged value from emptying a
    /// recording.
    #[must_use]
    pub fn from_seconds(seconds: u64) -> Option<Self> {
        (Self::SECONDS_MIN..=Self::SECONDS_MAX)
            .contains(&seconds)
            .then_some(Self { seconds })
    }

    #[must_use]
    pub fn seconds(self) -> u64 {
        self.seconds
    }

    /// The root's period. A root with none keeps the default, and so does one whose file does not
    /// read, reported, since keeping readings too long can be put right and losing them cannot.
    #[must_use]
    pub fn read(root: &Path) -> Self {
        match Self::read_saved(root) {
            Ok(Some(period)) => period,
            Ok(None) => Self::DEFAULT,
            Err(error) => {
                collector::diagnostics::error(format_args!("retention period: {error}"));
                Self::DEFAULT
            }
        }
    }

    fn read_saved(root: &Path) -> io::Result<Option<Self>> {
        let mut text = [0; PERIOD_DIGITS_MAX + 1];
        let Some(length) = read_bounded(root, PERIOD_NAME, &mut text)? else {
            return Ok(None);
        };
        let seconds = std::str::from_utf8(&text[..length])
            .ok()
            .and_then(|digits| digits.parse().ok())
            .and_then(Self::from_seconds);

        seconds.map(Some).ok_or_else(invalid)
    }

    /// Saves the period where the persister and every reader read it on their next pass.
    pub fn write(self, root: &Path) -> io::Result<()> {
        collector::recording::private::atomic_write(
            root,
            PERIOD_NAME,
            self.seconds.to_string().as_bytes(),
        )
    }
}

/// What the Mac's clocks read at one moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockReading {
    pub boot: Identity,
    pub continuous_ns: u64,
    pub wall_ns: u64,
}

impl ClockReading {
    pub fn now() -> io::Result<Self> {
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| invalid())?;

        Ok(Self {
            boot: collector::recording::boot_identity()?,
            continuous_ns: collector::recording::control::now()?,
            wall_ns: u64::try_from(wall.as_nanos()).map_err(|_| invalid())?,
        })
    }
}

/// The retention clock: elapsed time that only moves forward, and the moment it last moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    progress_ns: u64,
    at: ClockReading,
}

impl Clock {
    /// The clock of a root with no saved record.
    #[must_use]
    pub fn started_at(now: &ClockReading) -> Self {
        Self {
            progress_ns: now.wall_ns,
            at: *now,
        }
    }

    /// The clock at `now`, an operating value that no saved record can make fall.
    #[must_use]
    pub fn advanced_to(&self, now: &ClockReading) -> Self {
        let elapsed_ns = if now.boot == self.at.boot {
            now.continuous_ns.saturating_sub(self.at.continuous_ns)
        } else {
            now.wall_ns.saturating_sub(self.at.wall_ns)
        };

        Self {
            progress_ns: self.progress_ns.saturating_add(elapsed_ns),
            at: *now,
        }
    }

    /// The clock at `now` from what the root saved. A root with no record starts one, and so does
    /// one whose record does not read, reported, since a reader or a save must go on without it.
    #[must_use]
    pub fn read(root: &Path, now: &ClockReading) -> Self {
        match Self::read_saved(root) {
            Ok(Some(saved)) => saved.advanced_to(now),
            Ok(None) => Self::started_at(now),
            Err(error) => {
                collector::diagnostics::error(format_args!("retention clock: {error}"));
                Self::started_at(now)
            }
        }
    }

    fn read_saved(root: &Path) -> io::Result<Option<Self>> {
        let mut bytes = [0; CLOCK_SIZE + 1];
        let Some(length) = read_bounded(root, CLOCK_NAME, &mut bytes)? else {
            return Ok(None);
        };
        if length != CLOCK_SIZE || bytes[..8] != CLOCK_MAGIC[..] {
            return Err(invalid());
        }

        Ok(Some(Self {
            progress_ns: read_word(&bytes, 8),
            at: ClockReading {
                boot: Identity([read_word(&bytes, 16), read_word(&bytes, 24)]),
                continuous_ns: read_word(&bytes, 32),
                wall_ns: read_word(&bytes, 40),
            },
        }))
    }

    /// Saves the clock. Losing this write loses nothing, since the next read advances from the
    /// older record to the same value.
    pub fn write(&self, root: &Path) -> io::Result<()> {
        let mut bytes = [0; CLOCK_SIZE];
        bytes[..8].copy_from_slice(CLOCK_MAGIC);
        for (offset, word) in [
            (8, self.progress_ns),
            (16, self.at.boot.0[0]),
            (24, self.at.boot.0[1]),
            (32, self.at.continuous_ns),
            (40, self.at.wall_ns),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&word.to_be_bytes());
        }

        collector::recording::private::atomic_write(root, CLOCK_NAME, &bytes)
    }

    #[must_use]
    pub fn progress_ns(&self) -> u64 {
        self.progress_ns
    }

    /// What a reader or the worker compares a reading's retained time with.
    #[must_use]
    pub fn cutoff(&self, period: Period) -> Cutoff {
        Cutoff {
            expired_below_ns: self
                .progress_ns
                .saturating_sub(period.seconds * NANOSECONDS_PER_SECOND),
        }
    }
}

/// The clock's value below which a reading has outlived the retention period.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cutoff {
    expired_below_ns: u64,
}

impl Cutoff {
    /// Whether a reading has expired, given the retained time of its publication's newest reading
    /// and how far behind that reading it was taken.
    #[must_use]
    pub fn expired(self, retained_from_ns: u64, behind_newest_ns: u64) -> bool {
        retained_from_ns.saturating_sub(behind_newest_ns) < self.expired_below_ns
    }
}

/// Reads a small file of the root into `bytes`, whose last byte is there to notice a longer file.
/// None when the file is absent.
fn read_bounded(root: &Path, name: &str, bytes: &mut [u8]) -> io::Result<Option<usize>> {
    let directory = PrivateDirectory::open(root)?;
    let mut file = match directory.open_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut length = 0;
    while length < bytes.len() {
        let read = file.read(&mut bytes[length..])?;
        if read == 0 {
            break;
        }
        length += read;
    }

    Ok(Some(length))
}

fn read_word(bytes: &[u8], offset: usize) -> u64 {
    let mut word = [0; 8];
    word.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_be_bytes(word)
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

#[cfg(test)]
mod tests;
