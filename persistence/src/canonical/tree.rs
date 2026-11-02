use crate::resources::{ByteBudget, ByteReservation};
use collector::recording::bootstrap::StateObservation;
use collector::recording::private::{DirectoryWork, PrivateDirectory};
use collector::ring::{Identity, RunIdentity};
use std::io;
use std::sync::Arc;

const DIRECT_ENTRIES: u64 = 16 * 124 + 128;
const BOUNDARY_ENTRIES: u64 = 32 * 124 + 256;
const _: () = {
    assert!(DIRECT_ENTRIES == 2112);
    assert!(BOUNDARY_ENTRIES == 4224);
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Namespace {
    Ordinary,
    Known,
    Unknown,
    Terminal,
    Fragments,
}
impl Namespace {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Ordinary => "ordinary",
            Self::Known => "observations-known",
            Self::Unknown => "observations-unknown",
            Self::Terminal => "terminal",
            Self::Fragments => "event-fragments",
        }
    }
    pub(crate) fn width(self) -> usize {
        match self {
            Self::Ordinary | Self::Fragments => 40,
            Self::Known => 60,
            Self::Unknown => 48,
            Self::Terminal => 32,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalKey {
    pub(crate) bytes: [u8; 60],
    pub(crate) length: u8,
}
impl CanonicalKey {
    pub fn from_bytes(namespace: Namespace, bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() != namespace.width() {
            return Err(invalid());
        }
        let mut key = Self {
            bytes: [0; 60],
            length: u8::try_from(bytes.len()).map_err(|_| invalid())?,
        };
        key.bytes[..bytes.len()].copy_from_slice(bytes);
        Ok(key)
    }
    pub fn source(namespace: Namespace, run: &RunIdentity, origin: u64) -> io::Result<Self> {
        if !matches!(
            namespace,
            Namespace::Ordinary | Namespace::Terminal | Namespace::Fragments
        ) {
            return Err(invalid());
        }
        let mut bytes = [0; 40];
        for (offset, identity) in [(0, run.run), (16, run.boot)] {
            bytes[offset..offset + 8].copy_from_slice(&identity.0[0].to_be_bytes());
            bytes[offset + 8..offset + 16].copy_from_slice(&identity.0[1].to_be_bytes());
        }
        bytes[32..40].copy_from_slice(&origin.to_be_bytes());
        Self::from_bytes(namespace, &bytes[..namespace.width()])
    }
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}.seal", hex(&self.bytes, 0, usize::from(self.length) * 2))
    }
}
/// The exact64-byte logical node locator used by structural transaction descriptors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeLocator {
    namespace: Namespace,
    prefix: [u8; 60],
    nibbles: u8,
}
impl NodeLocator {
    pub fn from_key(namespace: Namespace, key: CanonicalKey, nibbles: u8) -> io::Result<Self> {
        if usize::from(key.length) != namespace.width()
            || usize::from(nibbles) > namespace.width() * 2
        {
            return Err(invalid());
        }
        let mut prefix = key.bytes;
        for index in usize::from(nibbles)..120 {
            if index.is_multiple_of(2) {
                prefix[index / 2] &= 0x0f;
            } else {
                prefix[index / 2] &= 0xf0;
            }
        }
        Ok(Self {
            namespace,
            prefix,
            nibbles,
        })
    }
    #[must_use]
    pub fn encode(&self) -> [u8; 64] {
        let mut bytes = [0; 64];
        bytes[0] = match self.namespace {
            Namespace::Ordinary => 1,
            Namespace::Known => 2,
            Namespace::Unknown => 3,
            Namespace::Terminal => 4,
            Namespace::Fragments => 5,
        };
        bytes[1] = self.nibbles;
        bytes[4..].copy_from_slice(&self.prefix);
        bytes
    }
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() != 64 || bytes[2..4] != [0, 0] {
            return Err(invalid());
        }
        let namespace = match bytes[0] {
            1 => Namespace::Ordinary,
            2 => Namespace::Known,
            3 => Namespace::Unknown,
            4 => Namespace::Terminal,
            5 => Namespace::Fragments,
            _ => return Err(invalid()),
        };
        let key = CanonicalKey::from_bytes(namespace, &bytes[4..4 + namespace.width()])?;
        let locator = Self::from_key(namespace, key, bytes[1])?;
        if locator.encode() != bytes {
            return Err(invalid());
        }
        Ok(locator)
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Direction {
    Forward,
    Reverse,
}
#[derive(Clone, Copy, Debug)]
pub struct SeekRange {
    pub first: CanonicalKey,
    pub last: CanonicalKey,
    pub direction: Direction,
}
impl SeekRange {
    pub fn source_tail(namespace: Namespace, run: &RunIdentity) -> io::Result<Self> {
        Ok(Self {
            first: CanonicalKey::source(namespace, run, 0)?,
            last: CanonicalKey::source(namespace, run, u64::MAX)?,
            direction: Direction::Reverse,
        })
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadWork {
    pub namespace_attempts: u64,
    pub directory_reads: u64,
    pub entries: u64,
    pub native_entries: u64,
    pub metadata_admissions: u64,
    pub raw_admissions: u64,
    pub node_attempts: u64,
}
#[derive(Clone, Copy)]
struct Prefix {
    bytes: [u8; 60],
    nibbles: u8,
}
impl Prefix {
    const EMPTY: Self = Self {
        bytes: [0; 60],
        nibbles: 0,
    };
}
#[derive(Clone, Copy)]
struct Frame {
    prefix: Prefix,
    children: [Prefix; 16],
    count: u8,
    next: u8,
    loaded: bool,
}
impl Frame {
    const EMPTY: Self = Self {
        prefix: Prefix::EMPTY,
        children: [Prefix::EMPTY; 16],
        count: 0,
        next: 0,
        loaded: false,
    };
}
#[derive(Debug)]
pub enum ReadStep {
    Found(CanonicalKey),
    Continue,
    End,
}

pub struct CanonicalCursor<'root, 'state> {
    root: &'root PrivateDirectory,
    state: StateObservation<'root, 'state>,
    namespace: Namespace,
    range: SeekRange,
    stack: Box<[Frame; 124]>,
    depth: usize,
    leaf: [CanonicalKey; 128],
    leaf_count: usize,
    leaf_next: usize,
    ended: bool,
    work: ReadWork,
    query_entries: u64,
    _reservation: ByteReservation,
}
const _: () =
    assert!(size_of::<[Frame; 124]>() + size_of::<[CanonicalKey; 128]>() + 4096 < 256 * 1024);
impl<'root, 'state> CanonicalCursor<'root, 'state> {
    pub fn begin(
        root: &'root PrivateDirectory,
        recording: Identity,
        namespace: Namespace,
        range: SeekRange,
        state_bytes: &'state mut [u8; 256],
        budget: &Arc<ByteBudget>,
    ) -> io::Result<Self> {
        Self::begin_counted(
            root,
            recording,
            namespace,
            range,
            state_bytes,
            budget,
            &mut ReadWork::default(),
        )
    }
    pub(crate) fn begin_counted(
        root: &'root PrivateDirectory,
        recording: Identity,
        namespace: Namespace,
        range: SeekRange,
        state_bytes: &'state mut [u8; 256],
        budget: &Arc<ByteBudget>,
        work_out: &mut ReadWork,
    ) -> io::Result<Self> {
        if usize::from(range.first.length) != namespace.width()
            || range.first.length != range.last.length
            || range.first > range.last
        {
            return Err(invalid());
        }
        let reservation = budget
            .reserve(256 * 1024)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let mut work = ReadWork::default();
        let observed = StateObservation::begin_counted(
            root,
            recording,
            state_bytes,
            &mut work.namespace_attempts,
        );
        *work_out = work;
        let state = observed?.ok_or_else(retry)?;
        Ok(Self {
            root,
            state,
            namespace,
            range,
            stack: vec![Frame::EMPTY; 124]
                .into_boxed_slice()
                .try_into()
                .map_err(|_| invalid())?,
            depth: 0,
            leaf: [CanonicalKey {
                bytes: [0; 60],
                length: 0,
            }; 128],
            leaf_count: 0,
            leaf_next: 0,
            ended: false,
            work,
            query_entries: 0,
            _reservation: reservation,
        })
    }
    pub(crate) fn reset(&mut self, namespace: Namespace, range: SeekRange) -> io::Result<()> {
        self.check_snapshot()?;
        self.namespace = namespace;
        self.range = range;
        self.stack.fill(Frame::EMPTY);
        self.depth = 0;
        self.leaf_count = 0;
        self.leaf_next = 0;
        self.ended = false;
        self.query_entries = self.work.entries;
        Ok(())
    }
    pub(crate) fn add_work(&mut self, work: ReadWork) {
        self.work.namespace_attempts += work.namespace_attempts;
        self.work.raw_admissions += work.raw_admissions;
        self.work.metadata_admissions += work.metadata_admissions;
        self.work.entries += work.entries;
        self.work.native_entries += work.native_entries;
        self.work.directory_reads += work.directory_reads;
        self.work.node_attempts += work.node_attempts;
    }
    #[must_use]
    pub fn current_locator(&self) -> NodeLocator {
        let prefix = self.stack[self.depth].prefix;
        NodeLocator {
            namespace: self.namespace,
            prefix: prefix.bytes,
            nibbles: prefix.nibbles,
        }
    }
    #[must_use]
    pub fn work(&self) -> ReadWork {
        self.work
    }
    pub fn check_snapshot(&mut self) -> io::Result<()> {
        if self
            .state
            .unchanged_counted(&mut [0; 256], &mut self.work.namespace_attempts)?
        {
            Ok(())
        } else {
            Err(retry())
        }
    }
    /// Each call is one pass; continuation retains this exact snapshot and traversal stack.
    pub fn step(&mut self) -> io::Result<ReadStep> {
        self.next_limited(4096)
    }
    pub(crate) fn next_limited(&mut self, limit: u64) -> io::Result<ReadStep> {
        self.check_snapshot()?;
        let result = self.next_inner(limit);
        // This check also dominates apparent corruption and absence from the walk.
        self.check_snapshot()?;
        if result.is_ok()
            && self.range.first == self.range.last
            && self.work.entries - self.query_entries > DIRECT_ENTRIES
        {
            return Err(invalid());
        }
        result
    }
    fn next_inner(&mut self, limit: u64) -> io::Result<ReadStep> {
        let initial = self.work.node_attempts;
        let entries = self.work.entries;
        for _ in 0..4096 {
            if self.leaf_next < self.leaf_count {
                let index = match self.range.direction {
                    Direction::Forward => self.leaf_next,
                    Direction::Reverse => self.leaf_count - 1 - self.leaf_next,
                };
                self.leaf_next += 1;
                let key = self.leaf[index];
                if key >= self.range.first && key <= self.range.last {
                    return Ok(ReadStep::Found(key));
                }
                continue;
            }
            if self.ended {
                return Ok(ReadStep::End);
            }
            if !self.stack[self.depth].loaded {
                let required = collector::u64_from_usize(self.depth + 2 + 132);
                if self.work.node_attempts - initial + required > limit
                    || self.work.entries - entries + 128 > BOUNDARY_ENTRIES
                {
                    return Ok(ReadStep::Continue);
                }
                self.load_node()?;
                continue;
            }
            let frame = &mut self.stack[self.depth];
            if frame.next < frame.count {
                let index = match self.range.direction {
                    Direction::Forward => usize::from(frame.next),
                    Direction::Reverse => usize::from(frame.count - 1 - frame.next),
                };
                frame.next += 1;
                let child = frame.children[index];
                if intersects(&child, &self.range) {
                    if self.depth + 1 >= 124 {
                        return Err(invalid());
                    }
                    self.depth += 1;
                    self.stack[self.depth] = Frame {
                        prefix: child,
                        ..Frame::EMPTY
                    };
                }
            } else if self.depth == 0 {
                self.ended = true;
            } else {
                self.depth -= 1;
            }
        }
        Ok(ReadStep::Continue)
    }
    fn load_node(&mut self) -> io::Result<()> {
        let directory = self.open_current()?;
        let locator = self.current_locator();
        let prefix = Prefix {
            bytes: locator.prefix,
            nibbles: locator.nibbles,
        };
        let mut children = [Prefix::EMPTY; 16];
        let mut child_count = 0;
        let mut leaf_count = 0;
        let mut native = DirectoryWork::default();
        let result = directory.visit_names_counted(128, &mut native, |name| {
            if let Some(digits) = name.strip_suffix(".seal") {
                if child_count != 0
                    || digits.len() != self.namespace.width() * 2
                    || leaf_count >= 128
                {
                    return Err(invalid());
                }
                let parsed = parse_prefix(digits)?;
                if !extends(&parsed, &prefix) {
                    return Err(invalid());
                }
                self.leaf[leaf_count] = CanonicalKey {
                    bytes: parsed.bytes,
                    length: u8::try_from(self.namespace.width()).map_err(|_| invalid())?,
                };
                leaf_count += 1;
            } else {
                if leaf_count != 0
                    || child_count >= 16
                    || name.len() + usize::from(prefix.nibbles) > self.namespace.width() * 2
                {
                    return Err(invalid());
                }
                let full = format!(
                    "{}{}",
                    hex(&prefix.bytes, 0, usize::from(prefix.nibbles)),
                    name
                );
                children[child_count] = parse_prefix(&full)?;
                child_count += 1;
            }
            Ok(())
        });
        self.work.namespace_attempts += native.open_attempts;
        self.work.node_attempts += native.open_attempts;
        self.work.entries += native.names;
        self.work.native_entries += native.entries;
        self.work.directory_reads += native.read_calls;
        result?;
        load_node_sort(
            &mut children[..child_count],
            &prefix,
            &mut self.leaf[..leaf_count],
        )?;
        self.stack[self.depth].children = children;
        self.stack[self.depth].count = u8::try_from(child_count).map_err(|_| invalid())?;
        self.stack[self.depth].loaded = true;
        self.leaf_count = leaf_count;
        self.leaf_next = 0;
        Ok(())
    }
    fn open_current(&mut self) -> io::Result<PrivateDirectory> {
        self.work.namespace_attempts += 1;
        self.work.node_attempts += 1;
        let mut directory = self.root.open_directory(self.namespace.name())?;
        for depth in 1..=self.depth {
            let parent = self.stack[depth - 1].prefix;
            let child = self.stack[depth].prefix;
            let name = hex(
                &child.bytes,
                usize::from(parent.nibbles),
                usize::from(child.nibbles),
            );
            self.work.namespace_attempts += 1;
            self.work.node_attempts += 1;
            directory = directory.open_directory(&name)?;
        }
        Ok(directory)
    }
    pub(crate) fn open_gate(&mut self, key: &CanonicalKey) -> io::Result<std::fs::File> {
        let directory = self.open_current()?;
        self.work.namespace_attempts += 1;
        self.work.metadata_admissions += 1;
        directory.open_file(&key.file_name())
    }
    pub(crate) fn root(&self) -> &PrivateDirectory {
        self.root
    }
}
fn parse_prefix(name: &str) -> io::Result<Prefix> {
    if name.is_empty() || name.len() > 120 {
        return Err(invalid());
    }
    let mut prefix = Prefix::EMPTY;
    prefix.nibbles = u8::try_from(name.len()).map_err(|_| invalid())?;
    for (index, byte) in name.bytes().enumerate() {
        let value = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => return Err(invalid()),
        };
        if index.is_multiple_of(2) {
            prefix.bytes[index / 2] = value << 4;
        } else {
            prefix.bytes[index / 2] |= value;
        }
    }
    Ok(prefix)
}
fn nibble(bytes: &[u8; 60], index: usize) -> u8 {
    if index.is_multiple_of(2) {
        bytes[index / 2] >> 4
    } else {
        bytes[index / 2] & 0x0f
    }
}
fn extends(child: &Prefix, parent: &Prefix) -> bool {
    (0..usize::from(parent.nibbles))
        .all(|index| nibble(&child.bytes, index) == nibble(&parent.bytes, index))
}
fn intersects(prefix: &Prefix, range: &SeekRange) -> bool {
    let mut high = prefix.bytes;
    for index in usize::from(prefix.nibbles)..usize::from(range.first.length) * 2 {
        if index.is_multiple_of(2) {
            high[index / 2] |= 0xf0;
        } else {
            high[index / 2] |= 0x0f;
        }
    }
    prefix.bytes <= range.last.bytes && high >= range.first.bytes
}
pub(crate) fn hex(bytes: &[u8], first: usize, end: usize) -> String {
    let mut out = String::with_capacity(end - first);
    for index in first..end {
        let digit = if index.is_multiple_of(2) {
            bytes[index / 2] >> 4
        } else {
            bytes[index / 2] & 0x0f
        };
        out.push(char::from(b"0123456789abcdef"[usize::from(digit)]));
    }
    out
}
pub(crate) fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
pub(crate) fn retry() -> io::Error {
    io::Error::from(io::ErrorKind::WouldBlock)
}

fn load_node_sort(
    children: &mut [Prefix],
    prefix: &Prefix,
    leaves: &mut [CanonicalKey],
) -> io::Result<()> {
    children.sort_unstable_by_key(|prefix| prefix.bytes);
    for index in 1..children.len() {
        if nibble(&children[index - 1].bytes, usize::from(prefix.nibbles))
            == nibble(&children[index].bytes, usize::from(prefix.nibbles))
        {
            return Err(invalid());
        }
    }
    leaves.sort_unstable();
    for index in 1..leaves.len() {
        if leaves[index - 1] == leaves[index] {
            return Err(invalid());
        }
    }
    Ok(())
}
