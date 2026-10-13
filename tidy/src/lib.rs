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

#[test]
fn repository_is_tidy() {
    // Goal: every file git tracks or would track meets the rules below.
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

/// The files git tracks and the untracked files it does not ignore, relative to `root`.
fn repository_paths(root: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output();
    let output = match output {
        Ok(output) => output,
        Err(error) => panic!("cannot run git ls-files: {error}"),
    };
    assert!(output.status.success(), "git ls-files: {}", output.status);

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
    let extension = Path::new(path).extension().and_then(OsStr::to_str);
    if matches!(extension, Some("rs" | "toml")) {
        match str::from_utf8(bytes) {
            Ok(text) => {
                for number in long_lines(text) {
                    findings.push(format!("{path}:{number}: over {COLUMNS_MAX} columns"));
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
