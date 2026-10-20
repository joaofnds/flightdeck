//! Fingerprints the code the collector is built from. A reader built from the same code
//! carries the same fingerprint, so it can refuse a running collector built from other code.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Many times the files the collector holds, so a walk that reaches it has gone wrong.
const FILES_MAX: usize = 4096;
/// FNV-1a 64, whose result no Rust release changes, unlike `DefaultHasher`'s.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn main() -> io::Result<()> {
    println!("cargo::rerun-if-changed=src");
    println!("cargo::rerun-if-changed=Cargo.toml");

    let mut files = main_sources(Path::new("src"))?;
    files.push(PathBuf::from("Cargo.toml"));
    files.sort();

    let mut build = FNV_OFFSET_BASIS;
    for file in &files {
        let path = file.as_os_str().as_encoded_bytes();
        let contents = fs::read(file)?;
        for bytes in [path, &contents] {
            let length = u64::try_from(bytes.len()).map_err(io::Error::other)?;
            build = main_hash(build, &length.to_le_bytes());
            build = main_hash(build, bytes);
        }
    }

    let output = std::env::var_os("OUT_DIR").ok_or(io::ErrorKind::NotFound)?;
    fs::write(Path::new(&output).join("build"), build_literal(build))
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
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
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
    Ok(files)
}
