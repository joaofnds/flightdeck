//! Holds the repository to the rules of `docs/tiger-style.md` that rustfmt and clippy cannot
//! check, as `TigerBeetle`'s `test "tidy"` does. It is only a test, run by `cargo test`.
#![cfg(test)]

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
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

/// Every rule's findings for one file, each starting with the file's path.
fn file_findings(path: &str, bytes: &[u8]) -> Vec<String> {
    let mut findings = Vec::new();
    if script(path, bytes) {
        findings.push(format!("{path}: a script, which belongs in a Rust binary"));
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

/// The number of each line, counted from 1, that runs past `COLUMNS_MAX` columns.
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

    assert!(numbers.is_empty());
}

/// The number of the `fn` line, counted from 1, of each function that runs past
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

/// Whether `line` opens a function, with nothing before `fn` but its visibility and its
/// qualifiers.
fn long_functions_opens(line: &str) -> bool {
    for word in line.split_whitespace() {
        match word {
            "fn" => return true,
            "pub" | "pub(crate)" | "pub(super)" | "pub(self)" | "const" | "async" | "unsafe"
            | "safe" | "extern" => {}
            abi if abi.starts_with('"') => {}
            _ => return false,
        }
    }
    false
}

/// The lines of the function whose `fn` line is at `start`, up to its closing brace. Rustfmt
/// ends a signature with `{` before a body, and closes the body with a brace alone on a line
/// at the `fn` line's indentation. A declaration ends its signature with `;` and an empty
/// body with `{}`, so neither has lines past its signature.
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

/// The lines up to and including the one that ends the signature at the start of `lines`,
/// or all of them when none does.
fn long_functions_count_signature(lines: &[&str]) -> usize {
    assert!(!lines.is_empty());

    for (index, line) in lines.iter().enumerate() {
        if matches!(line.chars().last(), Some('{' | '}' | ';')) {
            return index + 1;
        }
    }
    lines.len()
}

/// The lines up to and including the first that holds only a closing brace at
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

    assert!(numbers.is_empty());
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

/// A function of `line_count` lines at `indentation`, counted from its `fn` line to its
/// closing brace, whose body opens with a block that closes at a deeper indentation.
fn function_text(indentation: &str, line_count: usize) -> String {
    assert!(line_count >= 5);

    let block = format!(
        "{indentation}    if ready {{\n{indentation}        step_once();\n{indentation}    }}\n"
    );
    let steps = format!("{indentation}    step_once();\n").repeat(line_count - 5);
    format!("{indentation}fn step() {{\n{block}{steps}{indentation}}}\n")
}

/// Whether the file is a script, by its name or by a shebang on its first line.
fn script(path: &str, bytes: &[u8]) -> bool {
    assert!(!path.is_empty());

    let extension = Path::new(path).extension().and_then(OsStr::to_str);
    if matches!(
        extension,
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

/// The number of each line, counted from 1, that discards a value with `let _ =` or `_ =`.
/// Rustfmt puts one space on each side of `=`, which a match arm on `_` follows with `>`.
fn discards(text: &str) -> Vec<usize> {
    let mut numbers = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let statement = line.trim_start();
        if statement.starts_with("let _ = ") {
            numbers.push(index + 1);
        } else {
            if statement.starts_with("_ = ") {
                numbers.push(index + 1);
            }
        }
    }
    numbers
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
fn discards_passes_a_match_arm_and_a_named_binding() {
    // Goal: a match arm on `_` and a binding with a name are not discards.
    // Method: a function holding one of each.
    let text = "fn step() {\n    match read() {\n        _ => {}\n    }\n    let total = 1;\n}\n";

    let numbers = discards(text);

    assert!(numbers.is_empty());
}
