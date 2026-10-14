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
const ASSERTIONS_PER_FUNCTION_MIN: usize = 2;

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
        if let Some(bytes) = read_tracked(&root, path) {
            findings.extend(file_findings(path, &bytes));
        }
    }

    assert!(findings.is_empty(), "\n{}\n", findings.join("\n"));
}

#[test]
fn release_profile_traps_overflow_and_aborts_on_a_panic() {
    // Goal: the release build keeps the checks Tiger Style keeps in production.
    // Method: read the `[profile.release]` table of the workspace manifest.
    let text = read_text(&repository_root().join("Cargo.toml"));

    let settings = release_profile_settings(&text);

    assert!(settings.contains(&"overflow-checks = true"), "{settings:?}");
    assert!(settings.contains(&"panic = \"abort\""), "{settings:?}");
}

#[test]
fn dependencies_from_outside_the_workspace_have_a_recorded_case() {
    // Goal: the workspace depends on a crate from outside it only with a case recorded under
    // the dependency decision in docs/recording-research.md.
    // Method: read the workspace's packages from the members the workspace manifest lists, the
    // direct dependencies from Cargo.lock, and the cases from the research doc's section of
    // them.
    let root = repository_root();
    let lockfile = read_text(&root.join("Cargo.lock"));
    let research = read_text(&root.join("docs/recording-research.md"));
    let workspace = read_text(&root.join("Cargo.toml"));
    let manifests: Vec<String> = workspace_members(&workspace)
        .into_iter()
        .map(|member| read_text(&root.join(member).join("Cargo.toml")))
        .collect();
    let members: Vec<&str> = manifests
        .iter()
        .map(|manifest| package_name(manifest))
        .collect();
    assert!(members.contains(&"tidy"), "{members:?}");

    let findings = dependency_findings(&Dependencies {
        lockfile: &lockfile,
        members: &members,
        research: &research,
    });

    assert!(findings.is_empty(), "\n{}\n", findings.join("\n"));
}

#[test]
fn functions_average_two_assertions() {
    // Goal: the functions outside tests make two runtime assertions each on average, as
    // docs/tiger-style.md asks.
    // Method: count the functions with a body and the assertions in them over every Rust file
    // git tracks, and compare the assertions with twice the functions.
    let root = repository_root();
    let mut total = AssertionCounts::default();
    for path in repository_paths(&root) {
        if Path::new(&path).extension() == Some(OsStr::new("rs")) {
            if let Some(bytes) = read_tracked(&root, &path) {
                match str::from_utf8(&bytes) {
                    Ok(text) => total.add(assertion_counts(text)),
                    Err(error) => panic!("cannot read {path}: {error}"),
                }
            }
        }
    }

    assert!(total.functions > 0);
    assert!(
        total.assertions >= ASSERTIONS_PER_FUNCTION_MIN * total.functions,
        "{} runtime assertions in {} functions, under {ASSERTIONS_PER_FUNCTION_MIN} a function",
        total.assertions,
        total.functions
    );
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

/// Reads a file git tracks, or nothing when the working tree has deleted it, since a deleted
/// file stays in the index until the deletion is staged.
fn read_tracked(root: &Path, path: &str) -> Option<Vec<u8>> {
    match fs::read(root.join(path)) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => panic!("cannot read {path}: {error}"),
    }
}

fn read_text(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => panic!("cannot read {}: {error}", path.display()),
    }
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
        if function_opens(line) {
            if item_count(&lines[index..]) > FUNCTION_LINES_MAX {
                numbers.push(index + 1);
            }
        }
    }
    numbers
}

/// Tells whether `line` opens a function, with nothing before `fn` but its visibility and
/// its qualifiers. A restricted visibility can hold a space, as `pub(in crate::tracker)`
/// does, so it is dropped whole before the line is split into words.
fn function_opens(line: &str) -> bool {
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

/// Counts the lines of the item at the start of `lines`, a function or a `const` item or
/// block, up to its closing brace. Rustfmt ends a signature with `{` before a body, and
/// closes the body with a brace alone on a line at the item's indentation, followed by `;`
/// where the item is an expression. A declaration ends its signature with `;` and an empty
/// body with `{}`, so neither has lines past its signature. An item whose body no brace
/// closes runs to the end of `lines`.
fn item_count(lines: &[&str]) -> usize {
    assert!(!lines.is_empty());

    let signature_count = signature_count(lines);
    if !lines[signature_count - 1].ends_with('{') {
        return signature_count;
    }

    let first = lines[0];
    let indentation = &first[..first.len() - first.trim_start().len()];
    for (index, line) in lines.iter().enumerate() {
        if matches!(line.strip_prefix(indentation), Some("}" | "};")) {
            return index + 1;
        }
    }
    lines.len()
}

/// Counts the lines up to and including the one that ends the signature at the start of
/// `lines`, or all of them when none does.
fn signature_count(lines: &[&str]) -> usize {
    assert!(!lines.is_empty());

    for (index, line) in lines.iter().enumerate() {
        if matches!(line.chars().last(), Some('{' | '}' | ';')) {
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

/// The functions with a body that the assertion rule counts, and the runtime assertions they
/// make.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct AssertionCounts {
    functions: usize,
    assertions: usize,
}

impl AssertionCounts {
    fn add(&mut self, other: AssertionCounts) {
        self.functions += other.functions;
        self.assertions += other.assertions;
    }
}

/// Counts the functions with a body in `text`, the source of one Rust file, and the runtime
/// assertions they make. It leaves out a file compiled only for tests, every item under
/// `#[cfg(test)]`, a function declared with no body, and the assertions of a `const` block,
/// which the compiler runs. An assertion outside a function, as in a `const` item, counts
/// for none.
fn assertion_counts(text: &str) -> AssertionCounts {
    let lines: Vec<&str> = text.lines().collect();
    let mut counts = AssertionCounts::default();
    if lines.iter().any(|line| line.trim() == "#![cfg(test)]") {
        return counts;
    }

    // The indentation of the outermost function open at the line, whose body closes with a
    // brace alone on a line at that indentation.
    let mut open: Option<&str> = None;
    let mut index = 0;
    while index < lines.len() {
        let left_out = assertion_counts_left_out(&lines[index..]);
        if left_out > 0 {
            index += left_out;
        } else {
            let line = lines[index];
            if function_opens(line) {
                let signature_end = lines[index + signature_count(&lines[index..]) - 1];
                if !signature_end.ends_with(';') {
                    counts.functions += 1;
                    if signature_end.ends_with('{') {
                        if open.is_none() {
                            open = Some(&line[..line.len() - line.trim_start().len()]);
                        }
                    }
                }
            }
            if let Some(indentation) = open {
                counts.assertions += assertion_counts_line(line);
                if line.strip_prefix(indentation) == Some("}") {
                    open = None;
                }
            }
            index += 1;
        }
    }
    counts
}

/// Counts the lines of the item at the start of `lines` that the assertion rule leaves out:
/// an item under `#[cfg(test)]`, with the attributes before it, and a `const` block over
/// several lines. Returns 0 where the first line starts no such item.
fn assertion_counts_left_out(lines: &[&str]) -> usize {
    assert!(!lines.is_empty());

    let first = lines[0].trim_start();
    if first == "#[cfg(test)]" {
        let attribute_count = lines
            .iter()
            .take_while(|line| line.trim_start().starts_with("#["))
            .count();
        assert!(attribute_count > 0);
        if attribute_count == lines.len() {
            return attribute_count;
        }
        return attribute_count + item_count(&lines[attribute_count..]);
    }
    if first.ends_with("const {") {
        return item_count(lines);
    }
    0
}

/// Counts the runtime assertions `line` makes: each `assert!`, `assert_eq!` and `assert_ne!`
/// before any comment, outside a `const` block, and never `debug_assert!`, which a release
/// build leaves out.
fn assertion_counts_line(line: &str) -> usize {
    let code = match line.find("//") {
        Some(position) => &line[..position],
        None => line,
    };
    if code.contains("const {") {
        return 0;
    }

    let mut count = 0;
    for macro_call in ["assert!(", "assert_eq!(", "assert_ne!("] {
        for (position, _) in code.match_indices(macro_call) {
            let before = code[..position].chars().next_back();
            if !before.is_some_and(
                |character| matches!(character, '_' | 'a'..='z' | 'A'..='Z' | '0'..='9'),
            ) {
                count += 1;
            }
        }
    }
    assert!(count <= code.len());
    count
}

#[test]
fn assertion_counts_counts_each_function_with_a_body_and_its_assertions() {
    // Goal: each function with a body counts once, with each runtime assertion it makes.
    // Method: a function and a method in an impl, making three assertions between them.
    let text = "fn step(count: u64) -> u64 {\n    assert!(count > 0);\n    count\n}\n\n".to_owned()
        + "impl Tally {\n    fn add(&mut self) {\n        assert_eq!(0, self.ticks);\n"
        + "        assert_ne!(1, self.slips);\n    }\n}\n";

    let counts = assertion_counts(&text);

    assert_eq!(
        AssertionCounts {
            functions: 2,
            assertions: 3
        },
        counts
    );
}

#[test]
fn assertion_counts_leaves_out_every_item_under_cfg_test() {
    // Goal: an item under `#[cfg(test)]` counts no function and no assertion.
    // Method: a function, then a helper whose `#[cfg(test)]` comes before a second attribute,
    // then a test module.
    let text = "fn step() {\n    assert!(ready());\n}\n\n#[cfg(test)]\n#[must_use]\n".to_owned()
        + "fn helper() -> u64 {\n    assert!(ready());\n    7\n}\n\n#[cfg(test)]\n"
        + "mod tests {\n    #[test]\n    fn steps() {\n        assert!(ready());\n    }\n}\n";

    let counts = assertion_counts(&text);

    assert_eq!(
        AssertionCounts {
            functions: 1,
            assertions: 1
        },
        counts
    );
}

#[test]
fn assertion_counts_leaves_out_a_file_compiled_only_for_tests() {
    // Goal: a file that `#![cfg(test)]` compiles only for tests counts nothing.
    // Method: such a file holding a function that makes an assertion.
    let text = "#![cfg(test)]\n\nfn helper() {\n    assert!(ready());\n}\n";

    let counts = assertion_counts(text);

    assert_eq!(AssertionCounts::default(), counts);
}

#[test]
fn assertion_counts_leaves_out_a_function_declared_with_no_body() {
    // Goal: a function declared in an extern block or a trait, with no body, is not counted.
    // Method: an extern block behind a link attribute, one declaration over several lines,
    // and a trait method with no default body.
    let text = "#[link(name = \"IOKit\", kind = \"framework\")]\nunsafe extern \"C\" {\n"
        .to_owned()
        + "    fn IOObjectRelease(object: u32) -> i32;\n    fn sysctl(\n        name: *mut i32,\n"
        + "    ) -> i32;\n}\n\ntrait Read {\n    fn read(&self) -> u64;\n}\n";

    let counts = assertion_counts(&text);

    assert_eq!(AssertionCounts::default(), counts);
}

#[test]
fn assertion_counts_leaves_out_assertions_the_compiler_runs() {
    // Goal: an assertion in a `const` item or block runs at compile time, so it is not one of
    // a function's runtime assertions.
    // Method: a one-line and a multi-line const item, then a function making one runtime
    // assertion beside a one-line and a multi-line const block.
    let text = "const _: () = assert!(SIZE > 0);\n\nconst _: () = {\n    assert!(SIZE < 9);\n};\n\n"
        .to_owned()
        + "fn step() {\n    const { assert!(SIZE > 1) };\n    const {\n        assert!(SIZE > 2);\n"
        + "    };\n    assert!(ready());\n}\n";

    let counts = assertion_counts(&text);

    assert_eq!(
        AssertionCounts {
            functions: 1,
            assertions: 1
        },
        counts
    );
}

#[test]
fn assertion_counts_leaves_out_comments_and_debug_assertions() {
    // Goal: an assertion in a comment makes none, and `debug_assert!` makes no runtime
    // assertion, since a release build leaves it out.
    // Method: a function making one assertion, after a commented one and a debug assertion,
    // with another commented at the end of its line.
    let text = "fn step() {\n    // assert!(ready());\n    debug_assert!(ready());\n".to_owned()
        + "    assert!(ready()); // assert!(done())\n}\n";

    let counts = assertion_counts(&text);

    assert_eq!(
        AssertionCounts {
            functions: 1,
            assertions: 1
        },
        counts
    );
}

#[test]
fn assertion_counts_ends_an_empty_body_on_its_own_line() {
    // Goal: a function with an empty body counts with no assertion, and ends on its own line,
    // so the lines after it are not taken for its body.
    // Method: a function whose body is `{}`, a const item's assertion outside any function,
    // then a function making an assertion.
    let text = "fn idle() {}\n\nconst _: () = assert!(SIZE > 0);\n\n".to_owned()
        + "fn step() {\n    assert!(ready());\n}\n";

    let counts = assertion_counts(&text);

    assert_eq!(
        AssertionCounts {
            functions: 2,
            assertions: 1
        },
        counts
    );
}

/// Opens the research doc's section of recorded cases, one for each crate from outside the
/// workspace that a workspace package depends on.
const CASES_HEADING: &str = "## Crates from outside the workspace";

/// Checks a lockfile's direct dependencies from outside the workspace against the cases the
/// research doc records, and returns its findings.
fn dependency_findings(dependencies: &Dependencies<'_>) -> Vec<String> {
    let Dependencies {
        lockfile,
        members,
        research,
    } = *dependencies;
    let cases = recorded_cases(research);
    let external = external_dependencies(lockfile, members);

    let mut findings = Vec::new();
    if !research.lines().any(|line| line == CASES_HEADING) {
        findings.push(format!(
            "docs/recording-research.md: no `{CASES_HEADING}` section"
        ));
    }
    findings.extend(dependency_findings_entries(&cases));
    findings.extend(dependency_findings_uncased(&external, &cases));
    findings.extend(dependency_findings_unused(&cases, &external));
    findings
}

/// What the dependency rule reads: the lockfile, the names of the workspace's own packages,
/// and the research doc.
#[derive(Clone, Copy)]
struct Dependencies<'a> {
    lockfile: &'a str,
    members: &'a [&'a str],
    research: &'a str,
}

/// Reports each entry a case leaves empty.
fn dependency_findings_entries(cases: &[Case<'_>]) -> Vec<String> {
    let mut findings = Vec::new();
    for case in cases {
        for label in CASE_ENTRIES {
            if !case.labels_filled.contains(&label) {
                findings.push(format!(
                    "docs/recording-research.md: the case for {} says nothing under `{label}`",
                    case.name
                ));
            }
        }
    }
    assert!(findings.len() <= cases.len() * CASE_ENTRIES.len());
    findings
}

/// Reports each crate from outside the workspace that a member depends on with no case.
fn dependency_findings_uncased(external: &[&str], cases: &[Case<'_>]) -> Vec<String> {
    let mut findings = Vec::new();
    for name in external {
        if !cases.iter().any(|case| case.name == *name) {
            findings.push(format!(
                "Cargo.lock: {name} comes from outside the workspace with no case in the \
                 research doc"
            ));
        }
    }
    assert!(findings.len() <= external.len());
    findings
}

/// Reports each case whose crate no member depends on, as a case left after its crate went,
/// or under a heading that misspells its crate, would be.
fn dependency_findings_unused(cases: &[Case<'_>], external: &[&str]) -> Vec<String> {
    let mut findings = Vec::new();
    for case in cases {
        if !external.contains(&case.name) {
            findings.push(format!(
                "docs/recording-research.md: the case for {} names no crate from outside the \
                 workspace that a member depends on",
                case.name
            ));
        }
    }
    assert!(findings.len() <= cases.len());
    findings
}

/// The entries a case needs, each a line that starts with its label and goes on, on that
/// line, to say something: how much of the crate Flightdeck uses, whether code written here
/// would match its performance, and what that code would cost to maintain.
const CASE_ENTRIES: [&str; 3] = ["- Used:", "- Performance:", "- Maintenance:"];

/// A crate's case in the research doc, with the labels of the entries it fills.
struct Case<'a> {
    name: &'a str,
    labels_filled: Vec<&'static str>,
}

/// Reads the cases in the research doc's section of them, one under each `###` heading, whose
/// text is the crate's name, in backticks or not.
fn recorded_cases(research: &str) -> Vec<Case<'_>> {
    let mut cases: Vec<Case<'_>> = Vec::new();
    let mut inside = false;
    for line in research.lines() {
        if line.starts_with("## ") {
            inside = line == CASES_HEADING;
        } else {
            if inside {
                if let Some(name) = line.strip_prefix("### ") {
                    cases.push(Case {
                        name: name.trim_matches('`'),
                        labels_filled: Vec::new(),
                    });
                } else {
                    if let Some(case) = cases.last_mut() {
                        case.labels_filled.extend(recorded_cases_label(line));
                    }
                }
            }
        }
    }
    cases
}

/// Returns the label of the entry a line fills, if it starts with one and says something
/// after it.
fn recorded_cases_label(line: &str) -> Option<&'static str> {
    let label = CASE_ENTRIES.into_iter().find(|label| {
        line.strip_prefix(label)
            .is_some_and(|text| !text.trim().is_empty())
    });
    if let Some(label) = label {
        assert!(line.starts_with(label));
    }
    label
}

/// A package in a lockfile, and whether the lockfile gives its `source`. Cargo writes a
/// `source` for a package from a registry or a git repository, and none for a package it reads
/// from a path, inside the workspace or not.
struct Package<'a> {
    name: &'a str,
    sourced: bool,
    dependencies: Vec<&'a str>,
}

/// Lists the crates from outside the workspace that a package in it names as a dependency,
/// each once, in the order the lockfile first names them. A crate is from outside unless it is
/// one of the workspace's `members` and has no `source`.
fn external_dependencies<'a>(lockfile: &'a str, members: &[&str]) -> Vec<&'a str> {
    let packages = external_dependencies_packages(lockfile);
    let external = |package: &Package<'_>| package.sourced || !members.contains(&package.name);

    let mut names = Vec::new();
    for package in packages.iter().filter(|package| !external(package)) {
        for &name in &package.dependencies {
            if packages
                .iter()
                .any(|other| other.name == name && external(other))
            {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    for (index, name) in names.iter().enumerate() {
        assert!(!names[..index].contains(name));
    }
    names
}

/// Reads the packages of a lockfile in the form Cargo writes, where each `[[package]]`
/// table holds one key a line and its `dependencies` one name a line, with a version after
/// the name where two packages share it.
fn external_dependencies_packages(lockfile: &str) -> Vec<Package<'_>> {
    let mut packages: Vec<Package<'_>> = Vec::new();
    let mut inside_package = false;
    let mut inside_dependencies = false;
    for line in lockfile.lines() {
        if line.starts_with('[') {
            inside_package = line == "[[package]]";
            inside_dependencies = false;
            if inside_package {
                packages.push(Package {
                    name: "",
                    sourced: false,
                    dependencies: Vec::new(),
                });
            }
        } else {
            if inside_package {
                let Some(package) = packages.last_mut() else {
                    unreachable!("inside a package before reading one");
                };
                if inside_dependencies {
                    if line == "]" {
                        inside_dependencies = false;
                    } else {
                        let entry = line.trim().trim_end_matches(',').trim_matches('"');
                        let name = entry.split_once(' ').map_or(entry, |(name, _)| name);
                        package.dependencies.push(name);
                    }
                } else {
                    if let Some(name) = line.strip_prefix("name = ") {
                        package.name = name.trim_matches('"');
                    }
                    if line.starts_with("source = ") {
                        package.sourced = true;
                    }
                    inside_dependencies = line == "dependencies = [";
                }
            }
        }
    }
    packages
}

#[test]
fn dependency_findings_reports_each_direct_dependency_from_outside_with_no_case() {
    // Goal: a crate from outside the workspace that a workspace package depends on needs a
    // recorded case, reported once however many members depend on it, and a crate only
    // another outside crate depends on needs none.
    // Method: a lockfile where both members depend on a registry crate, one of them on the
    // other member too, and the registry crate on a third crate, against no cases.
    let research = research_text("");

    let findings = dependency_findings(&Dependencies {
        lockfile: LOCKFILE,
        members: &MEMBERS,
        research: &research,
    });

    assert_eq!(
        findings,
        ["Cargo.lock: libc comes from outside the workspace with no case in the research doc"]
    );
}

#[test]
fn dependency_findings_passes_a_dependency_with_a_full_case() {
    // Goal: a case that names how much of the crate Flightdeck uses, whether code written here
    // would match its performance, and what that code would cost to maintain admits it.
    // Method: the lockfile above against a case for `libc` with all three entries.
    let research = research_text(LIBC_CASE);

    let findings = dependency_findings(&Dependencies {
        lockfile: LOCKFILE,
        members: &MEMBERS,
        research: &research,
    });

    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn dependency_findings_reports_a_case_missing_an_entry() {
    // Goal: a case admits its crate only when it says all three things a case weighs.
    // Method: the case above with its first entry left empty, its second removed, and its
    // third saying only spaces.
    let case = LIBC_CASE
        .replace(
            "- Used: the kernel calls and the structs they fill.",
            "- Used:",
        )
        .replace("- Performance: the same calls, made the same way.\n", "")
        .replace(
            "- Maintenance: a struct to pin each time the SDK changes one.",
            "- Maintenance:   ",
        );
    let research = research_text(&case);

    let findings = dependency_findings(&Dependencies {
        lockfile: LOCKFILE,
        members: &MEMBERS,
        research: &research,
    });

    assert_eq!(
        findings,
        [
            "docs/recording-research.md: the case for libc says nothing under `- Used:`",
            "docs/recording-research.md: the case for libc says nothing under `- Performance:`",
            "docs/recording-research.md: the case for libc says nothing under `- Maintenance:`",
        ]
    );
}

#[test]
fn dependency_findings_reports_a_research_doc_with_no_section_of_cases() {
    // Goal: the rule reports a research doc that has lost its section of cases, which would
    // otherwise read as recording none.
    // Method: a full case for `libc` under a section of another name.
    let research = research_text(LIBC_CASE).replace(CASES_HEADING, "## Crates");

    let findings = dependency_findings(&Dependencies {
        lockfile: LOCKFILE,
        members: &MEMBERS,
        research: &research,
    });

    assert_eq!(
        findings,
        [
            "docs/recording-research.md: no `## Crates from outside the workspace` section",
            "Cargo.lock: libc comes from outside the workspace with no case in the research doc",
        ]
    );
}

#[test]
fn dependency_findings_reports_a_path_dependency_from_outside_the_workspace() {
    // Goal: a crate read from a path outside the workspace needs a case like a crate from a
    // registry, though Cargo writes no `source` for it.
    // Method: the lockfile above with `collector` also depending on `vendored`, a package
    // with no `source` that is not a member, against no cases.
    let lockfile = LOCKFILE.replace(" \"tidy\",\n]", " \"tidy\",\n \"vendored\",\n]")
        + "\n[[package]]\nname = \"vendored\"\nversion = \"0.1.0\"\n";
    let research = research_text("");

    let findings = dependency_findings(&Dependencies {
        lockfile: &lockfile,
        members: &MEMBERS,
        research: &research,
    });

    assert_eq!(
        findings,
        [
            "Cargo.lock: libc comes from outside the workspace with no case in the research doc",
            "Cargo.lock: vendored comes from outside the workspace with no case in the research \
             doc",
        ]
    );
}

#[test]
fn dependency_findings_reports_a_case_for_a_crate_no_member_depends_on() {
    // Goal: a case stands only for a direct dependency, so a case left after its crate went,
    // or under a heading that misspells the crate, shows.
    // Method: the full case for `libc` beside a full case for `serde`, which no package in the
    // lockfile names.
    let serde = LIBC_CASE.replace("### `libc`", "### serde");
    let research = research_text(&format!("{LIBC_CASE}\n{serde}"));

    let findings = dependency_findings(&Dependencies {
        lockfile: LOCKFILE,
        members: &MEMBERS,
        research: &research,
    });

    assert_eq!(
        findings,
        [
            "docs/recording-research.md: the case for serde names no crate from outside the \
             workspace that a member depends on"
        ]
    );
}

#[test]
fn dependency_findings_reports_a_registry_crate_named_like_a_member() {
    // Goal: a crate from a registry needs a case though it shares its name with a member.
    // Method: the lockfile above with `collector` also depending on a registry crate named
    // `tidy`, which Cargo then names with its version, against a full case for `libc`.
    let lockfile = LOCKFILE.replace(" \"tidy\",\n]", " \"tidy 0.1.0\",\n \"tidy 9.0.0\",\n]")
        + "\n[[package]]\nname = \"tidy\"\nversion = \"9.0.0\"\n"
        + "source = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
    let research = research_text(LIBC_CASE);

    let findings = dependency_findings(&Dependencies {
        lockfile: &lockfile,
        members: &MEMBERS,
        research: &research,
    });

    assert_eq!(
        findings,
        ["Cargo.lock: tidy comes from outside the workspace with no case in the research doc"]
    );
}

#[test]
fn dependency_findings_reads_cases_only_inside_their_section() {
    // Goal: the rule reads a case only in the section of cases, so a heading elsewhere in the
    // doc that names a crate does not admit it.
    // Method: no case in the section of cases, and a full case for `libc` in the section after
    // it.
    let research = research_text("") + "## Sources\n\n" + LIBC_CASE;

    let findings = dependency_findings(&Dependencies {
        lockfile: LOCKFILE,
        members: &MEMBERS,
        research: &research,
    });

    assert_eq!(
        findings,
        ["Cargo.lock: libc comes from outside the workspace with no case in the research doc"]
    );
}

/// The workspace's packages in the lockfile below.
const MEMBERS: [&str; 2] = ["collector", "tidy"];

/// A case for `libc`, its heading in backticks, with every entry a case needs.
const LIBC_CASE: &str = "### `libc`

- Used: the kernel calls and the structs they fill.
- Performance: the same calls, made the same way.
- Maintenance: a struct to pin each time the SDK changes one.
";

/// A lockfile as Cargo writes one, where the workspace package `collector` depends on
/// `libc`, named with its version, and on `tidy`, `tidy` depends on `libc` too, and `libc`
/// depends on `cfg-if`.
const LOCKFILE: &str = r#"version = 4

[[package]]
name = "cfg-if"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "collector"
version = "0.1.0"
dependencies = [
 "libc 0.2.190",
 "tidy",
]

[[package]]
name = "libc"
version = "0.2.190"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "cfg-if",
]

[[package]]
name = "tidy"
version = "0.1.0"
dependencies = [
 "libc 0.2.190",
]
"#;

/// Builds a research doc whose section of cases, its last, holds `cases`.
fn research_text(cases: &str) -> String {
    format!("# Research\n\n{CASES_HEADING}\n\nWhy.\n\n{cases}\n")
}

/// Lists the directories the workspace manifest names as its members, from its `members`
/// array.
fn workspace_members(manifest: &str) -> Vec<&str> {
    let Some((_, after)) = manifest.split_once("members = [") else {
        panic!("the workspace manifest lists no members");
    };
    let Some((list, _)) = after.split_once(']') else {
        panic!("the workspace manifest's members never close");
    };

    let members: Vec<&str> = list
        .split(',')
        .map(|member| member.trim().trim_matches('"'))
        .filter(|member| !member.is_empty())
        .collect();
    assert!(members.iter().all(|member| !member.is_empty()));
    members
}

/// Returns the name a package's manifest gives in its `[package]` table.
fn package_name(manifest: &str) -> &str {
    let mut inside = false;
    for line in manifest.lines() {
        if line.starts_with('[') {
            inside = line == "[package]";
        } else {
            if inside {
                if let Some(name) = line.strip_prefix("name = ") {
                    return name.trim_matches('"');
                }
            }
        }
    }
    panic!("a member's manifest names no package");
}
