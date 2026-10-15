//! Invalid recording roots must not be interpreted as engine glob patterns.

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use persistence::{Error, query};

    #[test]
    fn refuses_a_bracket_in_the_recording_path_before_discovery() {
        // Goal: a literal recording cannot redirect its query to a matching sibling.
        // Method: query a bracket path before any data directory exists.
        let result = query::processes_at_second(Path::new("/private/tmp/rec[1]"), 1);

        assert!(matches!(result, Err(Error::Bound(_))), "{result:?}");
    }

    #[test]
    fn refuses_a_star_in_the_recording_path_before_discovery() {
        // Goal: wildcard expansion cannot broaden the selected recording.
        // Method: query a star path before any data directory exists.
        let result = query::processes_at_second(Path::new("/private/tmp/rec*"), 1);

        assert!(matches!(result, Err(Error::Bound(_))), "{result:?}");
    }

    #[test]
    fn refuses_a_question_mark_in_the_recording_path_before_discovery() {
        // Goal: a wildcard cannot substitute another recording's files.
        // Method: query a question-mark path before any data directory exists.
        let result = query::processes_at_second(Path::new("/private/tmp/rec?"), 1);

        assert!(matches!(result, Err(Error::Bound(_))), "{result:?}");
    }

    #[test]
    fn refuses_a_non_utf8_recording_path_without_replacing_its_bytes() {
        // Goal: path conversion cannot silently select a different directory.
        // Method: pass invalid UTF-8 and require refusal before filesystem discovery.
        let path = Path::new(OsStr::from_bytes(b"/private/tmp/rec\xff"));

        let result = query::processes_at_second(path, 1);

        assert!(matches!(result, Err(Error::Bound(_))), "{result:?}");
    }
}
