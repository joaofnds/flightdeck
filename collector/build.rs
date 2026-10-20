//! Fingerprints the code the collector is built from. A reader built from the same code
//! carries the same fingerprint, so it can refuse a running collector built from other code.

use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

/// Many times the files the collector holds, so a walk that reaches it has gone wrong.
const FILES_MAX: usize = 4096;

fn main() -> io::Result<()> {
    println!("cargo::rerun-if-changed=src");
    println!("cargo::rerun-if-changed=Cargo.toml");

    let mut files = main_sources(Path::new("src"))?;
    files.push(PathBuf::from("Cargo.toml"));
    files.sort();

    let mut hasher = DefaultHasher::new();
    for file in &files {
        file.hash(&mut hasher);
        fs::read(file)?.hash(&mut hasher);
    }

    let output = std::env::var_os("OUT_DIR").ok_or(io::ErrorKind::NotFound)?;
    fs::write(
        Path::new(&output).join("build"),
        build_literal(hasher.finish()),
    )
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
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                directories.push(path);
            } else {
                files.push(path);
            }
            assert!(files.len() + directories.len() <= FILES_MAX);
        }
    }
    Ok(files)
}
