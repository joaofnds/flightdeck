//! Holds the repository to the rules of `docs/tiger-style.md` and `AGENTS.md` that rustfmt
//! and clippy cannot check, as `TigerBeetle`'s `test "tidy"` does. It is only a test, run by
//! `cargo test`.
#![cfg(test)]

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str;

const COLUMNS_MAX: usize = 100;
const FUNCTION_LINES_MAX: usize = 70;

#[test]
fn repository_is_tidy() {
    // Goal: every file git tracks meets the rules below.
    // Method: list the files with git, apply each rule to each, and report every finding at
    // once, so one run shows all that a change has to fix.
    let root = repository_root();
    let paths = repository_paths(&root);
    assert!(paths.iter().any(|path| path == "Cargo.toml"));

    let mut findings = Vec::new();
    for path in &paths {
        match fs::read(root.join(path)) {
            Ok(bytes) => findings.extend(file_findings(path, &bytes)),
            // A file deleted in the working tree stays in the index until the deletion is
            // staged.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("cannot read {path}: {error}"),
        }
    }

    assert!(findings.is_empty(), "\n{}\n", findings.join("\n"));
}

#[test]
fn release_profile_traps_overflow_and_aborts_on_a_panic() {
    // Goal: the release build keeps the checks Tiger Style keeps in production.
    // Method: read the `[profile.release]` table of the workspace manifest.
    let manifest = repository_root().join("Cargo.toml");
    let text = match fs::read_to_string(&manifest) {
        Ok(text) => text,
        Err(error) => panic!("cannot read {}: {error}", manifest.display()),
    };

    let settings = release_profile_settings(&text);

    assert!(settings.contains(&"overflow-checks = true"), "{settings:?}");
    assert!(settings.contains(&"panic = \"abort\""), "{settings:?}");
}

/// Lists the lines of the `[profile.release]` table in a manifest, trimmed, up to the next
/// table.
fn release_profile_settings(text: &str) -> Vec<&str> {
    let mut settings = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let setting = line.trim();
        if setting.starts_with('[') {
            inside = setting == "[profile.release]";
        } else {
            if inside {
                settings.push(setting);
            }
        }
    }
    settings
}

fn repository_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    match manifest.parent() {
        Some(root) => root.to_path_buf(),
        None => panic!("{} has no parent", manifest.display()),
    }
}

/// Lists the files git tracks, relative to `root`. It leaves out untracked files, since what
/// each clone ignores depends on its own global ignore file, and CI checks a clean clone.
fn repository_paths(root: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args(["ls-files", "-z", "--cached"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let output = match output {
        Ok(output) => output,
        Err(error) => panic!("cannot run git ls-files: {error}"),
    };
    assert!(
        output.status.success(),
        "git ls-files: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let mut paths = Vec::new();
    for name in output.stdout.split(|&byte| byte == 0) {
        if !name.is_empty() {
            match str::from_utf8(name) {
                Ok(path) => paths.push(path.to_owned()),
                Err(error) => panic!("a path git lists is not UTF-8: {error}"),
            }
        }
    }
    paths
}

/// Applies every rule to one file and returns its findings, each led by the file's path.
fn file_findings(path: &str, bytes: &[u8]) -> Vec<String> {
    let mut findings = Vec::new();
    if script(path, bytes) {
        findings.push(format!("{path}: a script, which belongs in a Rust binary"));
    }
    if toolchain_file(path) {
        findings.push(format!(
            "{path}: a toolchain file, which makes rustup install a toolchain on every clone"
        ));
    }

    let extension = Path::new(path).extension().and_then(OsStr::to_str);
    if matches!(extension, Some("rs" | "toml")) {
        match str::from_utf8(bytes) {
            Ok(text) => {
                for number in long_lines(text) {
                    findings.push(format!("{path}:{number}: over {COLUMNS_MAX} columns"));
                }
                if extension == Some("rs") {
                    for number in discards(text) {
                        findings.push(format!("{path}:{number}: discards a value"));
                    }
                    for number in long_functions(text) {
                        findings.push(format!(
                            "{path}:{number}: function over {FUNCTION_LINES_MAX} lines"
                        ));
                    }
                }
            }
            Err(error) => findings.push(format!("{path}: not UTF-8, {error}")),
        }
    }
    findings
}

#[test]
fn file_findings_applies_every_rule_to_rust_source() {
    // Goal: Rust source is held to the column, discard and function rules, each finding led
    // by the path and the line.
    // Method: a function one line past the limit, then a long comment and a discard.
    let text = file_findings_text();

    let findings = file_findings("collector/src/main.rs", text.as_bytes());

    assert_eq!(
        findings,
        [
            "collector/src/main.rs:72: over 100 columns",
            "collector/src/main.rs:73: discards a value",
            "collector/src/main.rs:1: function over 70 lines",
        ]
    );
}

#[test]
fn file_findings_applies_only_the_column_rule_to_toml() {
    // Goal: a TOML file is held to the column rule and to no rule about Rust code.
    // Method: the Rust source of the test above under a TOML name.
    let text = file_findings_text();

    let findings = file_findings("Cargo.toml", text.as_bytes());

    assert_eq!(findings, ["Cargo.toml:72: over 100 columns"]);
}

#[test]
fn file_findings_applies_no_text_rule_to_a_document() {
    // Goal: Markdown may run past the limits, as the Tiger page's departures allow.
    // Method: the Rust source of the test above under a Markdown name.
    let text = file_findings_text();

    let findings = file_findings("README.md", text.as_bytes());

    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn file_findings_reports_a_script() {
    // Goal: a script is reported by its path alone.
    // Method: a shell script by its extension.
    let findings = file_findings("tools/measure.sh", b"echo\n");

    assert_eq!(
        findings,
        ["tools/measure.sh: a script, which belongs in a Rust binary"]
    );
}

#[test]
fn file_findings_reports_rust_source_that_is_not_utf8() {
    // Goal: Rust source that is not UTF-8 is reported, since no text rule can read it.
    // Method: a byte that never starts a UTF-8 character.
    let findings = file_findings("collector/src/main.rs", b"\xff\n");

    assert_eq!(findings.len(), 1);
    assert!(
        findings[0].starts_with("collector/src/main.rs: not UTF-8"),
        "{}",
        findings[0]
    );
}

#[test]
fn file_findings_reports_a_toolchain_file() {
    // Goal: a file that pins the toolchain is reported wherever it sits, since rustup
    // installs that toolchain on the machine of anyone who runs cargo beside it.
    // Method: both names rustup reads, at the root and in a package.
    let reason = "a toolchain file, which makes rustup install a toolchain on every clone";

    let root = file_findings("rust-toolchain.toml", b"[toolchain]\n");
    let package = file_findings("collector/rust-toolchain", b"1.95.0\n");

    assert_eq!(root, [format!("rust-toolchain.toml: {reason}")]);
    assert_eq!(package, [format!("collector/rust-toolchain: {reason}")]);
}

/// Builds a function one line past `FUNCTION_LINES_MAX`, then a comment past `COLUMNS_MAX`
/// on line 72 and a discard on line 73.
fn file_findings_text() -> String {
    function_text("", FUNCTION_LINES_MAX + 1)
        + "// "
        + &"x".repeat(COLUMNS_MAX)
        + "\n"
        + "let _ = read();\n"
}

/// Lists the number of each line, counted from 1, that runs past `COLUMNS_MAX` columns.
fn long_lines(text: &str) -> Vec<usize> {
    let mut numbers = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.chars().count() > COLUMNS_MAX {
            numbers.push(index + 1);
        }
    }
    numbers
}

#[test]
fn long_lines_reports_a_line_past_the_limit() {
    // Goal: a line of 101 columns is reported by its line number, and one of 100 is not.
    // Method: two lines, the second one column past the limit.
    let text = format!(
        "{}\n{}\n",
        "x".repeat(COLUMNS_MAX),
        "x".repeat(COLUMNS_MAX + 1)
    );

    let numbers = long_lines(&text);

    assert_eq!(numbers, [2]);
}

#[test]
fn long_lines_counts_columns_and_not_bytes() {
    // Goal: a line of 100 two-byte characters is within the limit.
    // Method: one line of 100 'é', which is 200 bytes.
    let text = "é".repeat(COLUMNS_MAX);

    let numbers = long_lines(&text);

    assert!(numbers.is_empty(), "{numbers:?}");
}

/// Lists the number of the `fn` line, counted from 1, of each function that runs past
/// `FUNCTION_LINES_MAX` lines, counted from its `fn` line to its closing brace.
fn long_functions(text: &str) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let mut numbers = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if long_functions_opens(line) {
            if long_functions_count(&lines, index) > FUNCTION_LINES_MAX {
                numbers.push(index + 1);
            }
        }
    }
    numbers
}

/// Tells whether `line` opens a function, with nothing before `fn` but its visibility and
/// its qualifiers. A restricted visibility can hold a space, as `pub(in crate::tracker)`
/// does, so it is dropped whole before the line is split into words.
fn long_functions_opens(line: &str) -> bool {
    let mut rest = line.trim_start();
    if rest.starts_with("pub(") {
        match rest.find(')') {
            Some(index) => rest = &rest[index + 1..],
            None => return false,
        }
    }

    for word in rest.split_whitespace() {
        match word {
            "fn" => return true,
            "pub" | "const" | "async" | "unsafe" | "safe" | "extern" => {}
            abi if abi.starts_with('"') => {}
            _ => return false,
        }
    }
    false
}

/// Counts the lines of the function whose `fn` line is at `start`, up to its closing brace.
/// Rustfmt ends a signature with `{` before a body, and closes the body with a brace alone on
/// a line at the `fn` line's indentation. A declaration ends its signature with `;` and an
/// empty body with `{}`, so neither has lines past its signature.
fn long_functions_count(lines: &[&str], start: usize) -> usize {
    assert!(start < lines.len());

    let rest = &lines[start..];
    let signature_count = long_functions_count_signature(rest);
    if rest[signature_count - 1].ends_with('{') {
        let line = rest[0];
        let indentation = &line[..line.len() - line.trim_start().len()];
        long_functions_count_body(rest, indentation)
    } else {
        signature_count
    }
}

/// Counts the lines up to and including the one that ends the signature at the start of
/// `lines`, or all of them when none does.
fn long_functions_count_signature(lines: &[&str]) -> usize {
    assert!(!lines.is_empty());

    for (index, line) in lines.iter().enumerate() {
        if matches!(line.chars().last(), Some('{' | '}' | ';')) {
            return index + 1;
        }
    }
    lines.len()
}

/// Counts the lines up to and including the first that holds only a closing brace at
/// `indentation`, or all of them when none does, since the function then runs to the end.
fn long_functions_count_body(lines: &[&str], indentation: &str) -> usize {
    assert!(!lines.is_empty());

    for (index, line) in lines.iter().enumerate() {
        if line.strip_prefix(indentation) == Some("}") {
            return index + 1;
        }
    }
    lines.len()
}

#[test]
fn long_functions_reports_a_function_past_the_limit() {
    // Goal: a function of 71 lines is reported by its `fn` line, and one of 70 is not.
    // Method: two functions one after the other, the second one line past the limit.
    let text = function_text("", FUNCTION_LINES_MAX) + &function_text("", FUNCTION_LINES_MAX + 1);

    let numbers = long_functions(&text);

    assert_eq!(numbers, [FUNCTION_LINES_MAX + 1]);
}

#[test]
fn long_functions_counts_a_signature_over_several_lines() {
    // Goal: the count starts at the `fn` line, so a signature that rustfmt wraps counts too.
    // Method: a function of 71 lines whose signature and where clause take its first six.
    let text = "fn step(\n    count: u64,\n) -> u64\nwhere\n    u64: Copy,\n{\n".to_owned()
        + &"    step_once();\n".repeat(FUNCTION_LINES_MAX - 6)
        + "}\n";

    let numbers = long_functions(&text);

    assert_eq!(numbers, [1]);
}

#[test]
fn long_functions_skips_a_declaration_and_an_empty_body() {
    // Goal: a declaration with no body and an empty body on its `fn` line end there, and do
    // not run on into the function after them.
    // Method: a declaration and an empty function, each followed by a function at the limit.
    let text = "trait Clock {\n    fn now(&self) -> u64;\n".to_owned()
        + &function_text("    ", FUNCTION_LINES_MAX)
        + "}\n\nfn idle() {}\n"
        + &function_text("", FUNCTION_LINES_MAX);

    let numbers = long_functions(&text);

    assert!(numbers.is_empty(), "{numbers:?}");
}

#[test]
fn long_functions_ends_an_indented_function_at_its_own_closing_brace() {
    // Goal: a function inside a module ends at the brace at its own indentation, and not at
    // the module's.
    // Method: two indented functions in a module, at the limit and one line past it.
    let text = "mod tests {\n".to_owned()
        + &function_text("    ", FUNCTION_LINES_MAX)
        + &function_text("    ", FUNCTION_LINES_MAX + 1)
        + "}\n";

    let numbers = long_functions(&text);

    assert_eq!(numbers, [FUNCTION_LINES_MAX + 2]);
}

#[test]
fn long_functions_counts_a_function_behind_its_visibility_and_qualifiers() {
    // Goal: a function is measured whatever visibility and qualifiers stand before its `fn`.
    // Method: a function one line past the limit behind each prefix rustfmt can write.
    let prefixes = [
        "pub ",
        "pub(crate) ",
        "pub(super) ",
        "pub(self) ",
        "pub(in crate::tracker) ",
        "const ",
        "async ",
        "unsafe ",
        "safe ",
        "extern \"C\" ",
        "pub(crate) const unsafe extern \"C\" ",
    ];

    for prefix in prefixes {
        let text = function_text("", FUNCTION_LINES_MAX + 1).replacen(
            "fn ",
            &(prefix.to_owned() + "fn "),
            1,
        );

        let numbers = long_functions(&text);

        assert_eq!(numbers, [1], "{prefix}");
    }
}

/// Builds a function of `line_count` lines at `indentation`, counted from its `fn` line to
/// its closing brace, whose body opens with a block that closes at a deeper indentation.
fn function_text(indentation: &str, line_count: usize) -> String {
    assert!(line_count >= 5);

    let block = format!(
        "{indentation}    if ready {{\n{indentation}        step_once();\n{indentation}    }}\n"
    );
    let steps = format!("{indentation}    step_once();\n").repeat(line_count - 5);
    format!("{indentation}fn step() {{\n{block}{steps}{indentation}}}\n")
}

/// Tells whether the file pins the toolchain, which rustup by default installs for anyone
/// who runs cargo beside it.
fn toolchain_file(path: &str) -> bool {
    assert!(!path.is_empty());

    let name = Path::new(path).file_name().and_then(OsStr::to_str);
    matches!(name, Some("rust-toolchain" | "rust-toolchain.toml"))
}

/// Tells whether the file is a script, by its name or by a shebang on its first line.
fn script(path: &str, bytes: &[u8]) -> bool {
    assert!(!path.is_empty());

    let extension = Path::new(path)
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase);
    if matches!(
        extension.as_deref(),
        Some("sh" | "bash" | "zsh" | "py" | "pl" | "rb" | "js" | "ts" | "c" | "m")
    ) {
        true
    } else {
        // Rust opens an inner attribute with `#![`, which is not a shebang.
        if bytes.starts_with(b"#!") {
            !bytes.starts_with(b"#![")
        } else {
            false
        }
    }
}

#[test]
fn script_reports_a_shebang_and_each_script_extension() {
    // Goal: a file is a script by its first line or by its name, whatever else it holds.
    // Method: a shebang in a file with no extension, then each extension on an empty file.
    let extensions = ["sh", "bash", "zsh", "py", "pl", "rb", "js", "ts", "c", "m"];

    assert!(script(
        "tools/measure",
        b"#!/usr/bin/env python3\nprint(1)\n"
    ));
    for extension in extensions {
        assert!(
            script(&format!("tools/measure.{extension}"), b"x"),
            "{extension}"
        );
    }
}

#[test]
fn script_reports_an_extension_in_capitals() {
    // Goal: a script's extension counts whatever its case.
    // Method: one extension in capitals and one in mixed case.
    assert!(script("tools/measure.SH", b"x"));
    assert!(script("tools/measure.Py", b"x"));
}

#[test]
fn script_passes_rust_configuration_and_documents() {
    // Goal: Rust, its configuration and the documents are not scripts, and neither is a file
    // that opens with an inner attribute, which starts with `#!` as a shebang does.
    // Method: a file of each kind, with a shebang later in a document.
    assert!(!script("tidy/src/lib.rs", b"#![cfg(test)]\n"));
    assert!(!script("collector/src/main.rs", b"fn main() {}\n"));
    assert!(!script("Cargo.toml", b"[workspace]\n"));
    assert!(!script(".github/workflows/check.yml", b"name: check\n"));
    assert!(!script("README.md", b"# Flightdeck\n\n#!/bin/sh\n"));
}

/// Lists the number of each line, counted from 1, that assigns a value to `_`.
fn discards(text: &str) -> Vec<usize> {
    let mut numbers = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if discards_holds(line) {
            numbers.push(index + 1);
        }
    }
    numbers
}

/// Tells whether `line` assigns a value to `_`, in a statement, in a binding with or without
/// a type, or after a match arm's arrow or a closure's parameters.
fn discards_holds(line: &str) -> bool {
    let statement = line.trim_start();
    let binding = statement.strip_prefix("let ").unwrap_or(statement);
    if binding.starts_with("_:") {
        return true;
    }
    if discards_assigns(binding) {
        return true;
    }

    for separator in ["=> ", "| "] {
        for (position, _) in line.match_indices(separator) {
            if discards_assigns(&line[position + separator.len()..]) {
                return true;
            }
        }
    }
    false
}

/// Tells whether `text` starts by assigning to `_`. Rustfmt writes `_ =` with nothing after
/// it where it wraps the value onto the next line, and a match arm on `_` writes `_ =>`.
fn discards_assigns(text: &str) -> bool {
    match text.strip_prefix("_ =") {
        Some(rest) => !rest.starts_with(['=', '>']),
        None => false,
    }
}

#[test]
fn discards_reports_a_value_discarded_by_assignment() {
    // Goal: both statements that discard a value are reported by their line numbers.
    // Method: one of each in a function.
    let text = "fn step() {\n    let _ = read();\n    _ = write();\n}\n";

    let numbers = discards(text);

    assert_eq!(numbers, [2, 3]);
}

#[test]
fn discards_reports_a_discard_in_an_arm_a_closure_a_type_or_a_wrapped_line() {
    // Goal: a value assigned to `_` is reported wherever rustfmt can put the assignment.
    // Method: a match arm, a closure, a typed binding, and two discards rustfmt wrapped after
    // their `=`. The escaped spaces keep this test's own source free of a discard.
    let text = "fn step() {\n".to_owned()
        + "    match read() {\n"
        + "        Some(count) => _\x20= write(count),\n"
        + "        None => {}\n"
        + "    }\n"
        + "    each(|count| _\x20= write(count));\n"
        + "    let _: u64 = read();\n"
        + "    let _ =\n"
        + "        read_every_source_of_the_machine();\n"
        + "    _ =\n"
        + "        write_every_source_of_the_machine();\n"
        + "}\n";

    let numbers = discards(&text);

    assert_eq!(numbers, [3, 6, 7, 8, 10]);
}

#[test]
fn discards_passes_a_match_arm_a_closure_and_a_named_binding() {
    // Goal: a match arm on `_`, a closure that ignores its parameter and a binding with a
    // name are not discards.
    // Method: a function holding one of each, the arm after an or-pattern.
    let text = "fn step() {\n    match read() {\n        Some(_) | _ => {}\n    }\n".to_owned()
        + "    each(|_| write());\n    let total: u64 = 1;\n}\n";

    let numbers = discards(&text);

    assert!(numbers.is_empty(), "{numbers:?}");
}
