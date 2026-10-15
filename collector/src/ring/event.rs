//! A process event, as the writer stores it in the event log and a reader loads it back, as
//! whole 64-bit words like a tick's readings.

use std::sync::atomic::AtomicU64;

use super::layout::{EVENT_SEQUENCE, EVENT_WORDS};
use super::tick::{
    Halves, USAGE_WORDS, high_half, load_words, low_half, store_words, usage_from, usage_words,
};
use crate::processes::Unreadable;
use crate::tracker::{Event, Noticed};

/// The words after the sequence word: the tick, the kind and the pid, what the kind adds,
/// the coalition of a start, and the last usage of an exit.
const EVENT_TICK: usize = 1;
const EVENT_KIND_PID: usize = 2;
const EVENT_DETAIL: usize = 3;
const EVENT_COALITION: usize = 4;
const EVENT_USAGE: usize = 5;
const _: () = assert!(EVENT_SEQUENCE < EVENT_TICK);
const _: () = assert!(EVENT_USAGE + USAGE_WORDS == EVENT_WORDS);

const KIND_START: u32 = 1;
const KIND_EXEC: u32 = 2;
const KIND_EXIT: u32 = 3;

/// What an exit's record says of its last usage, in the low half of its detail word.
const LAST_USAGE: u32 = 0;
const LAST_REFUSED: u32 = 1;
const LAST_GONE: u32 = 2;

/// When the tracker noticed an exit, in the high half of its detail word.
const NOTICED_ON_EXIT: u32 = 0;
const NOTICED_ALREADY_EXITED: u32 = 1;

/// Stores `event` of `tick` in `record`, a record of the log whose sequence word the caller
/// keeps. Every word is stored, so no word of an event the record held before stays.
pub(super) fn store_event(record: &[AtomicU64], tick: u64, event: &Event) {
    assert_eq!(EVENT_WORDS, record.len());

    store_words(&record[EVENT_TICK..], &event_words(tick, event));
}

/// The tick and the event `record` holds, or `None` where its words make no event, which a
/// record the writer overwrote during the load may hold.
pub(super) fn load_event(record: &[AtomicU64]) -> Option<(u64, Event)> {
    assert_eq!(EVENT_WORDS, record.len());

    event_from(&load_words(&record[EVENT_TICK..]))
}

/// The words of a record past its sequence word.
const STORED_WORDS: usize = EVENT_WORDS - EVENT_TICK;

/// The index within the stored words of the record word at `index`.
const fn stored(index: usize) -> usize {
    index - EVENT_TICK
}

fn event_words(tick: u64, event: &Event) -> [u64; STORED_WORDS] {
    let mut words = [0; STORED_WORDS];
    words[stored(EVENT_TICK)] = tick;
    match event {
        Event::Start { pid, coalition } => {
            words[stored(EVENT_KIND_PID)] = Halves {
                low: KIND_START,
                high: pid.cast_unsigned(),
            }
            .word();
            if let Some(coalition) = coalition {
                words[stored(EVENT_DETAIL)] = 1;
                words[stored(EVENT_COALITION)] = *coalition;
            }
        }
        Event::Exec { pid } => {
            words[stored(EVENT_KIND_PID)] = Halves {
                low: KIND_EXEC,
                high: pid.cast_unsigned(),
            }
            .word();
        }
        Event::Exit { pid, last, noticed } => {
            words[stored(EVENT_KIND_PID)] = Halves {
                low: KIND_EXIT,
                high: pid.cast_unsigned(),
            }
            .word();
            let last_word = match last {
                Ok(usage) => {
                    words[stored(EVENT_USAGE)..].copy_from_slice(&usage_words(usage));
                    LAST_USAGE
                }
                Err(Unreadable::Refused) => LAST_REFUSED,
                Err(Unreadable::Gone) => LAST_GONE,
            };
            let noticed_word = match noticed {
                Noticed::OnExit => NOTICED_ON_EXIT,
                Noticed::AlreadyExited => NOTICED_ALREADY_EXITED,
            };
            words[stored(EVENT_DETAIL)] = Halves {
                low: last_word,
                high: noticed_word,
            }
            .word();
        }
    }
    words
}

fn event_from(words: &[u64; STORED_WORDS]) -> Option<(u64, Event)> {
    let kind_pid = words[stored(EVENT_KIND_PID)];
    let pid = high_half(kind_pid).cast_signed();
    let detail = words[stored(EVENT_DETAIL)];
    let event = match low_half(kind_pid) {
        KIND_START => Event::Start {
            pid,
            coalition: match detail {
                0 => None,
                1 => Some(words[stored(EVENT_COALITION)]),
                _ => return None,
            },
        },
        KIND_EXEC => Event::Exec { pid },
        KIND_EXIT => {
            let mut usage = [0; USAGE_WORDS];
            usage.copy_from_slice(&words[stored(EVENT_USAGE)..]);
            let last = match low_half(detail) {
                LAST_USAGE => Ok(usage_from(&usage)),
                LAST_REFUSED => Err(Unreadable::Refused),
                LAST_GONE => Err(Unreadable::Gone),
                _ => return None,
            };
            let noticed = match high_half(detail) {
                NOTICED_ON_EXIT => Noticed::OnExit,
                NOTICED_ALREADY_EXITED => Noticed::AlreadyExited,
                _ => return None,
            };
            Event::Exit { pid, last, noticed }
        }
        _ => return None,
    };
    Some((words[stored(EVENT_TICK)], event))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processes::Usage;
    use std::sync::atomic::Ordering;

    fn usage() -> Usage {
        Usage {
            awake_ns_at_start: 1,
            user_ns: 2,
            system_ns: 3,
            energy_nj: 4,
            instructions: 5,
            super_instructions: 6,
            cycles: 7,
            idle_wakeups: 8,
            interrupt_wakeups: 9,
            pageins: 10,
            resident_bytes: 11,
            footprint_bytes: 12,
            disk_read_bytes: 13,
            disk_written_bytes: 14,
        }
    }

    fn record() -> Vec<AtomicU64> {
        (0..EVENT_WORDS).map(|_| AtomicU64::new(0)).collect()
    }

    /// Stores `event` of tick 9 over a record that held an exit with a usage, and loads it.
    fn store_and_load(event: &Event) -> Option<(u64, Event)> {
        let record = record();
        let exit = Event::Exit {
            pid: 1,
            last: Ok(usage()),
            noticed: Noticed::AlreadyExited,
        };
        store_event(&record, 3, &exit);

        store_event(&record, 9, event);
        load_event(&record)
    }

    #[test]
    fn each_kind_of_event_loads_back_as_it_was_stored() {
        // Goal: a reader gets back every event the writer stored, of each kind and detail,
        // with no word of the event the record held before.
        // Method: store each event over a record that held an exit, and load it.
        let events = [
            Event::Start {
                pid: 7,
                coalition: Some(u64::MAX),
            },
            Event::Start {
                pid: 7,
                coalition: None,
            },
            Event::Exec { pid: i32::MAX },
            Event::Exit {
                pid: 7,
                last: Ok(usage()),
                noticed: Noticed::OnExit,
            },
            Event::Exit {
                pid: 7,
                last: Err(Unreadable::Refused),
                noticed: Noticed::AlreadyExited,
            },
            Event::Exit {
                pid: 7,
                last: Err(Unreadable::Gone),
                noticed: Noticed::OnExit,
            },
        ];

        for event in events {
            let loaded = store_and_load(&event);

            assert_eq!(Some((9, event)), loaded);
        }
    }

    #[test]
    fn a_record_whose_words_make_no_event_does_not_load() {
        // Goal: a record the writer overwrote during the load, whose words mix two events,
        // loads as no event rather than as one nobody stored.
        // Method: store an event, then set its kind, a start's detail, and an exit's last
        // usage and notice to values no event has, one at a time.
        let start = Event::Start {
            pid: 7,
            coalition: None,
        };
        let exit = Event::Exit {
            pid: 7,
            last: Err(Unreadable::Gone),
            noticed: Noticed::OnExit,
        };
        let broken = [
            (&start, EVENT_KIND_PID, Halves { low: 4, high: 7 }.word()),
            (&start, EVENT_DETAIL, 2),
            (
                &exit,
                EVENT_DETAIL,
                Halves {
                    low: 3,
                    high: NOTICED_ON_EXIT,
                }
                .word(),
            ),
            (
                &exit,
                EVENT_DETAIL,
                Halves {
                    low: LAST_GONE,
                    high: 2,
                }
                .word(),
            ),
        ];

        for (event, index, word) in broken {
            let record = record();
            store_event(&record, 9, event);
            record[index].store(word, Ordering::Relaxed);

            assert_eq!(None, load_event(&record), "word {index} as {word}");
        }
    }
}
