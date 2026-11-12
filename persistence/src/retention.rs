//! How long a recording keeps its readings, and the clock that measures their age.
//!
//! The clock counts elapsed time and reads no reading's wall clock, so a wall clock that steps or
//! freezes changes no reading's age. Within a boot it follows the continuous clock, which counts
//! sleep. Across a reboot only the wall clock can say how long the Mac was away, and a wall clock
//! that went back counts as no time.
//!
//! A saved reading's place on that clock is its own continuous clock plus the offset its boot
//! recorded, so nothing about a window's time has to be saved with the window, and a window
//! rebuilt after a crash is the same window.
//!
//! All of it lies in the root's `retention` directory: the period, the clock, one record per boot,
//! and the retention order, which names every publication under the hour of its retained time.

use crate::metadata::TimeBounds;
use collector::recording::private::{self, PrivateDirectory};
use collector::ring::Identity;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const BOOT_MAGIC: &[u8; 8] = b"FDRETBT1";
const BOOT_PREFIX: &str = "boot-";
/// More boots than a recording of ten years could need, which the directory's few other names
/// leave room in.
const BOOT_RECORDS_MAX: usize = 1 << 16;
const BOOT_SIZE: usize = 24;
const CLOCK_MAGIC: &[u8; 8] = b"FDRETCK1";
const CLOCK_NAME: &str = "clock";
const CLOCK_SIZE: usize = 48;
const DIRECTORY_NAME: &str = "retention";
const NANOSECONDS_PER_HOUR: u64 = 3_600 * NANOSECONDS_PER_SECOND;
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
const ORDER_NAME: &str = "order";
const PERIOD_DIGITS_MAX: usize = 20;
const PERIOD_NAME: &str = "period";
/// How long physical removal waits past a reading's expiry, which a read under way has to finish
/// or be cancelled in.
const READER_GRACE_SECONDS: u64 = 60;
const RESERVE_NAME: &str = "reserve";
/// Room set aside for removal on a full disk. APFS volumes of 256 MiB and 2 GiB that refused a new
/// file took the file and the rename the state of a removal needs once 256 KiB came free, and
/// refused the rename at 64 KiB. A removal then frees more than it uses.
const RESERVE_BYTES: usize = 1 << 20;
const ORDER_HOURS_MAX: usize = 1 << 20;

const _: () = assert!(
    (Period::SECONDS_MAX + READER_GRACE_SECONDS)
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

    /// The root's period, the default where it holds none. A file that does not read is
    /// `InvalidData`, which says nothing of how long the user meant, so no caller may take a
    /// default for it. A line break at its end is no damage, since the file may be written by hand.
    pub fn read(root: &PrivateDirectory) -> io::Result<Self> {
        let mut text = [0; PERIOD_DIGITS_MAX + 2];
        let Some(length) = read_bounded(root, PERIOD_NAME, &mut text)? else {
            return Ok(Self::DEFAULT);
        };
        let seconds = std::str::from_utf8(text[..length].trim_ascii_end())
            .ok()
            .and_then(|digits| digits.parse().ok())
            .and_then(Self::from_seconds);

        seconds.ok_or_else(invalid)
    }

    /// Saves the period where the persister and every reader read it on their next pass.
    pub fn write(self, root: &PrivateDirectory) -> io::Result<()> {
        private::atomic_write_in(
            &root.open_directory(DIRECTORY_NAME)?,
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
    pub fn read(root: &PrivateDirectory, now: &ClockReading) -> Self {
        match Self::read_saved(root) {
            Ok(Some(saved)) => saved.advanced_to(now),
            Ok(None) => Self::started_at(now),
            Err(error) => {
                collector::diagnostics::error(format_args!("retention clock: {error}"));
                Self::started_at(now)
            }
        }
    }

    /// The clock a pass of physical removal runs on at `now`. A clock that starts again because
    /// its record is gone or does not read stands no lower than the newest entry of the order, so
    /// a lost record cannot make a saved publication look young.
    pub(crate) fn read_for_removal(
        root: &PrivateDirectory,
        now: &ClockReading,
    ) -> io::Result<Self> {
        match Self::read_saved(root) {
            Ok(Some(saved)) => return Ok(saved.advanced_to(now)),
            Ok(None) => {}
            Err(error) => {
                collector::diagnostics::error(format_args!("retention clock: {error}"));
            }
        }

        Ok(Self {
            progress_ns: now.wall_ns.max(newest_entry_ns(root)?),
            at: *now,
        })
    }

    fn read_saved(root: &PrivateDirectory) -> io::Result<Option<Self>> {
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
    pub fn write(&self, root: &PrivateDirectory) -> io::Result<()> {
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

        private::atomic_write_in(&root.open_directory(DIRECTORY_NAME)?, CLOCK_NAME, &bytes)
    }

    #[must_use]
    pub fn progress_ns(&self) -> u64 {
        self.progress_ns
    }

    /// What a reading of `boot` adds to its continuous clock to land on this clock. A boot this
    /// clock is not running in can only count its newest reading as taken now, so a window saved
    /// after an outage outlives the period by that outage at most.
    fn boot_offset_ns(&self, boot: Identity, newest_continuous_ns: u64) -> u64 {
        if boot == self.at.boot {
            self.progress_ns.saturating_sub(self.at.continuous_ns)
        } else {
            self.progress_ns.saturating_sub(newest_continuous_ns)
        }
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

    /// What physical removal compares a publication's retained time with, later than readers stop
    /// returning it by the grace a read under way has.
    #[must_use]
    pub(crate) fn removal_cutoff(&self, period: Period) -> Cutoff {
        Cutoff {
            expired_below_ns: self
                .progress_ns
                .saturating_sub((period.seconds + READER_GRACE_SECONDS) * NANOSECONDS_PER_SECOND),
        }
    }
}

/// The clock's value below which a reading has outlived the retention period.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cutoff {
    expired_below_ns: u64,
}

impl Cutoff {
    /// Whether a reading whose place on the retention clock is `retained_ns` has expired.
    #[must_use]
    pub fn expired(self, retained_ns: u64) -> bool {
        retained_ns < self.expired_below_ns
    }
}

/// Which readings a reader may return, as the recording's clock stood when this was read. It holds
/// the offset of each boot asked about, none for a boot with no record that reads, so a read of
/// many windows reads each record once.
#[derive(Debug)]
pub struct Retained {
    cutoff: Cutoff,
    offsets: Vec<(Identity, Option<u64>)>,
}

impl Retained {
    /// What `root` retains of readings of `boots`. A clock no persister has saved yet has not
    /// started, so nothing has expired. A record that does not read is reported, and the readings
    /// it would place stay shown, those of its boot where it is a boot's, since a reader must go on
    /// and keeping errs on the side of the user's readings.
    #[must_use]
    pub fn read(root: &Path, boots: impl IntoIterator<Item = Identity>) -> Self {
        let saved = match Saved::read(root, &boots_distinct(boots)) {
            Ok(Some(saved)) => saved,
            Ok(None) => return Self::everything(),
            Err(error) => {
                retained_report(&error);
                return Self::everything();
            }
        };

        now_reported().map_or_else(Self::everything, |now| saved.retained_at(&now))
    }

    /// Every reading, which a reader returns where no clock has started or a record does not read.
    fn everything() -> Self {
        Self {
            cutoff: Cutoff {
                expired_below_ns: 0,
            },
            offsets: Vec::new(),
        }
    }

    fn offset_ns(&self, boot: Identity) -> Option<u64> {
        self.offsets
            .iter()
            .find(|(known, _)| *known == boot)
            .and_then(|(_, offset_ns)| *offset_ns)
    }

    /// Whether a reading of `boot` taken at `continuous_ns` may be returned. A reading with no
    /// clock, or of a boot with no record, has no place on the clock, and stays until physical
    /// removal takes its window.
    #[must_use]
    pub fn holds(&self, boot: Identity, continuous_ns: Option<u64>) -> bool {
        let (Some(continuous_ns), Some(offset_ns)) = (continuous_ns, self.offset_ns(boot)) else {
            return true;
        };

        !self.cutoff.expired(continuous_ns.saturating_add(offset_ns))
    }

    /// Whether every reading of a window of `boot` with these clock bounds may be returned. A
    /// window with no bounds holds a reading with no clock and may hold others that have one, so
    /// only a boot with no place on the clock answers for it.
    #[must_use]
    pub fn holds_every(&self, boot: Identity, continuous_ns: Option<TimeBounds>) -> bool {
        let Some(bounds) = continuous_ns else {
            return self.offset_ns(boot).is_none();
        };

        self.holds(boot, Some(bounds.min))
    }

    /// Whether a window of `boot` with these clock bounds may hold a reading to return, so a window
    /// that holds none is never read and physical removal can take it from under a reader.
    #[must_use]
    pub fn holds_any(&self, boot: Identity, continuous_ns: Option<TimeBounds>) -> bool {
        continuous_ns.is_none_or(|bounds| self.holds(boot, Some(bounds.max)))
    }

    /// How far on the clock a reading of `boot` at `continuous_ns` lies past the cutoff, none where
    /// it has no place on the clock or has expired.
    #[must_use]
    pub fn retained_by_ns(&self, boot: Identity, continuous_ns: Option<u64>) -> Option<u64> {
        let place_ns = continuous_ns?.saturating_add(self.offset_ns(boot)?);

        place_ns.checked_sub(self.cutoff.expired_below_ns)
    }

    /// How far on the clock a reading of `boot` at `continuous_ns` lies below the cutoff, zero
    /// where it is retained.
    #[must_use]
    pub fn expired_by_ns(&self, boot: Identity, continuous_ns: u64) -> u64 {
        let Some(offset_ns) = self.offset_ns(boot) else {
            return 0;
        };

        self.cutoff
            .expired_below_ns
            .saturating_sub(continuous_ns.saturating_add(offset_ns))
    }
}

/// What a recording's retention records said of the readings of some boots when they were read.
struct Saved {
    clock: Clock,
    period: Period,
    offsets: Vec<(Identity, Option<u64>)>,
    /// Whether every boot's record read. One that did not places no reading, as a boot with no
    /// record does, and its reader reads it again at the next ask, since the failure may pass.
    whole: bool,
}

impl Saved {
    /// What the records of `root` say of readings of `boots`, each named once, none where no
    /// persister has saved a clock.
    fn read(root: &Path, boots: &[Identity]) -> io::Result<Option<Self>> {
        let directory = match PrivateDirectory::open(root) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let Some(clock) = Clock::read_saved(&directory)? else {
            return Ok(None);
        };
        let period = Period::read(&directory)?;
        let mut offsets = Vec::with_capacity(boots.len());
        let mut whole = true;
        for boot in boots {
            let offset_ns = match boot_offset_ns(&directory, *boot) {
                Ok(offset_ns) => offset_ns,
                Err(error) => {
                    retained_report(&error);
                    whole = false;
                    None
                }
            };
            offsets.push((*boot, offset_ns));
        }

        Ok(Some(Self {
            clock,
            period,
            offsets,
            whole,
        }))
    }

    /// What these records retain once the Mac's clocks read `now`.
    fn retained_at(&self, now: &ClockReading) -> Retained {
        Retained {
            cutoff: self.clock.advanced_to(now).cutoff(self.period),
            offsets: self.offsets.clone(),
        }
    }
}

/// A reader of a recording's retention records that keeps what it read between asks. A read opens
/// the recording's directory, its retention directory and every record, and the app's store asks
/// at every step, many times a second, while the persister saves the clock once a minute. So this
/// reader reads again only once a record or one of the two directories has changed, or an ask
/// names other boots.
pub struct Reader {
    root: PathBuf,
    kept: Option<Kept>,
    reads: u64,
}

/// What one read found, with what the two directories and each record's file showed just before
/// it. It keeps the boots of one ask, so the reader's own bound on those is its bound.
struct Kept {
    /// None where the read found no saved clock, or failed.
    saved: Option<Saved>,
    boots: Vec<Identity>,
    statuses: Vec<(PathBuf, Option<PathStatus>)>,
    /// Whether the read answers until a status changes. One that failed, in a status or in a
    /// record, is made again at the next ask, as a reader that keeps nothing makes it.
    stands: bool,
}

/// What a file or a directory shows without being opened. A save replaces a record's file, which
/// changes its inode. A write over a record changes its times, and so does a change to a
/// directory's mode, owner or access list. A write of the same length within one grain of the
/// volume's times is not told apart from none, and stands until the next change.
#[derive(Clone, Copy, PartialEq, Eq)]
struct PathStatus {
    device: u64,
    inode: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl PathStatus {
    /// The status of what is at `path`, none where nothing is.
    fn read(path: &Path) -> io::Result<Option<Self>> {
        use std::os::unix::fs::MetadataExt;

        match std::fs::symlink_metadata(path) {
            Ok(metadata) => Ok(Some(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                bytes: metadata.len(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                changed: (metadata.ctime(), metadata.ctime_nsec()),
            })),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl Kept {
    /// The two directories and the two records an ask reads whatever boots it names.
    const PATHS_ALWAYS: usize = 4;

    /// Reads the records of `root` for readings of `boots`. Every status is taken before the read,
    /// so a record that changes between the two is read again at the next ask.
    fn read(root: &Path, boots: Vec<Identity>) -> Self {
        let mut stands = true;
        let statuses = match Self::read_statuses(root, &boots) {
            Ok(statuses) => statuses,
            Err(error) => {
                retained_report(&error);
                stands = false;
                Vec::new()
            }
        };
        let saved = match Saved::read(root, &boots) {
            Ok(saved) => saved,
            Err(error) => {
                retained_report(&error);
                stands = false;
                None
            }
        };
        if saved.as_ref().is_some_and(|saved| !saved.whole) {
            stands = false;
        }
        if stands {
            assert_eq!(statuses.len(), Self::PATHS_ALWAYS + boots.len());
        }

        Self {
            saved,
            boots,
            statuses,
            stands,
        }
    }

    /// What the recording's directory, its retention directory, the clock, the period and the
    /// record of each of `boots` show.
    fn read_statuses(
        root: &Path,
        boots: &[Identity],
    ) -> io::Result<Vec<(PathBuf, Option<PathStatus>)>> {
        let directory = root.join(DIRECTORY_NAME);
        let mut paths = vec![
            root.to_path_buf(),
            directory.clone(),
            directory.join(CLOCK_NAME),
            directory.join(PERIOD_NAME),
        ];
        paths.extend(
            boots
                .iter()
                .map(|boot| directory.join(boot_record_name(*boot))),
        );
        let mut statuses = Vec::with_capacity(paths.len());
        for path in paths {
            let status = PathStatus::read(&path)?;
            statuses.push((path, status));
        }

        Ok(statuses)
    }

    /// Whether a read for readings of `boots` would find what this one found: this one stands, it
    /// was made for those boots, and neither a directory nor a record has changed since.
    fn answers(&self, boots: &[Identity]) -> bool {
        if !self.stands || self.boots != boots {
            return false;
        }

        self.statuses
            .iter()
            .all(|(path, kept)| PathStatus::read(path).is_ok_and(|status| status == *kept))
    }

    /// What the read retains once the Mac's clocks read `now`, everything where it found no clock.
    fn retained_at(&self, now: &ClockReading) -> Retained {
        self.saved
            .as_ref()
            .map_or_else(Retained::everything, |saved| saved.retained_at(now))
    }
}

impl Reader {
    /// A reader of the recording at `root`, which reads the records at its first ask.
    #[must_use]
    pub fn of(root: PathBuf) -> Self {
        Self {
            root,
            kept: None,
            reads: 0,
        }
    }

    /// What the recording retains now of readings of `boots`, which is what `Retained::read`
    /// answers.
    #[must_use]
    pub fn retained(&mut self, boots: impl IntoIterator<Item = Identity>) -> Retained {
        match now_reported() {
            Some(now) => self.retained_at(boots, &now),
            None => Retained::everything(),
        }
    }

    /// How many times this reader has read the records, which is what its asks cost.
    #[must_use]
    pub fn reads(&self) -> u64 {
        self.reads
    }

    fn retained_at(
        &mut self,
        boots: impl IntoIterator<Item = Identity>,
        now: &ClockReading,
    ) -> Retained {
        let boots = boots_distinct(boots);
        let kept = match self.kept.take() {
            Some(kept) if kept.answers(&boots) => kept,
            _ => {
                self.reads += 1;
                Kept::read(&self.root, boots)
            }
        };
        let retained = kept.retained_at(now);
        self.kept = Some(kept);

        retained
    }
}

/// Each of `boots` once, in the order it first comes.
fn boots_distinct(boots: impl IntoIterator<Item = Identity>) -> Vec<Identity> {
    let mut distinct = Vec::new();
    for boot in boots {
        if !distinct.contains(&boot) {
            distinct.push(boot);
        }
    }

    distinct
}

/// What the Mac's clocks read now, none where they do not read, which is reported.
fn now_reported() -> Option<ClockReading> {
    match ClockReading::now() {
        Ok(now) => Some(now),
        Err(error) => {
            retained_report(&error);
            None
        }
    }
}

/// Says once a process what keeps a reader from the clock, since a reader asks many times a second.
fn retained_report(error: &io::Error) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if !REPORTED.swap(true, Ordering::Relaxed) {
        collector::diagnostics::error(format_args!("retention: {error}"));
    }
}

/// The retained time of a publication saved at `now`: the place on the retention clock of its
/// newest reading, from the clocks a window's readings carry, or the clock's value for a
/// publication with none, whose readings have no other time.
pub(crate) fn retained_from_ns(
    root: &PrivateDirectory,
    now: &ClockReading,
    boot: Identity,
    continuous_ns: Option<TimeBounds>,
) -> io::Result<u64> {
    let clock = Clock::read(root, now);
    let Some(bounds) = continuous_ns else {
        return Ok(clock.progress_ns());
    };

    match retained_from_ns_boot_offset(root, &clock, boot, bounds.max) {
        Ok(offset_ns) => Ok(bounds.max.saturating_add(offset_ns)),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            collector::diagnostics::error(format_args!("retention boot record: {error}"));
            Ok(clock.progress_ns())
        }
        Err(error) => Err(error),
    }
}

/// The boot's saved offset, saving one the first time a publication of that boot needs it. Once
/// saved it never changes, so one reading keeps one place on the clock.
fn retained_from_ns_boot_offset(
    root: &PrivateDirectory,
    clock: &Clock,
    boot: Identity,
    newest_continuous_ns: u64,
) -> io::Result<u64> {
    if let Some(offset_ns) = boot_offset_ns(root, boot)? {
        return Ok(offset_ns);
    }

    let offset_ns = clock.boot_offset_ns(boot, newest_continuous_ns);
    boot_offset_save(root, boot, offset_ns)?;

    Ok(offset_ns)
}

/// What `boot` saved as its offset, none where no publication of that boot has needed one.
pub(crate) fn boot_offset_ns(root: &PrivateDirectory, boot: Identity) -> io::Result<Option<u64>> {
    let record = BootRecord::read(root, &boot_record_name(boot))?;

    Ok(record.map(|record| record.offset_ns))
}

/// Saves the offset of a boot that has none. No caller replaces one, so a reading keeps one place.
pub fn boot_offset_save(root: &PrivateDirectory, boot: Identity, offset_ns: u64) -> io::Result<()> {
    let record = BootRecord {
        offset_ns,
        ended_ns: 0,
    };

    record.write(root, &boot_record_name(boot))
}

fn boot_record_name(boot: Identity) -> String {
    format!("{BOOT_PREFIX}{}", collector::recording::identity_text(boot))
}

/// What a boot's record holds.
struct BootRecord {
    offset_ns: u64,
    /// The clock when a pass first saw another boot running, which no reading of this boot can lie
    /// past, or zero until then.
    ended_ns: u64,
}

impl BootRecord {
    fn read(root: &PrivateDirectory, name: &str) -> io::Result<Option<Self>> {
        let mut bytes = [0; BOOT_SIZE + 1];
        let Some(length) = read_bounded(root, name, &mut bytes)? else {
            return Ok(None);
        };
        if length != BOOT_SIZE || bytes[..8] != BOOT_MAGIC[..] {
            return Err(invalid());
        }

        Ok(Some(Self {
            offset_ns: read_word(&bytes, 8),
            ended_ns: read_word(&bytes, 16),
        }))
    }

    fn write(&self, root: &PrivateDirectory, name: &str) -> io::Result<()> {
        let mut bytes = [0; BOOT_SIZE];
        bytes[..8].copy_from_slice(BOOT_MAGIC);
        bytes[8..16].copy_from_slice(&self.offset_ns.to_be_bytes());
        bytes[16..].copy_from_slice(&self.ended_ns.to_be_bytes());

        private::atomic_write_in(&root.open_directory(DIRECTORY_NAME)?, name, &bytes)
    }
}

/// Removes the record of each boot that has ended and that no publication can still need, since a
/// reader shows a reading whose boot has no record. A record that does not read places no reading,
/// so it goes as well, and the running boot's next publication saves that boot's again at the same
/// offset, which the running clock fixes.
pub(crate) fn boot_records_settle(root: &PrivateDirectory, clock: &Clock) -> io::Result<()> {
    let directory = root.open_directory(DIRECTORY_NAME)?;
    let mut names = Vec::new();
    directory.visit_names(BOOT_RECORDS_MAX, |name| {
        if name.starts_with(BOOT_PREFIX) {
            names.push(name.to_owned());
        }
        Ok(())
    })?;

    for name in &names {
        boot_records_settle_one(root, &directory, name, clock)?;
    }

    Ok(())
}

fn boot_records_settle_one(
    root: &PrivateDirectory,
    directory: &PrivateDirectory,
    name: &str,
    clock: &Clock,
) -> io::Result<()> {
    let record = match BootRecord::read(root, name) {
        Ok(record) => record,
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            collector::diagnostics::error(format_args!("retention boot record {name}: {error}"));
            return directory.remove_file(name);
        }
        Err(error) => return Err(error),
    };
    let Some(record) = record else {
        return Ok(());
    };
    if name == boot_record_name(clock.at.boot) {
        return Ok(());
    }
    if record.ended_ns == 0 {
        let ended = BootRecord {
            ended_ns: clock.progress_ns,
            ..record
        };
        return ended.write(root, name);
    }

    let at_or_below_end = Cutoff {
        expired_below_ns: record.ended_ns.saturating_add(1),
    };
    if expired_entries(root, at_or_below_end, 1)?.is_empty() {
        directory.remove_file(name)?;
    }

    Ok(())
}

/// Sets room aside for a full disk to give up, unless it already is. A refused write can leave part
/// of it in a file the next write replaces.
pub(crate) fn reserve_keep(root: &PrivateDirectory) -> io::Result<()> {
    let directory = root.open_directory(DIRECTORY_NAME)?;
    match directory.open_file(RESERVE_NAME) {
        Ok(_reserve) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            private::atomic_write_in(&directory, RESERVE_NAME, &vec![0; RESERVE_BYTES])
        }
        Err(error) => Err(error),
    }
}

/// Gives up the room `reserve_keep` set aside, if there is any.
pub(crate) fn reserve_release(root: &PrivateDirectory) -> io::Result<()> {
    let directory = root.open_directory(DIRECTORY_NAME)?;
    match directory.remove_file(RESERVE_NAME) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        Ok(()) | Err(_) => Ok(()),
    }
}

/// A publication's name in the retention order, which physical removal walks oldest first. Its
/// retained time lies in the name, since a resumed publication with no clocks may link a second
/// entry with a later time, and removal treats each as naming the same proof.
pub(crate) struct OrderEntry {
    pub(crate) retained_from_ns: u64,
    pub(crate) transaction: Identity,
    pub(crate) child: &'static str,
}

impl OrderEntry {
    /// Links the proof of `proof_directory` into the order and makes the name durable. A second
    /// call with the same values changes nothing.
    pub(crate) fn link(
        &self,
        root: &PrivateDirectory,
        proof_directory: &PrivateDirectory,
        syncs: &mut u64,
    ) -> io::Result<()> {
        let order = root
            .open_directory(DIRECTORY_NAME)?
            .create_directory_counted(ORDER_NAME, syncs)?;
        // Floors, so a directory holds the retained times inside its hour.
        let hour_index = self.retained_from_ns / NANOSECONDS_PER_HOUR;
        let hour = order.create_directory_counted(&hour_index.to_string(), syncs)?;
        let name = format!(
            "{}-{}-{}",
            self.retained_from_ns,
            collector::recording::identity_text(self.transaction),
            self.child
        );
        proof_directory.link_to(crate::store::PROOF_NAME, &hour, &name)?;
        *syncs += 1;

        hour.full_sync()
    }
}

/// An entry of the retention order as a pass of physical removal lists it.
#[derive(Debug)]
pub(crate) struct ListedEntry {
    hour: String,
    name: String,
    retained_from_ns: u64,
}

impl ListedEntry {
    /// None for a name no link made, which a pass reports and passes over.
    fn parse(hour: &str, name: &str) -> Option<Self> {
        let (retained, _rest) = name.split_once('-')?;

        Some(Self {
            hour: hour.to_owned(),
            name: name.to_owned(),
            retained_from_ns: retained.parse().ok()?,
        })
    }

    /// The transaction and child the name gives, for removing what a proof that does not read
    /// cannot name.
    pub(crate) fn raw_names(&self) -> Option<(&str, &str)> {
        let (_retained, rest) = self.name.split_once('-')?;
        rest.split_once('-')
    }

    pub(crate) fn open_proof(&self, root: &PrivateDirectory) -> io::Result<std::fs::File> {
        order_directory(root)?
            .open_directory(&self.hour)?
            .open_file(&self.name)
    }

    /// Removes the entry, the last step of a removal, since the entry keeps the proof a repeat
    /// after a crash starts from. An hour left empty goes with it.
    pub(crate) fn remove(&self, root: &PrivateDirectory) -> io::Result<()> {
        let order = order_directory(root)?;
        let hour = order.open_directory(&self.hour)?;
        match hour.remove_file(&self.name) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        hour.full_sync()?;
        drop(hour);

        order_remove_hour(&order, &self.hour)
    }
}

fn order_directory(root: &PrivateDirectory) -> io::Result<PrivateDirectory> {
    root.open_directory(DIRECTORY_NAME)?
        .open_directory(ORDER_NAME)
}

fn order_remove_hour(order: &PrivateDirectory, hour: &str) -> io::Result<()> {
    match order.remove_directory(hour) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    }

    order.full_sync()
}

/// The newest retained time the order holds, zero where it holds none.
fn newest_entry_ns(root: &PrivateDirectory) -> io::Result<u64> {
    let order = match order_directory(root) {
        Ok(order) => order,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut newest_hour = None;
    order.visit_names(ORDER_HOURS_MAX, |name| {
        if let Ok(hour) = name.parse::<u64>() {
            newest_hour = newest_hour.max(Some(hour));
        }
        Ok(())
    })?;
    let Some(hour) = newest_hour else {
        return Ok(0);
    };

    let hour_name = hour.to_string();
    let mut newest_ns = 0;
    order
        .open_directory(&hour_name)?
        .visit_names(ORDER_HOURS_MAX, |name| {
            if let Some(entry) = ListedEntry::parse(&hour_name, name) {
                newest_ns = newest_ns.max(entry.retained_from_ns);
            }
            Ok(())
        })?;

    Ok(newest_ns)
}

/// The entries whose retained time lies below `cutoff`, oldest hours first and oldest first, at
/// most `entries_max` of them, so a pass costs its bound however much has expired.
pub(crate) fn expired_entries(
    root: &PrivateDirectory,
    cutoff: Cutoff,
    entries_max: usize,
) -> io::Result<Vec<ListedEntry>> {
    let order = match order_directory(root) {
        Ok(order) => order,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut hours = Vec::new();
    order.visit_names(ORDER_HOURS_MAX, |name| {
        match name.parse::<u64>() {
            Ok(hour) => hours.push(hour),
            Err(_) => {
                collector::diagnostics::error(format_args!("retention order: unknown hour {name}"));
            }
        }
        Ok(())
    })?;
    hours.sort_unstable();

    let mut entries = Vec::with_capacity(entries_max.min(256));
    for hour in hours {
        let hour_start_ns = hour.saturating_mul(NANOSECONDS_PER_HOUR);
        if hour_start_ns >= cutoff.expired_below_ns || entries.len() == entries_max {
            break;
        }
        match expired_entries_of_hour(&order, hour, cutoff, entries_max, &mut entries) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotADirectory => {
                collector::diagnostics::error(format_args!("retention order: unknown hour {hour}"));
            }
            Err(error) => return Err(error),
        }
    }
    entries.sort_unstable_by_key(|entry| entry.retained_from_ns);

    Ok(entries)
}

fn expired_entries_of_hour(
    order: &PrivateDirectory,
    hour: u64,
    cutoff: Cutoff,
    entries_max: usize,
    entries: &mut Vec<ListedEntry>,
) -> io::Result<()> {
    let hour_name = hour.to_string();
    let directory = order.open_directory(&hour_name)?;
    let mut names = 0_usize;
    // A full list stops the walk by an error of its own, since a dense hour must not be a refusal.
    let walked = directory.visit_names(ORDER_HOURS_MAX, |name| {
        names += 1;
        match ListedEntry::parse(&hour_name, name) {
            Some(entry) if entry.retained_from_ns < cutoff.expired_below_ns => entries.push(entry),
            Some(_) => {}
            None => {
                collector::diagnostics::error(format_args!(
                    "retention order: unknown entry {name}"
                ));
            }
        }
        if entries.len() == entries_max {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        Ok(())
    });
    match walked {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
        Err(error) => return Err(error),
    }
    if names == 0 {
        drop(directory);
        order_remove_hour(order, &hour_name)?;
    }

    Ok(())
}

/// Reads a small file of the root's retention directory into `bytes`, whose last byte is there to
/// notice a longer file. None when the file is absent.
fn read_bounded(
    root: &PrivateDirectory,
    name: &str,
    bytes: &mut [u8],
) -> io::Result<Option<usize>> {
    let opened = root
        .open_directory(DIRECTORY_NAME)
        .and_then(|directory| directory.open_file(name));
    let mut file = match opened {
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

/// The names the retention order holds, whatever hour each lies under.
#[cfg(test)]
pub(crate) fn order_entry_names(root: &Path) -> Vec<String> {
    let order = root.join(DIRECTORY_NAME).join(ORDER_NAME);
    let mut names = Vec::new();
    if !order.exists() {
        return names;
    }

    for hour in std::fs::read_dir(order).unwrap() {
        for entry in std::fs::read_dir(hour.unwrap().path()).unwrap() {
            names.push(entry.unwrap().file_name().into_string().unwrap());
        }
    }
    names
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
