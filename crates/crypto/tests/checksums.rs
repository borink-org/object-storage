//! The checksums themselves, against the vectors Azure answered and the
//! catalogue's own.

#![cfg(all(feature = "crc64", feature = "md5"))]

use borink_crypto::{Checksum, Crc64, Md5};
use borink_object_storage_proto::checksum::BASE64_LEN;

fn base64<C: Checksum>(pieces: &[&[u8]]) -> String {
    let mut sum = C::default();
    for piece in pieces {
        sum.update(piece);
    }
    let mut into = [0; BASE64_LEN];
    sum.finish().base64(&mut into).into()
}

#[test]
fn crc64_is_the_catalogue_crc_64_nvme() {
    // <https://reveng.sourceforge.io/crc-catalogue/all.htm#crc.cat.crc-64-nvme>
    assert_eq!(Crc64::of(b"123456789"), 0xae8b_1486_0a79_9888);
    assert_eq!(Crc64::of(b""), 0);
}

#[test]
fn crc64_matches_what_azure_answered() {
    // Azure answered this `x-ms-content-crc64` for a `0123456789` write, and
    // stored it when the write sent it.
    assert_eq!(base64::<Crc64>(&[b"0123456789"]), "HZz9TO6x+RU=");
    assert_eq!(base64::<Crc64>(&[b"0123", b"456789"]), "HZz9TO6x+RU=");
    assert_eq!(base64::<Crc64>(&[b""]), "AAAAAAAAAAA=");
    // One piece at a time is the same as all of them at once.
    let long: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    let whole = base64::<Crc64>(&[&long]);
    let pieces: Vec<&[u8]> = long.chunks(7).collect();
    assert_eq!(base64::<Crc64>(&pieces), whole);
}

#[test]
fn md5_matches_the_reference_vectors_and_what_azure_stored() {
    assert_eq!(base64::<Md5>(&[b""]), "1B2M2Y8AsgTpgAmY7PhCfg==");
    // Azure computed and stored this for a `0123456789` write.
    assert_eq!(base64::<Md5>(&[b"0123456789"]), "eB5eJF1ptWaXm4bijSPyxw==");
    assert_eq!(
        base64::<Md5>(&[b"01234", b"56789"]),
        "eB5eJF1ptWaXm4bijSPyxw=="
    );
    // The block list of a one-block commit, which is what a commit sums.
    assert_eq!(
        base64::<Md5>(&[
            b"<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList><Latest>AAAAAA==</Latest></BlockList>"
        ]),
        "YzOsE0fk1HdRsGkEw5j/sg=="
    );
    // Across the 56-byte and 64-byte padding boundaries, in one piece and in
    // two.
    let long = [b'a'; 200];
    for len in [55, 56, 63, 64, 65, 119, 120, 128, 200] {
        let expected = base64::<Md5>(&[&long[..len]]);
        assert_eq!(
            base64::<Md5>(&[&long[..len / 2], &long[len / 2..len]]),
            expected,
            "{len}"
        );
    }
}

#[test]
fn every_implementation_fits_the_state_slot() {
    // `provider` asserts this at compile time; this reports the numbers that
    // `ChecksumState::LEN` is chosen against.
    use borink_object_storage_proto::checksum::ChecksumState;
    for (name, size) in [("Crc64", size_of::<Crc64>()), ("Md5", size_of::<Md5>())] {
        assert!(size <= ChecksumState::LEN, "{name} is {size} bytes");
    }
    assert_eq!(size_of::<Md5>(), 88);
}
