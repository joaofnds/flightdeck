//! Fingerprints the worker cohort source and dependency manifests for compatibility.
//! This deterministic guard is not a cryptographic identity of a compiled executable.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Many times the files the collector holds, so a walk that reaches it has gone wrong.
const FILES_MAX: usize = 4096;
/// FNV-1a 64, whose result no Rust release changes, unlike `DefaultHasher`'s.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[cfg(not(test))]
fn main() -> io::Result<()> {
    let build = main_fingerprint(Path::new("."), &mut io::stdout().lock())?;
    let output = std::env::var_os("OUT_DIR").ok_or(io::ErrorKind::NotFound)?;
    fs::write(Path::new(&output).join("build"), build_literal(build))
}

fn main_fingerprint(directory: &Path, output: &mut impl Write) -> io::Result<u64> {
    let mut files = Vec::new();
    for root in [
        "src",
        "../persister/src",
        "../persistence/src",
        "../model/src",
    ] {
        writeln!(output, "cargo::rerun-if-changed={root}")?;
        for path in main_sources(&directory.join(root))? {
            files.push(
                path.strip_prefix(directory)
                    .map_err(io::Error::other)?
                    .to_path_buf(),
            );
        }
        assert!(files.len() <= FILES_MAX);
    }
    for manifest in [
        "Cargo.toml",
        "build.rs",
        "../Cargo.toml",
        "../Cargo.lock",
        "../persister/Cargo.toml",
        "../persistence/Cargo.toml",
        "../model/Cargo.toml",
    ] {
        writeln!(output, "cargo::rerun-if-changed={manifest}")?;
        files.push(PathBuf::from(manifest));
        assert!(files.len() <= FILES_MAX);
    }
    files.sort();

    fingerprint(directory, &files)
}

fn fingerprint(directory: &Path, files: &[PathBuf]) -> io::Result<u64> {
    let mut build = FNV_OFFSET_BASIS;
    for file in files {
        let path = file.as_os_str().as_encoded_bytes();
        let contents = fs::read(directory.join(file))?;
        for bytes in [path, &contents] {
            let length = u64::try_from(bytes.len()).map_err(io::Error::other)?;
            build = main_hash(build, &length.to_le_bytes());
            build = main_hash(build, bytes);
        }
    }

    Ok(build)
}

/// Folds `bytes` into `hash`. Each part is preceded by its length, so moving bytes from one
/// file's path or contents to the next changes the fingerprint.
fn main_hash(hash: u64, bytes: &[u8]) -> u64 {
    let mut hash = hash;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Writes `build` as Rust source, its hexadecimal digits grouped by four as clippy asks.
fn build_literal(build: u64) -> String {
    let digits = format!("{build:016x}");
    format!(
        "0x{}_{}_{}_{}",
        &digits[0..4],
        &digits[4..8],
        &digits[8..12],
        &digits[12..16]
    )
}

fn main_sources(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut directories = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut remaining = FILES_MAX;
    for _directory in 0..FILES_MAX {
        let Some(directory) = directories.pop() else {
            return Ok(files);
        };
        for entry in fs::read_dir(directory)?.take(FILES_MAX + 1) {
            remaining = remaining
                .checked_sub(1)
                .ok_or_else(|| io::Error::other("worker source traversal exceeded entry bound"))?;
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                directories.push(path);
            } else {
                // An editor's backup or a Finder file beside the code is not the code.
                if path.extension().is_some_and(|extension| extension == "rs") {
                    files.push(path);
                }
            }
            assert!(files.len() + directories.len() <= FILES_MAX);
        }
    }
    Err(io::Error::other(
        "worker source traversal exceeded file bound",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const INPUTS: [&str; 11] = [
        "collector/src/worker.rs",
        "persister/src/worker.rs",
        "persistence/src/worker.rs",
        "model/src/worker.rs",
        "collector/Cargo.toml",
        "collector/build.rs",
        "Cargo.toml",
        "Cargo.lock",
        "persister/Cargo.toml",
        "persistence/Cargo.toml",
        "model/Cargo.toml",
    ];
    static NEXT: AtomicU64 = AtomicU64::new(0);

    macro_rules! required_input {
        ($name:ident, $path:literal) => {
            #[test]
            fn $name() {
                // Goal: each required cohort input affects the actual build identity.
                // Method: change one private workspace file through production input selection.
                let fixture = Fixture::new();
                let before = fixture.fingerprint();

                fs::write(fixture.0.join($path), b"after").unwrap();

                assert_ne!(before, fixture.fingerprint(), $path);
            }
        };
    }

    required_input!(requires_collector_source, "collector/src/worker.rs");
    required_input!(requires_persister_source, "persister/src/worker.rs");
    required_input!(requires_persistence_source, "persistence/src/worker.rs");
    required_input!(requires_model_source, "model/src/worker.rs");
    required_input!(requires_collector_manifest, "collector/Cargo.toml");
    required_input!(requires_build_script, "collector/build.rs");
    required_input!(requires_workspace_manifest, "Cargo.toml");
    required_input!(requires_workspace_lockfile, "Cargo.lock");
    required_input!(requires_persister_manifest, "persister/Cargo.toml");
    required_input!(requires_persistence_manifest, "persistence/Cargo.toml");
    required_input!(requires_model_manifest, "model/Cargo.toml");

    #[test]
    fn cargo_rechecks_every_selected_directory_and_manifest() {
        // Goal: Cargo reruns production selection when any worker input can change.
        // Method: capture the real build directives for an isolated complete workspace.
        let fixture = Fixture::new();
        let mut output = Vec::new();

        main_fingerprint(&fixture.0.join("collector"), &mut output).unwrap();

        let output = String::from_utf8(output).unwrap();
        let paths: Vec<_> = output
            .lines()
            .map(|line| line.strip_prefix("cargo::rerun-if-changed=").unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                "src",
                "../persister/src",
                "../persistence/src",
                "../model/src",
                "Cargo.toml",
                "build.rs",
                "../Cargo.toml",
                "../Cargo.lock",
                "../persister/Cargo.toml",
                "../persistence/Cargo.toml",
                "../model/Cargo.toml",
            ]
        );
    }

    #[test]
    fn new_nested_source_changes_the_cohort_but_editor_files_do_not() {
        // Goal: real source discovery includes additions without depending on checkout location.
        // Method: compare two workspaces, then add nested Rust and adjacent non-source files.
        let fixture = Fixture::new();
        let equivalent = Fixture::new();
        let before = fixture.fingerprint();
        assert_eq!(before, equivalent.fingerprint());
        fs::write(fixture.0.join("model/src/worker.rs.bak"), b"editor").unwrap();
        assert_eq!(before, fixture.fingerprint());

        fs::create_dir(fixture.0.join("persistence/src/nested")).unwrap();
        fs::write(fixture.0.join("persistence/src/nested/new.rs"), b"new").unwrap();

        assert_ne!(before, fixture.fingerprint());
        assert_eq!(build_literal(before).len(), 21);
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "fd-build-fingerprint-{}-{serial}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap();
            let fixture = Self(root);
            for path in INPUTS {
                let path = fixture.0.join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, b"before").unwrap();
            }
            fixture
        }

        fn fingerprint(&self) -> u64 {
            main_fingerprint(&self.0.join("collector"), &mut io::sink()).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
}
