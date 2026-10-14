pub mod clock;
pub mod coalitions;
pub mod collector;
pub mod cost;
pub mod interfaces;
pub mod ioreport;
mod kernel;
pub mod machine;
pub mod processes;
pub mod schedule;
pub mod smc;
pub mod tracker;

use std::ffi::OsString;

/// The words the program was given after its name, or a usage error naming `program` and the
/// first word that is not UTF-8, where `std::env::args` would panic on it.
pub fn arguments(program: &str) -> Result<Vec<String>, String> {
    arguments_from(program, std::env::args_os().skip(1))
}

fn arguments_from(
    program: &str,
    words: impl Iterator<Item = OsString>,
) -> Result<Vec<String>, String> {
    words
        .map(|word| {
            word.into_string()
                .map_err(|word| format!("{program}: argument \"{}\" is not UTF-8", word.display()))
        })
        .collect()
}

/// Pushes `item` onto `list`, whose room was taken at startup for every item it holds, so
/// the push never grows it.
pub(crate) fn push_without_growing<T>(list: &mut Vec<T>, item: T) {
    let capacity = list.capacity();
    assert!(list.len() < capacity);

    list.push(item);
    assert_eq!(capacity, list.capacity());
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    #[test]
    fn takes_the_words_a_program_was_given() {
        // Goal: words that are UTF-8 come through as they were given.
        // Method: read two words.
        let words = [OsString::from("300"), OsString::from("--pids")];

        let arguments = arguments_from("forkrate", words.into_iter());

        assert_eq!(Ok(vec!["300".to_owned(), "--pids".to_owned()]), arguments);
    }

    #[test]
    fn refuses_a_word_that_is_not_utf8() {
        // Goal: a word that is not UTF-8 is a usage error naming the program and the word,
        // where the standard library's reading panics.
        // Method: read a word whose second byte, 0xff, starts no UTF-8 character.
        let words = [OsString::from("300"), OsString::from_vec(vec![b'1', 0xff])];

        let arguments = arguments_from("forkrate", words.into_iter());

        assert_eq!(
            Err("forkrate: argument \"1\u{fffd}\" is not UTF-8".to_owned()),
            arguments
        );
    }
}
