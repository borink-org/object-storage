//! The SHA-256 providers, against the published vectors and each other.

#![cfg(any(feature = "sha256-rustcrypto", feature = "sha256-minimal"))]

use borink_object_storage_crypto::Sha256;
use borink_object_storage_proto::sigv4::{Sha256Provider, Sha256State};

// Every provider that this build has.
#[allow(clippy::vec_init_then_push)]
fn providers() -> Vec<(&'static str, Sha256Provider)> {
    let mut all = Vec::new();
    #[cfg(feature = "sha256-rustcrypto")]
    all.push((
        "rustcrypto",
        borink_object_storage_crypto::SHA256_RUSTCRYPTO,
    ));
    #[cfg(feature = "sha256-minimal")]
    all.push(("minimal", borink_object_storage_crypto::SHA256_MINIMAL));
    all
}

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn every_provider_matches_the_published_vectors() {
    for (name, provider) in providers() {
        // FIPS 180-2, appendix B.
        for (input, expected) in [
            (
                b"".as_slice(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ] {
            assert_eq!(hex(provider.hash(input)), expected, "{name}");
        }
        // RFC 4231, test cases 1, 2 and 6. The last key is longer than a
        // block, so HMAC hashes it first.
        for (key, message, expected) in [
            (
                [0x0b; 20].as_slice(),
                b"Hi There".as_slice(),
                "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            ),
            (
                b"Jefe",
                b"what do ya want for nothing?",
                "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            ),
            (
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First",
                "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
            ),
        ] {
            assert_eq!(hex(provider.hmac(key, message)), expected, "{name}");
        }
    }
}

#[test]
fn providers_agree_around_the_block_boundaries() {
    let all = providers();
    let (first_name, first) = all[0];
    for len in [0, 1, 55, 56, 63, 64, 65, 127, 128, 129, 1000] {
        let data = vec![0x5a; len];
        for (name, provider) in &all[1..] {
            assert_eq!(
                provider.hash(&data),
                first.hash(&data),
                "{name} vs {first_name}, {len}"
            );
            assert_eq!(
                provider.hmac(&data, &data),
                first.hmac(&data, &data),
                "{name} vs {first_name}, {len}"
            );
        }
    }
}

// One piece at a time is the same as all of them at once.
fn pieces_hash_as_the_whole<S: Sha256>(name: &str) {
    let long: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    for len in [0, 1, 55, 56, 63, 64, 65, 127, 128, 129, 1000] {
        let mut whole = S::default();
        whole.update(&long[..len]);
        let mut pieces = S::default();
        for piece in long[..len].chunks(7) {
            pieces.update(piece);
        }
        assert_eq!(pieces.finish(), whole.finish(), "{name}, {len}");
    }
}

#[test]
fn every_implementation_hashes_in_pieces() {
    #[cfg(feature = "sha256-rustcrypto")]
    pieces_hash_as_the_whole::<borink_object_storage_crypto::Sha256RustCrypto>("rustcrypto");
    #[cfg(feature = "sha256-minimal")]
    pieces_hash_as_the_whole::<borink_object_storage_crypto::Sha256Minimal>("minimal");
}

#[test]
#[allow(clippy::vec_init_then_push)]
fn every_implementation_fits_the_state_slot() {
    // `sha256_provider` asserts this at compile time; this reports the
    // numbers that `Sha256State::LEN` is chosen against.
    let mut sizes = Vec::new();
    #[cfg(feature = "sha256-rustcrypto")]
    sizes.push((
        "rustcrypto",
        size_of::<borink_object_storage_crypto::Sha256RustCrypto>(),
    ));
    #[cfg(feature = "sha256-minimal")]
    sizes.push((
        "minimal",
        size_of::<borink_object_storage_crypto::Sha256Minimal>(),
    ));
    for (name, size) in sizes {
        assert!(size <= Sha256State::LEN, "{name} is {size} bytes");
    }
}
