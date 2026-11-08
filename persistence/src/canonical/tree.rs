use super::node::{Entry, Grammar};
use crate::resources::{ByteBudget, ByteReservation};
use collector::recording::bootstrap::StateObservation;
use collector::recording::private::{DirectoryWork, PrivateDirectory};
use collector::ring::{Identity, RunIdentity};
use std::io;
use std::sync::Arc;

/// The working memory one cursor reserves while it is open.
const CURSOR_BYTES: usize = 256 * 1024;
/// The node opens one step spends at most before it returns `Continue`.
const STEP_NODE_ATTEMPTS_MAX: u64 = 4096;
/// The levels a walk holds at most, which covers the root and one for each of a key's 120 nibbles.
const STACK_FRAMES_MAX: usize = 124;
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
    #[must_use]
    pub fn name(self) -> &'static str {
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
    pub(crate) namespace: Namespace,
    pub(crate) prefix: [u8; 60],
    pub(crate) nibbles: u8,
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
    pub links: u64,
    pub full_syncs: u64,
    pub creates: u64,
    pub renames: u64,
    pub removals: u64,
}
impl ReadWork {
    pub(crate) fn add(&mut self, work: Self) {
        self.namespace_attempts += work.namespace_attempts;
        self.raw_admissions += work.raw_admissions;
        self.metadata_admissions += work.metadata_admissions;
        self.entries += work.entries;
        self.native_entries += work.native_entries;
        self.directory_reads += work.directory_reads;
        self.node_attempts += work.node_attempts;
        self.links += work.links;
        self.full_syncs += work.full_syncs;
        self.creates += work.creates;
        self.renames += work.renames;
        self.removals += work.removals;
    }
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
    /// The frames from the root down to the node the walk is at, never empty.
    stack: Vec<Frame>,
    leaf: [CanonicalKey; 128],
    leaf_count: usize,
    leaf_next: usize,
    ended: bool,
    work: ReadWork,
    query_entries: u64,
    _reservation: ByteReservation,
}
const _: () = assert!(
    size_of::<[Frame; STACK_FRAMES_MAX]>() + size_of::<[CanonicalKey; 128]>() + 4096 < CURSOR_BYTES
);
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
            .reserve(CURSOR_BYTES)
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
        Ok(Self::from_state(
            root,
            namespace,
            range,
            state,
            work,
            reservation,
        ))
    }
    fn from_state(
        root: &'root PrivateDirectory,
        namespace: Namespace,
        range: SeekRange,
        state: StateObservation<'root, 'state>,
        work: ReadWork,
        reservation: ByteReservation,
    ) -> Self {
        Self {
            root,
            state,
            namespace,
            range,
            stack: vec![Frame::EMPTY],
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
        }
    }
    pub(crate) fn reset(&mut self, namespace: Namespace, range: SeekRange) -> io::Result<()> {
        self.check_snapshot()?;
        self.namespace = namespace;
        self.range = range;
        self.stack.truncate(1);
        self.stack[0] = Frame::EMPTY;
        self.leaf_count = 0;
        self.leaf_next = 0;
        self.ended = false;
        self.query_entries = self.work.entries;
        Ok(())
    }
    pub(crate) fn add_work(&mut self, work: ReadWork) {
        self.work.add(work);
    }
    #[must_use]
    pub fn current_locator(&self) -> NodeLocator {
        let prefix = self.stack[self.depth()].prefix;
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
        self.next_limited(STEP_NODE_ATTEMPTS_MAX)
    }
    pub(crate) fn next_limited(&mut self, limit: u64) -> io::Result<ReadStep> {
        let start = self.work;
        self.walk_checked(|cursor| cursor.next_inner(limit, &start))
    }
    /// Appends to `keys` the keys one step finds, up to `keys_max` in all, checking the snapshot
    /// once before them and once after rather than around each, since the keys of a leaf already
    /// read cost no read of their own. Returns whether the range holds no further key. The step
    /// spends no more than `step` may, so it may return before `keys` holds `keys_max` keys.
    pub(crate) fn step_keys(
        &mut self,
        keys: &mut Vec<CanonicalKey>,
        keys_max: usize,
    ) -> io::Result<bool> {
        assert!(keys.len() < keys_max);
        let start = self.work;
        self.walk_checked(|cursor| {
            for _key in keys.len()..keys_max {
                match cursor.next_inner(STEP_NODE_ATTEMPTS_MAX, &start)? {
                    ReadStep::Found(key) => keys.push(key),
                    ReadStep::Continue => return Ok(false),
                    ReadStep::End => return Ok(true),
                }
            }
            Ok(false)
        })
    }
    /// Runs `walk` between two checks of the snapshot, which a step's result depends on.
    fn walk_checked<T>(&mut self, walk: impl FnOnce(&mut Self) -> io::Result<T>) -> io::Result<T> {
        self.check_snapshot()?;
        let result = walk(self);
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
    /// Walks on to the next key, spending no more than `limit` node opens and the boundary's
    /// entries since `start`, the work when the step began.
    fn next_inner(&mut self, limit: u64, start: &ReadWork) -> io::Result<ReadStep> {
        let initial = start.node_attempts;
        let entries = start.entries;
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
            if !self.stack[self.depth()].loaded {
                let required = collector::u64_from_usize(self.depth() + 2 + 132);
                if self.work.node_attempts - initial + required > limit
                    || self.work.entries - entries + 128 > BOUNDARY_ENTRIES
                {
                    return Ok(ReadStep::Continue);
                }
                self.load_node()?;
                continue;
            }
            let depth = self.depth();
            let frame = &mut self.stack[depth];
            if frame.next < frame.count {
                let index = match self.range.direction {
                    Direction::Forward => usize::from(frame.next),
                    Direction::Reverse => usize::from(frame.count - 1 - frame.next),
                };
                frame.next += 1;
                let child = frame.children[index];
                if intersects(&child, &self.range) {
                    self.descend(child)?;
                }
            } else if depth == 0 {
                self.ended = true;
            } else {
                self.stack.pop();
            }
        }
        Ok(ReadStep::Continue)
    }
    /// Steps down into `child`. The stack doubles as it fills, up to `STACK_FRAMES_MAX` frames,
    /// so a walk allocates the levels it descends and no more than its reservation covers.
    fn descend(&mut self, child: Prefix) -> io::Result<()> {
        let frames = self.stack.len();
        if frames == STACK_FRAMES_MAX {
            return Err(invalid());
        }

        if frames == self.stack.capacity() {
            self.stack
                .reserve_exact((frames * 2).min(STACK_FRAMES_MAX) - frames);
        }
        self.stack.push(Frame {
            prefix: child,
            ..Frame::EMPTY
        });
        Ok(())
    }
    /// The level of the node the walk is at, 0 at the root.
    fn depth(&self) -> usize {
        self.stack.len() - 1
    }
    fn load_node(&mut self) -> io::Result<()> {
        let directory = self.open_current()?;
        let locator = self.current_locator();
        let mut children = [Prefix::EMPTY; 16];
        let mut child_count = 0;
        let mut leaf_count = 0;
        let mut native = DirectoryWork::default();
        let mut grammar = Grammar::new(&locator);
        let result = directory.visit_names_counted(128, &mut native, |name| {
            match grammar.admit(name)? {
                Entry::Seal(key) => {
                    self.leaf[leaf_count] = key;
                    leaf_count += 1;
                }
                Entry::Child(child) => {
                    children[child_count] = Prefix {
                        bytes: child.prefix,
                        nibbles: child.nibbles,
                    };
                    child_count += 1;
                }
            }
            Ok(())
        });
        self.work.namespace_attempts += native.open_attempts;
        self.work.node_attempts += native.open_attempts;
        self.work.entries += native.names;
        self.work.native_entries += native.entries;
        self.work.directory_reads += native.read_calls;
        result?;
        children[..child_count].sort_unstable_by_key(|prefix| prefix.bytes);
        self.leaf[..leaf_count].sort_unstable();
        let depth = self.depth();
        self.stack[depth].children = children;
        self.stack[depth].count = u8::try_from(child_count).map_err(|_| invalid())?;
        self.stack[depth].loaded = true;
        self.leaf_count = leaf_count;
        self.leaf_next = 0;
        Ok(())
    }
    fn open_current(&mut self) -> io::Result<PrivateDirectory> {
        self.work.namespace_attempts += 1;
        self.work.node_attempts += 1;
        let mut directory = self.root.open_directory(self.namespace.name())?;
        for depth in 1..=self.depth() {
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
        let gate = self.open_gate_name(key);
        // This check dominates absence here as it does in the walk, since a removal may have taken
        // the name since the step that found it.
        if gate.is_err() {
            self.check_snapshot()?;
        }

        gate
    }
    fn open_gate_name(&mut self, key: &CanonicalKey) -> io::Result<std::fs::File> {
        let directory = self.open_current()?;
        self.work.namespace_attempts += 1;
        self.work.metadata_admissions += 1;
        directory.open_file(&key.file_name())
    }
    pub(crate) fn root(&self) -> &PrivateDirectory {
        self.root
    }
}
impl<'root> CanonicalCursor<'root, 'static> {
    pub(crate) fn begin_owned(
        root: &'root PrivateDirectory,
        recording: Identity,
        namespace: Namespace,
        range: SeekRange,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<Self> {
        if usize::from(range.first.length) != namespace.width()
            || range.first.length != range.last.length
            || range.first > range.last
        {
            return Err(invalid());
        }
        let reservation = budget
            .reserve(CURSOR_BYTES)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let mut work = ReadWork::default();
        let state = StateObservation::begin_owned(root, recording, &mut work.namespace_attempts)?
            .ok_or_else(retry)?;
        Ok(Self::from_state(
            root,
            namespace,
            range,
            state,
            work,
            reservation,
        ))
    }
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
