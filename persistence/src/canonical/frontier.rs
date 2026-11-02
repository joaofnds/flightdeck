use super::metadata::CanonicalMetadata;
use super::rows::AdmittedCanonical;
use super::tree::{CanonicalCursor, Namespace, ReadStep, ReadWork, SeekRange, invalid, retry};
use crate::resources::ByteBudget;
use crate::transaction::Predecessor;
use collector::recording::private::PrivateDirectory;
use collector::ring::RunIdentity;
use std::io;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct SourceFrontier {
    predecessor: Predecessor,
    publication_end: u64,
    event_end: u64,
    prior_window: Option<u64>,
    ordered_window: Option<u64>,
    captured: bool,
    complete: bool,
    trailing_unknown: bool,
    unknown_provenance: Option<crate::transaction::UnknownProvenance>,
}
impl SourceFrontier {
    #[must_use]
    pub fn predecessor(&self) -> &Predecessor {
        &self.predecessor
    }
    #[must_use]
    pub fn publication_end(&self) -> u64 {
        self.publication_end
    }
    #[must_use]
    pub fn event_end(&self) -> u64 {
        self.event_end
    }
    #[must_use]
    pub fn prior_window(&self) -> Option<u64> {
        self.prior_window
    }
    #[must_use]
    pub fn trailing_unknown(&self) -> bool {
        self.trailing_unknown
    }
    #[must_use]
    pub fn unknown_provenance(&self) -> Option<&crate::transaction::UnknownProvenance> {
        self.unknown_provenance.as_ref()
    }
    #[must_use]
    pub fn complete(&self) -> bool {
        self.complete
    }
    pub(crate) fn read(
        root: &PrivateDirectory,
        run: &RunIdentity,
        budget: &Arc<ByteBudget>,
        work: &mut ReadWork,
    ) -> io::Result<Option<Self>> {
        let mut state = [0; 256];
        let mut query = SourceFrontierRead::begin_counted(root, *run, &mut state, budget, work)?;
        let result = (|| {
            for _ in 0..3 {
                let remaining = 4096_u64.saturating_sub(query.work().node_attempts);
                if query.step_limited(remaining)? == FrontierStep::Complete {
                    return Ok(query.result()?.cloned());
                }
            }
            Err(retry())
        })();
        *work = query.work();
        result
    }
    pub(crate) fn matches_input(&self, input: &crate::transaction::AttemptInput) -> io::Result<()> {
        if self.complete {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "source already has a certified terminal disposition",
            ));
        }
        if self.trailing_unknown {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "canonical trailing source extent remains unresolved",
            ));
        }
        if self.predecessor != input.losses.predecessor
            || self.publication_end != input.losses.publication_start
            || self.event_end != input.losses.event_start
            || self.prior_window != input.losses.prior_window
            || self.captured != input.captured
        {
            return Err(invalid());
        }
        if let crate::transaction::AttemptOperation::Ordinary { window, .. } = input.operation {
            if self
                .ordered_window
                .is_some_and(|previous| window < previous)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrontierStep {
    Continue,
    Complete,
}

pub struct SourceFrontierRead<'root, 'state> {
    cursor: CanonicalCursor<'root, 'state>,
    run: RunIdentity,
    budget: Arc<ByteBudget>,
    stage: usize,
    selected: Option<AdmittedCanonical>,
    ordinary_tail: Option<(u64, Option<u64>)>,
    result: Option<SourceFrontier>,
    failed: bool,
}
impl<'root, 'state> SourceFrontierRead<'root, 'state> {
    pub fn begin(
        root: &'root PrivateDirectory,
        run: RunIdentity,
        state: &'state mut [u8; 256],
        budget: &Arc<ByteBudget>,
    ) -> io::Result<Self> {
        Self::begin_counted(root, run, state, budget, &mut ReadWork::default())
    }
    fn begin_counted(
        root: &'root PrivateDirectory,
        run: RunIdentity,
        state: &'state mut [u8; 256],
        budget: &Arc<ByteBudget>,
        work: &mut ReadWork,
    ) -> io::Result<Self> {
        let cursor = CanonicalCursor::begin_counted(
            root,
            run.recording,
            Namespace::Ordinary,
            SeekRange::source_tail(Namespace::Ordinary, &run)?,
            state,
            budget,
            work,
        )?;
        Ok(Self {
            cursor,
            run,
            budget: Arc::clone(budget),
            stage: 0,
            selected: None,
            ordinary_tail: None,
            result: None,
            failed: false,
        })
    }
    #[must_use]
    pub fn work(&self) -> ReadWork {
        self.cursor.work()
    }
    pub fn step(&mut self) -> io::Result<FrontierStep> {
        self.step_limited(4096)
    }
    fn step_limited(&mut self, limit: u64) -> io::Result<FrontierStep> {
        if self.failed {
            return Err(invalid());
        }
        let initial = self.cursor.check_snapshot();
        if let Err(error) = initial {
            self.failed = true;
            return Err(error);
        }
        let result = self.step_inner(limit);
        let stable = self.cursor.check_snapshot();
        if stable.is_err() || result.is_err() {
            self.failed = true;
        }
        stable?;
        result
    }
    fn step_inner(&mut self, limit: u64) -> io::Result<FrontierStep> {
        if self.stage == 3 {
            return Ok(FrontierStep::Complete);
        }
        let namespace = [
            Namespace::Ordinary,
            Namespace::Terminal,
            Namespace::Fragments,
        ][self.stage];
        match self.cursor.next_limited(limit)? {
            ReadStep::Continue => return Ok(FrontierStep::Continue),
            ReadStep::End => {}
            ReadStep::Found(key) => {
                let gate = self.cursor.open_gate(&key)?;
                let mut work = ReadWork::default();
                let candidate = super::rows::admit(
                    self.cursor.root(),
                    gate,
                    namespace,
                    &key,
                    &self.run,
                    &self.budget,
                    &mut work,
                );
                self.cursor.add_work(work);
                let candidate = candidate?;
                if namespace == Namespace::Ordinary {
                    self.ordinary_tail = Some((
                        candidate.metadata.window,
                        candidate.metadata.effective_window(),
                    ));
                }
                self.selected = Some(choose(self.selected.take(), candidate)?);
            }
        }
        self.stage += 1;
        if self.stage == 3 {
            if let (Some(candidate), Some((_, saved_window))) = (&self.selected, self.ordinary_tail)
            {
                if candidate.metadata.effective_window() != saved_window {
                    return Err(invalid());
                }
            }
            self.result = self.selected.take().map(|candidate| {
                source_value(&candidate, self.ordinary_tail.map(|(window, _)| window))
            });
            Ok(FrontierStep::Complete)
        } else {
            let namespace = [
                Namespace::Ordinary,
                Namespace::Terminal,
                Namespace::Fragments,
            ][self.stage];
            self.cursor
                .reset(namespace, SeekRange::source_tail(namespace, &self.run)?)?;
            Ok(FrontierStep::Continue)
        }
    }
    pub fn result(&mut self) -> io::Result<Option<&SourceFrontier>> {
        if self.failed {
            return Err(invalid());
        }
        self.cursor.check_snapshot()?;
        if self.stage != 3 {
            return Err(retry());
        }
        Ok(self.result.as_ref())
    }
    pub(crate) fn validate_owner(
        &mut self,
        root: &PrivateDirectory,
        run: &RunIdentity,
    ) -> io::Result<()> {
        self.cursor.check_snapshot()?;
        if !self.cursor.root().same_inode(root)? || self.run != *run {
            return Err(invalid());
        }
        Ok(())
    }
}
fn source_value(candidate: &AdmittedCanonical, ordered_window: Option<u64>) -> SourceFrontier {
    let metadata = &candidate.metadata;
    SourceFrontier {
        predecessor: metadata.reference(),
        publication_end: metadata.loss.publication_end,
        event_end: metadata.loss.event_end,
        prior_window: metadata.effective_window(),
        ordered_window,
        captured: metadata.captured,
        trailing_unknown: metadata.trailing_unknown,
        unknown_provenance: metadata.loss.inherited.unknown.clone(),
        complete: metadata.certified_terminal(),
    }
}
fn choose(
    previous: Option<AdmittedCanonical>,
    next: AdmittedCanonical,
) -> io::Result<AdmittedCanonical> {
    let Some(previous) = previous else {
        return Ok(next);
    };
    let previous_metadata = &previous.metadata;
    let next_metadata = &next.metadata;
    if previous_metadata.transaction == next_metadata.transaction
        && previous.device == next.device
        && previous.inode == next.inode
        && previous_metadata == next_metadata
    {
        return Ok(previous);
    }

    if previous_metadata.captured != next_metadata.captured {
        return Err(invalid());
    }

    if choose_precedes(previous_metadata, next_metadata) {
        choose_validate_successor(previous_metadata, next_metadata)?;
        Ok(next)
    } else if choose_precedes(next_metadata, previous_metadata) {
        choose_validate_successor(next_metadata, previous_metadata)?;
        Ok(previous)
    } else {
        Err(invalid())
    }
}

fn choose_precedes(earlier: &CanonicalMetadata, later: &CanonicalMetadata) -> bool {
    let earlier_end = (earlier.loss.publication_end, earlier.loss.event_end);
    let later_end = (later.loss.publication_end, later.loss.event_end);
    earlier_end.0 <= later_end.0
        && earlier_end.1 <= later_end.1
        && (earlier_end != later_end
            || (later.certified_terminal()
                && later.loss.inherited.predecessor == earlier.reference()))
}

fn choose_validate_successor(
    earlier: &CanonicalMetadata,
    later: &CanonicalMetadata,
) -> io::Result<()> {
    let domain = &later.loss.inherited;
    if earlier.certified_terminal()
        || earlier.loss.publication_end > domain.publication_start
        || earlier.loss.event_end > domain.event_start
        || earlier.effective_window() > domain.prior_window
    {
        return Err(invalid());
    }

    // Independent namespace tails can have intervening transactions. Only an exact
    // predecessor reference promises adjacency; the ordinary tail pins the saved window.
    if domain.predecessor == earlier.reference()
        && (earlier.loss.publication_end != domain.publication_start
            || earlier.loss.event_end != domain.event_start
            || earlier.effective_window() != domain.prior_window)
    {
        return Err(invalid());
    }

    Ok(())
}
