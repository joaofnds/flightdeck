use std::cmp::Ordering;

use persistence::{Error, Result};

use super::{
    AppProcessGroupsHistory, GROUPS_MAX, KEY_BYTES_MAX, LEADERS_MAX, NAME_BYTES_MAX, POINTS_MAX,
    ProcessGroup, ProcessGroupReading, ProcessGroupsPoint, RESULT_BYTES_MAX, SERIES_GROUPS_MAX,
};
use crate::cancellation::Operation;
use crate::live_app_response::{KeyKind, Observation, Selected};

pub(crate) struct Reduction<'operation> {
    result: Box<AppProcessGroupsHistory>,
    selected: Selected,
    names: Vec<Mean>,
    key_bytes: usize,
    current: Option<usize>,
    phase: Phase,
    operation: &'operation Operation,
}

struct Mean {
    name: Box<[u8]>,
    amount: u128,
    elapsed_ns: u128,
}

#[derive(Clone, Copy)]
pub(crate) enum Phase {
    Observe,
    Project,
}

pub(crate) struct Capture<'observation> {
    pub observation: &'observation Observation,
    pub elapsed_ns: Option<u64>,
    pub source_available: bool,
    pub membership_complete: bool,
    pub unnamed_members: Option<u32>,
    pub live: bool,
}

pub(crate) const OPERATION_BYTES_MAX: usize = size_of::<Reduction<'static>>()
    + GROUPS_MAX * size_of::<Mean>()
    + KEY_BYTES_MAX
    + RESULT_BYTES_MAX
    + POINTS_MAX * size_of::<Key>()
    + LEADERS_MAX * size_of::<usize>();

const _: () = {
    assert!(GROUPS_MAX == crate::apps::PROCESS_INTERVALS_MAX);
    assert!(size_of::<Mean>() <= 48);
    assert!(OPERATION_BYTES_MAX < 18 * 1024 * 1024);
};

impl<'operation> Reduction<'operation> {
    pub(crate) fn new(
        selected: &Selected,
        kind: KeyKind,
        app_bytes: &[u8],
        operation: &'operation Operation,
    ) -> Result<Box<Self>> {
        if app_bytes.len() > NAME_BYTES_MAX {
            return Err(Error::Bound("process cohort App key"));
        }

        let result = Box::new(AppProcessGroupsHistory {
            selected: *selected.observation(),
            app_kind: kind,
            app_bytes: app_bytes.into(),
            context_seconds: 900,
            named_group_count: 0,
            context_observations_unavailable: 0,
            groups: Vec::with_capacity(SERIES_GROUPS_MAX),
            points: Vec::with_capacity(POINTS_MAX),
        });

        result.validate_bytes()?;
        let reduction = Box::new(Self {
            result,
            selected: *selected,
            names: Vec::with_capacity(GROUPS_MAX),
            key_bytes: 0,
            current: None,
            phase: Phase::Observe,
            operation,
        });

        if reduction.names.capacity() > GROUPS_MAX {
            return Err(Error::Bound("process cohort dictionary capacity"));
        }

        reduction.validate_bytes()?;

        Ok(reduction)
    }

    pub(crate) fn check(&self) -> Result<()> {
        self.operation.check()
    }

    pub(crate) fn begin(&mut self, source: &Capture<'_>, phase: Phase) -> Result<bool> {
        self.operation.check()?;
        self.current = None;
        self.phase = phase;
        if !self.in_range(source.observation)? {
            return Ok(false);
        }

        let index = self
            .result
            .points
            .binary_search_by_key(&key(source.observation), |point| key(&point.observation));
        let index = match index {
            Ok(index) => {
                let point = &self.result.points[index];
                if point.observation != *source.observation {
                    return Err(Error::Bound("conflicting process cohort observation"));
                }

                if point.live {
                    if !source.live {
                        if point.source_available {
                            return Ok(false);
                        }
                    }
                }

                index
            }
            Err(index) => {
                if matches!(phase, Phase::Project) {
                    return Err(Error::Bound("process cohort projection observation"));
                }

                self.insert(source, index)?;
                index
            }
        };

        if matches!(phase, Phase::Observe) {
            let point = &mut self.result.points[index];
            point.elapsed_ns = source.elapsed_ns;
            point.source_available = source.source_available;
            point.membership_complete = source.membership_complete;
            point.unnamed_members = source.unnamed_members;
            point.live = source.live;
        }

        if !source.source_available {
            return Ok(false);
        }

        self.current = Some(index);

        Ok(true)
    }

    pub(crate) fn group(&mut self, name: &[u8], reading: &ProcessGroupReading) -> Result<()> {
        self.operation.check()?;
        if name.is_empty() {
            return Ok(());
        }

        let index = self
            .current
            .ok_or(Error::Bound("process cohort current point"))?;

        match self.phase {
            Phase::Observe => self.group_observe(name, reading, index),
            Phase::Project => {
                let slot = self
                    .result
                    .groups
                    .iter()
                    .position(|group| match group {
                        ProcessGroup::Named(bytes) => bytes.as_ref() == name,
                        ProcessGroup::Other => false,
                    })
                    .or_else(|| {
                        self.result
                            .groups
                            .iter()
                            .position(|group| matches!(group, ProcessGroup::Other))
                    });

                let slot = slot.ok_or(Error::Bound("process cohort missing group"))?;
                self.result.points[index].readings[slot].add(reading)
            }
        }
    }

    pub(crate) fn context(
        &mut self,
        context: &[crate::cpu_history_sample::Identity],
    ) -> Result<()> {
        if context.len() > POINTS_MAX {
            return Err(Error::Bound("process cohort context observations"));
        }

        let mut seen: Vec<Key> = Vec::with_capacity(POINTS_MAX);
        if seen.capacity() > POINTS_MAX {
            return Err(Error::Bound("process cohort identity capacity"));
        }

        for identity in context {
            self.operation.check()?;
            let observation = Observation {
                recording: self.result.selected.recording,
                stamp: crate::apps::Stamp {
                    boot: collector::recording::parse_identity(&identity.boot)?.0,
                    run: collector::recording::parse_identity(&identity.run)?.0,
                    publication: identity.observation.publication,
                    continuous_ns: identity.observation.continuous,
                    awake_ns: None,
                },
                run_tick: identity.observation.run_tick,
                wall_ns: Some(identity.observation.wall),
            };

            match seen.binary_search(&key(&observation)) {
                Ok(_) => return Err(Error::Bound("duplicate process cohort context")),
                Err(index) => seen.insert(index, key(&observation)),
            }

            match self
                .result
                .points
                .binary_search_by_key(&key(&observation), |point| key(&point.observation))
            {
                Ok(index) => self.context_validate(index, &observation)?,
                Err(index) => {
                    if !self.in_range(&observation)? {
                        return Err(Error::Bound("process cohort context range"));
                    }

                    self.insert(
                        &Capture {
                            observation: &observation,
                            elapsed_ns: None,
                            source_available: false,
                            membership_complete: false,
                            unnamed_members: None,
                            live: false,
                        },
                        index,
                    )?;
                }
            }
        }

        Ok(())
    }

    fn context_validate(&self, index: usize, observation: &Observation) -> Result<()> {
        let captured = &self.result.points[index].observation;
        if captured.wall_ns != observation.wall_ns {
            return Err(Error::Bound("process cohort context clock"));
        }

        if captured.run_tick != observation.run_tick {
            return Err(Error::Bound("process cohort context tick"));
        }

        if captured.stamp.continuous_ns != observation.stamp.continuous_ns {
            return Err(Error::Bound("process cohort context continuous clock"));
        }

        Ok(())
    }

    pub(crate) fn choose(&mut self) -> Result<()> {
        self.result.named_group_count = u32::try_from(self.names.len())
            .map_err(|_| Error::Bound("process cohort name count"))?;
        let mut chosen: Vec<usize> = Vec::with_capacity(LEADERS_MAX);
        if chosen.capacity() > LEADERS_MAX {
            return Err(Error::Bound("process cohort leader capacity"));
        }

        for (index, name) in self.names.iter().enumerate() {
            self.operation.check()?;
            let position = chosen.partition_point(|selected| {
                choose_compare(&self.names[*selected], name) != Ordering::Less
            });

            if position < LEADERS_MAX {
                if chosen.len() == LEADERS_MAX {
                    chosen.truncate(LEADERS_MAX - 1);
                }

                chosen.insert(position, index);
            }
        }

        for index in chosen {
            self.result
                .groups
                .push(ProcessGroup::Named(self.names[index].name.clone()));
        }

        if self.names.len() > LEADERS_MAX {
            self.result.groups.push(ProcessGroup::Other);
        }

        self.validate_bytes()
    }

    #[allow(
        clippy::boxed_local,
        reason = "Consume bounded operation state in its heap allocation"
    )]
    pub(crate) fn finish(mut self: Box<Self>) -> Result<Box<AppProcessGroupsHistory>> {
        self.operation.check()?;
        self.result.context_observations_unavailable = u32::try_from(
            self.result
                .points
                .iter()
                .filter(|point| !point.source_available)
                .count(),
        )
        .map_err(|_| Error::Bound("process cohort unavailable observations"))?;
        self.result
            .points
            .sort_by_key(|point| (point.observation.wall_ns, key(&point.observation)));
        self.result.validate_bytes()?;

        Ok(self.result)
    }

    fn insert(&mut self, source: &Capture<'_>, index: usize) -> Result<()> {
        if self.result.points.len() == POINTS_MAX {
            return Err(Error::Bound("process cohort observations"));
        }

        self.result.points.insert(
            index,
            ProcessGroupsPoint {
                observation: *source.observation,
                elapsed_ns: source.elapsed_ns,
                source_available: source.source_available,
                membership_complete: source.membership_complete,
                unnamed_members: source.unnamed_members,
                readings: [ProcessGroupReading::EMPTY; SERIES_GROUPS_MAX],
                live: source.live,
            },
        );

        Ok(())
    }

    fn group_observe(
        &mut self,
        name: &[u8],
        reading: &ProcessGroupReading,
        point: usize,
    ) -> Result<()> {
        let index = match self
            .names
            .binary_search_by(|entry| entry.name.as_ref().cmp(name))
        {
            Ok(index) => index,
            Err(index) => {
                if self.names.len() == GROUPS_MAX {
                    return Err(Error::Bound("process cohort names"));
                }

                if name.len() > NAME_BYTES_MAX {
                    return Err(Error::Bound("process cohort name bytes"));
                }

                if name.len() > KEY_BYTES_MAX - self.key_bytes {
                    return Err(Error::Bound("process cohort key bytes"));
                }

                self.key_bytes += name.len();
                self.names.insert(
                    index,
                    Mean {
                        name: name.into(),
                        amount: 0,
                        elapsed_ns: 0,
                    },
                );
                index
            }
        };

        if let Some(elapsed) = self.result.points[point]
            .elapsed_ns
            .filter(|elapsed| *elapsed > 0)
        {
            if let Some(amount) = reading.measures[0].measured() {
                self.names[index].amount += u128::from(amount);
                self.names[index].elapsed_ns += u128::from(elapsed);
            }
        }

        Ok(())
    }

    fn in_range(&self, observation: &Observation) -> Result<bool> {
        if observation.recording != self.result.selected.recording {
            return Err(Error::Bound("process cohort recording"));
        }

        // Wall nanoseconds floor to the containing second.
        let second = observation
            .wall_ns
            .ok_or(Error::Bound("process cohort point clock"))?
            / 1_000_000_000;

        Ok(self.selected.holds(second))
    }

    fn validate_bytes(&self) -> Result<()> {
        self.result.validate_bytes()?;
        let bytes = size_of::<Self>()
            + self.names.capacity() * size_of::<Mean>()
            + self.key_bytes
            + self.result.bytes();
        if bytes > OPERATION_BYTES_MAX {
            return Err(Error::Bound("process cohort operation bytes"));
        }

        Ok(())
    }
}

type Key = ([u64; 2], [u64; 2], [u64; 2], u64);

fn key(observation: &Observation) -> Key {
    (
        observation.recording,
        observation.stamp.boot,
        observation.stamp.run,
        observation.stamp.publication,
    )
}

fn choose_compare(first: &Mean, second: &Mean) -> Ordering {
    if first.elapsed_ns == 0 {
        return if second.elapsed_ns == 0 {
            Ordering::Equal
        } else {
            Ordering::Less
        };
    }

    if second.elapsed_ns == 0 {
        return Ordering::Greater;
    }

    // Continued fractions use floored quotients to avoid overflowing u128 cross products.
    let (mut numerator, mut denominator) = (first.amount, first.elapsed_ns);
    let (mut other_numerator, mut other_denominator) = (second.amount, second.elapsed_ns);
    let mut reversed = false;
    for _step in 0..256 {
        let ordering = (numerator / denominator).cmp(&(other_numerator / other_denominator));
        if ordering != Ordering::Equal {
            return if reversed {
                ordering.reverse()
            } else {
                ordering
            };
        }

        let (rest, other_rest) = (numerator % denominator, other_numerator % other_denominator);
        if rest == 0 {
            let ordering = rest.cmp(&other_rest);
            return if reversed {
                ordering.reverse()
            } else {
                ordering
            };
        }

        if other_rest == 0 {
            let ordering = rest.cmp(&other_rest);
            return if reversed {
                ordering.reverse()
            } else {
                ordering
            };
        }

        (numerator, denominator) = (denominator, rest);
        (other_numerator, other_denominator) = (other_denominator, other_rest);
        reversed = !reversed;
    }

    unreachable!("Euclid terminates within 256 steps for u128 inputs")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::Cancellation;

    macro_rules! fraction_case {
        ($name:ident, $amount:expr, $elapsed:expr, $other_amount:expr,
            $other_elapsed:expr, $expected:expr) => {
            #[test]
            fn $name() {
                // Compare literal fractions at one independently reported termination boundary.
                let first = Mean {
                    name: b"first".as_slice().into(),
                    amount: $amount,
                    elapsed_ns: $elapsed,
                };
                let second = Mean {
                    name: b"second".as_slice().into(),
                    amount: $other_amount,
                    elapsed_ns: $other_elapsed,
                };

                let ordering = choose_compare(&first, &second);

                assert_eq!(ordering, $expected);
            }
        };
    }

    fraction_case!(
        orders_a_terminating_first_remainder_above_the_other_fraction,
        1,
        2,
        2,
        5,
        Ordering::Greater
    );
    fraction_case!(
        orders_a_terminating_second_remainder_below_the_other_fraction,
        2,
        5,
        1,
        2,
        Ordering::Less
    );
    fraction_case!(
        equates_fractions_when_both_remainders_terminate,
        1,
        2,
        2,
        4,
        Ordering::Equal
    );
    fraction_case!(
        orders_large_fractions_without_overflowing_a_cross_product,
        u128::MAX,
        u128::MAX - 1,
        u128::MAX - 1,
        u128::MAX,
        Ordering::Greater
    );

    #[test]
    fn ranks_context_cpu_by_captured_elapsed_instead_of_mean_sample_percentages() {
        // A has one CPU-second over ten seconds; B has one over its nine measured seconds.
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
        let selected = Observation {
            recording: [1, 2],
            stamp: crate::apps::Stamp {
                boot: [3, 4],
                run: [5, 6],
                publication: 2,
                continuous_ns: Some(10_000_000_000),
                awake_ns: Some(10_000_000_000),
            },
            run_tick: 2,
            wall_ns: Some(1_010_000_000_000),
        };
        let mut first = selected;
        first.stamp.publication = 1;
        first.stamp.continuous_ns = Some(1_000_000_000);
        first.stamp.awake_ns = Some(1_000_000_000);
        first.run_tick = 1;
        first.wall_ns = Some(1_001_000_000_000);
        let mut reduction = Reduction::new(
            &Selected::ending_at(selected).unwrap(),
            KeyKind::Bundle,
            b"/A.app",
            &operation,
        )
        .unwrap();
        let mut reading = ProcessGroupReading::EMPTY;
        reading.present = true;
        reading.members = 1;
        reading.measures[0] = crate::live_app_response::Measure {
            known: 1_000_000_000,
            measured_members: 1,
            unavailable_members: 0,
            overflowed: false,
        };

        let mut capture = Capture {
            observation: &first,
            elapsed_ns: Some(1_000_000_000),
            source_available: true,
            membership_complete: true,
            unnamed_members: Some(0),
            live: false,
        };
        assert!(reduction.begin(&capture, Phase::Observe).unwrap());
        reduction.group(b"A", &reading).unwrap();

        capture.observation = &selected;
        capture.elapsed_ns = Some(9_000_000_000);
        assert!(reduction.begin(&capture, Phase::Observe).unwrap());
        let mut zero = reading;
        zero.measures[0].known = 0;
        reduction.group(b"A", &zero).unwrap();
        reduction.group(b"B", &reading).unwrap();

        reduction.choose().unwrap();

        assert!(
            matches!(&reduction.result.groups[0], ProcessGroup::Named(name)
            if name.as_ref() == b"B")
        );
        assert!(
            matches!(&reduction.result.groups[1], ProcessGroup::Named(name)
            if name.as_ref() == b"A")
        );
    }

    macro_rules! context_floor_case {
        ($name:ident, $wall_ns:expr, $expected:expr) => {
            #[test]
            fn $name() {
                // Selected second 1002 includes seconds 103 through 1002 with fractional endpoints.
                let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
                let selected = Observation {
                    recording: [1, 2],
                    stamp: crate::apps::Stamp {
                        boot: [3, 4],
                        run: [5, 6],
                        publication: 2,
                        continuous_ns: None,
                        awake_ns: None,
                    },
                    run_tick: 2,
                    wall_ns: Some(1_002_999_999_999),
                };
                let reduction = Reduction::new(
                    &crate::live_app_response::Selected::ending_at(selected).unwrap(),
                    KeyKind::Bundle,
                    b"/A.app",
                    &operation,
                )
                .unwrap();
                let mut observation = selected;
                observation.wall_ns = Some($wall_ns);

                let included = reduction.in_range(&observation).unwrap();

                assert_eq!(included, $expected);
            }
        };
    }

    context_floor_case!(
        excludes_fractional_second_before_context_start,
        102_999_999_999,
        false
    );
    context_floor_case!(
        includes_first_nanosecond_of_context_start,
        103_000_000_000,
        true
    );
    context_floor_case!(
        includes_final_nanosecond_of_selected_second,
        1_002_999_999_999,
        true
    );
    context_floor_case!(
        excludes_first_nanosecond_after_selected_second,
        1_003_000_000_000,
        false
    );
}
