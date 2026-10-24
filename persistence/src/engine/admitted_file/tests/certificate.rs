use std::io::{self, Cursor, Read};
use std::process::Command;

use polars_parquet_format::thrift::protocol::{
    TCompactInputProtocol, TCompactOutputProtocol, TInputProtocol, TOutputProtocol,
};

use super::allocation;
use crate::resources::ByteBudget;

#[test]
fn holds_the_supported_cursor_parser_build_assumptions() {
    // Goal: a changed build cannot silently inherit the cursor's derived layout envelope.
    // Method: check the compiler/target and both dependency locks on the existing test route.
    let compiler = Command::new("rustc").arg("-Vv").output().unwrap();
    assert!(compiler.status.success());
    let identity = std::str::from_utf8(&compiler.stdout).unwrap();

    assert!(identity.contains("release: 1.98.0\n"));
    assert!(identity.contains("commit-hash: 88d9e12ae178fab0fb5cc050a94da85685d449ea\n"));
    const {
        assert!(cfg!(target_arch = "aarch64"));
        assert!(cfg!(target_os = "macos"));
        assert!(size_of::<usize>() == 8);
    }
    assert_cursor_dependencies(include_str!("../../../../../Cargo.lock"));
    assert_cursor_dependencies(include_str!("../../../../../desktop/Cargo.lock"));
    assert_eq!(
        super::super::admission::cursor_scratch_bytes().unwrap(),
        43 * 1024 * 1024
    );
}

fn assert_cursor_dependencies(lock: &str) {
    assert_package(
        lock,
        "polars",
        "0.55.1",
        "22a147de3d2bb2e44b97338a2510816c7105c9f2",
    );
    assert_package(
        lock,
        "polars-parquet",
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
        "base64",
        "0.22.1",
        "72b3254f16251a8381aa12e40e3c4d2f0199f8c6508fbecb9d91f575e0fbb8c6",
    );
    assert_package(
        lock,
        "polars-arrow-format",
        "0.2.1",
        "a556ac0ee744e61e167f34c1eb0013ce740e0ee6cd8c158b2ec0b518f10e6675",
    );
    assert_package(
        lock,
        "planus",
        "1.1.1",
        "3daf8e3d4b712abe1d690838f6e29fb76b76ea19589c4afa39ec30e12f62af71",
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
fn holds_the_private_and_public_error_layout_terms() {
    // Goal: E=283+K+max(S,T,R)=363 continues to fit the phase headroom.
    // Method: isolate Custom with a zero-sized payload, then StringError with a static string.
    let budget = ByteBudget::new(0);
    let ((), custom) = allocation::observe(&budget, || {
        drop(std::hint::black_box(io::Error::new(
            io::ErrorKind::InvalidData,
            ZeroSizedError,
        )));
    });
    let ((), string) = allocation::observe(&budget, || {
        drop(std::hint::black_box(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unterminated varint",
        )));
    });

    assert_eq!(&custom.layouts[..custom.allocations], &[(40, 8)]);
    assert_eq!(
        &string.layouts[..string.allocations],
        &[(19, 1), (24, 8), (40, 8)]
    );
    assert_eq!(custom.allocated_bytes, custom.deallocated_bytes);
    assert_eq!(string.allocated_bytes, string.deallocated_bytes);
    assert_eq!(custom.reallocations, 0);
    assert_eq!(string.reallocations, 0);
    assert_eq!(size_of::<polars_parquet_format::thrift::Error>(), 40);
    assert_eq!(align_of::<polars_parquet_format::thrift::Error>(), 8);
    assert_eq!(size_of::<std::collections::TryReserveError>(), 16);
    assert_eq!(align_of::<std::collections::TryReserveError>(), 8);
}

#[derive(Debug)]
struct ZeroSizedError;

impl std::fmt::Display for ZeroSizedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("zero sized error")
    }
}

impl std::error::Error for ZeroSizedError {}

macro_rules! byte_capacity_cases {
    ($($name:ident: $length:expr),+ $(,)?) => {
        $(#[test]
        fn $name() {
            // Goal: native byte requests obey max(8,n) with no subsequent growth.
            // Method: decode a compact byte value through short reads and observe layouts.
            assert_byte_capacity($length, $length);
        })+
    };
}

byte_capacity_cases! {
    empty_bytes_need_no_backing: 0,
    one_byte_uses_the_minimum_capacity: 1,
    seven_bytes_use_the_minimum_capacity: 7,
    eight_bytes_need_no_growth: 8,
    thirty_one_bytes_need_no_growth: 31,
    thirty_two_bytes_need_no_growth: 32,
    footer_limit_bytes_need_no_growth: 4 * 1024 * 1024,
}

#[test]
fn truncated_native_bytes_do_not_grow_the_initial_reservation() {
    // Goal: EOF before the declared length cannot increase the capacity envelope.
    // Method: declare 32 bytes, supply seven, and observe the native reader's actual capacity.
    assert_byte_capacity(32, 7);
}

fn assert_byte_capacity(declared: usize, supplied: usize) {
    let mut encoded = Vec::new();
    TCompactOutputProtocol::new(&mut encoded)
        .write_bytes(&vec![0; declared])
        .unwrap();
    encoded.truncate(encoded.len() - (declared - supplied));
    let reader = ShortReader(Cursor::new(&encoded));
    let mut protocol = TCompactInputProtocol::new(reader, 4 * 1024 * 1024);
    let budget = ByteBudget::new(0);

    let (bytes, observation) = allocation::observe(&budget, || protocol.read_bytes().unwrap());

    assert_eq!(bytes, vec![0; supplied]);
    assert_eq!(observation.reallocations, 0);
    assert_eq!(observation.allocations, usize::from(declared != 0));
    if declared == 0 {
        assert_eq!(bytes.capacity(), 0);
    } else {
        assert_eq!(observation.layouts[0], (declared.max(8), 1));
        assert_eq!(bytes.capacity(), declared.max(8));
    }
}

struct ShortReader<R>(R);

impl<R: Read> Read for ShortReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let length = bytes.len().min(3);
        self.0.read(&mut bytes[..length])
    }
}
