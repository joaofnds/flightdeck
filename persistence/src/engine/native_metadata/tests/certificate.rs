use std::process::Command;

use polars_buffer::Buffer;
use polars_parquet::parquet::error::ParquetError;
use polars_parquet::parquet::metadata::ColumnChunkMetadata;
use polars_parquet::parquet::read::deserialize_metadata;
use polars_parquet_format::{ColumnChunk, ColumnOrder, RowGroup};

use crate::allocation_tests as allocation;
use crate::resources::ByteBudget;

#[test]
fn holds_the_native_metadata_source_and_layout_assumptions() {
    // Goal: compiler, target and native-source changes invalidate this envelope's certificate.
    // Method: pin the supported compiler and both locks, and check public layout consequences.
    let compiler = Command::new("rustc").arg("-Vv").output().unwrap();
    assert!(compiler.status.success());
    let identity = std::str::from_utf8(&compiler.stdout).unwrap();

    assert!(identity.contains("release: 1.98.0\n"));
    assert!(identity.contains("commit-hash: 88d9e12ae178fab0fb5cc050a94da85685d449ea\n"));
    const {
        assert!(cfg!(target_arch = "aarch64"));
        assert!(cfg!(target_os = "macos"));
        assert!(size_of::<usize>() == 8);
        // compact.rs: CompactRowGroup is Vec + i64 + i64 + Option<Vec>, exactly 64 bytes.
        // ColumnChunkMetadata contains CompactColumnChunk inline, plus a descriptor reference.
        // column_order.rs: the private tag is a three-variant payload-free enum (one byte).
        // Its zero-sized raw counterpart needs the separate footer-byte term instead.
        // These are consequences of the pinned definitions, not duplicated private types.
        assert!(64 <= size_of::<RowGroup>());
        assert!(size_of::<ColumnChunkMetadata>() <= size_of::<ColumnChunk>());
        assert!(size_of::<ColumnOrder>() == 0);
        assert!(size_of::<polars::prelude::PlSmallStr>() == 24);
    }
    assert_dependencies(include_str!("../../../../../Cargo.lock"));
    assert_dependencies(include_str!("../../../../../desktop/Cargo.lock"));
}

fn assert_dependencies(lock: &str) {
    assert_package(
        lock,
        "polars-parquet",
        "0.55.1",
        "22a147de3d2bb2e44b97338a2510816c7105c9f2",
    );
    assert_package(
        lock,
        "polars-buffer",
        "0.55.1",
        "22a147de3d2bb2e44b97338a2510816c7105c9f2",
    );
    assert_package(
        lock,
        "polars-utils",
        "0.55.1",
        "22a147de3d2bb2e44b97338a2510816c7105c9f2",
    );
    assert_package(
        lock,
        "polars-parquet-format",
        "0.1.0",
        "c025243dcfe8dbc57e94d9f82eb3bef10b565ab180d5b99bed87fd8aea319ce1",
    );
    assert_package(
        lock,
        "hashbrown",
        "0.17.1",
        "d76a6f81abd8f388676aff15068236a8d21c7e1a",
    );
    assert_package(
        lock,
        "foldhash",
        "0.2.0",
        "77ce24cb58228fbb8aa041425bb1050850ac19177686ea6e0f41a70416f56fdb",
    );
    assert_package(
        lock,
        "compact_str",
        "0.9.1",
        "9dfdd1c2274d9aa354115b09dc9a901d6c5576818cdf70d14cae2bdb47df00ab",
    );
}

fn assert_package(lock: &str, name: &str, version: &str, source: &str) {
    let header = format!("\nname = \"{name}\"\n");
    let version = format!("\nversion = \"{version}\"\n");
    let packages: Vec<_> = lock
        .split("[[package]]")
        .filter(|package| package.starts_with(&header))
        .filter(|package| package.contains(&version))
        .collect();
    assert_eq!(packages.len(), 1, "{name}");
    assert!(packages[0].contains(source), "{name}");
}

#[test]
fn holds_the_borrowed_buffer_owner_layout_and_release() {
    // Goal: borrowed metadata storage requests only its owner and releases it under charge.
    // Method: isolate Buffer::with_slice without parsing, observing the owner allocation/free.
    let budget = ByteBudget::new(0);

    let ((), observation) = allocation::observe(&budget, || {
        Buffer::with_slice(&[0u8; 1], |buffer| drop(std::hint::black_box(buffer)));
    });

    assert_eq!(&observation.layouts[..observation.allocations], &[(48, 8)]);
    assert_eq!(observation.deallocated_bytes, 48);
    assert_eq!(observation.reallocations, 0);
}

#[test]
fn drops_native_parser_errors_inside_the_borrowed_buffer_scope() {
    // Goal: a native ParquetError's owned message fits the error term and dies in scope.
    // Method: truncate a required integer and observe pure Parquet rejection and release.
    let budget = ByteBudget::new(0);

    let ((), observation) = allocation::observe(&budget, || {
        Buffer::with_slice(&[0x15], |buffer| {
            let error = deserialize_metadata(buffer).unwrap_err();
            assert!(matches!(error, ParquetError::OutOfSpec(_)));
            drop(error);
        });
    });

    assert!(observation.allocated_bytes <= 48 + 4096);
    assert_eq!(observation.allocated_bytes, observation.deallocated_bytes);
}
