//! Incremental original-census digest, using the SDK `CommonCrypto` caller-owned context.
use std::io;

pub(super) struct CensusDigest {
    context: CC_SHA256_CTX,
}
#[allow(non_camel_case_types)]
#[repr(C)]
struct CC_SHA256_CTX {
    count: [u32; 2],
    hash: [u32; 8],
    wbuf: [u32; 16],
}
#[link(name = "System")]
unsafe extern "C" {
    fn CC_SHA256_Init(context: *mut CC_SHA256_CTX) -> std::ffi::c_int;
    fn CC_SHA256_Update(
        context: *mut CC_SHA256_CTX,
        data: *const std::ffi::c_void,
        length: u32,
    ) -> std::ffi::c_int;
    fn CC_SHA256_Final(digest: *mut u8, context: *mut CC_SHA256_CTX) -> std::ffi::c_int;
}
const _: () = assert!(size_of::<CC_SHA256_CTX>() == 104);
impl CensusDigest {
    pub(super) fn new() -> io::Result<Self> {
        let mut context = CC_SHA256_CTX {
            count: [0; 2],
            hash: [0; 8],
            wbuf: [0; 16],
        };
        if unsafe { CC_SHA256_Init(&raw mut context) } != 1 {
            return Err(io::Error::other("CC_SHA256_Init"));
        }
        Ok(Self { context })
    }
    pub(super) fn update(&mut self, bytes: &[u8]) -> io::Result<()> {
        assert!(bytes.len() <= 12544);
        let length = u32::try_from(bytes.len()).map_err(io::Error::other)?;
        if unsafe { CC_SHA256_Update(&raw mut self.context, bytes.as_ptr().cast(), length) } != 1 {
            return Err(io::Error::other("CC_SHA256_Update"));
        }
        Ok(())
    }
    pub(super) fn finish(mut self) -> io::Result<[u8; 32]> {
        let mut digest = [0; 32];
        if unsafe { CC_SHA256_Final(digest.as_mut_ptr(), &raw mut self.context) } != 1 {
            return Err(io::Error::other("CC_SHA256_Final"));
        }
        Ok(digest)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_incremental_sha256_vectors() {
        // Goal: pin the native digest and incremental context layout with independent vectors.
        // Method: empty, split abc and one million a bytes, with bounded updates.
        for (parts, expected) in [
            (
                vec![],
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                vec![b"a".as_slice(), b"bc"],
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
        ] {
            let mut digest = CensusDigest::new().unwrap();
            for part in parts {
                digest.update(part).unwrap();
            }
            assert_eq!(hex(&digest.finish().unwrap()), expected);
        }
        let mut digest = CensusDigest::new().unwrap();
        for _ in 0..1000 {
            digest.update(&[b'a'; 1000]).unwrap();
        }
        assert_eq!(
            hex(&digest.finish().unwrap()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }
    fn hex(bytes: &[u8; 32]) -> String {
        use std::fmt::Write;
        let mut result = String::with_capacity(64);
        for byte in bytes {
            write!(&mut result, "{byte:02x}").unwrap();
        }
        result
    }
}
