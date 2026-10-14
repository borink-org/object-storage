//! The checksums that only S3 takes, against the catalogue's vectors and
//! what S3 answered, and the headers a write computes them into.

#![cfg(all(
    feature = "crc32",
    feature = "crc32c",
    feature = "sha1-rustcrypto",
    feature = "sha256-rustcrypto",
    feature = "sha256-minimal",
    feature = "crc64",
))]

use borink_object_storage_crypto::{
    Checksum, Crc32, Crc32c, Crc64, Sha1RustCrypto, Sha256Minimal, Sha256RustCrypto, wipe,
};
use borink_object_storage_proto::checksum::BASE64_LEN;
use borink_object_storage_proto::s3::{self, Bucket, PayloadSigning, Service};
use borink_object_storage_proto::sigv4::Credentials;
use borink_object_storage_proto::{
    ChecksumKind, ChecksumValue, Digest, Payload, PutItem, PutOptions, Timestamps,
};

fn base64<C: Checksum>(pieces: &[&[u8]]) -> String {
    let mut sum = C::default();
    for piece in pieces {
        sum.update(piece);
    }
    let mut into = [0; BASE64_LEN];
    sum.finish().base64(&mut into).into()
}

#[test]
fn the_crcs_are_the_catalogue_crcs() {
    // <https://reveng.sourceforge.io/crc-catalogue/all.htm>, CRC-32/ISO-HDLC
    // and CRC-32/ISCSI.
    assert_eq!(Crc32::of(b"123456789"), 0xcbf4_3926);
    assert_eq!(Crc32c::of(b"123456789"), 0xe306_9283);
    assert_eq!(Crc32::of(b""), 0);
    // S3 answered this `x-amz-checksum-crc32` for a write of `checksum\n`.
    assert_eq!(base64::<Crc32>(&[b"checksum\n"]), "ItMTFg==");
    assert_eq!(base64::<Crc32>(&[b"check", b"sum\n"]), "ItMTFg==");
}

#[test]
fn the_hashes_are_the_reference_hashes() {
    // FIPS 180's "abc".
    assert_eq!(
        base64::<Sha1RustCrypto>(&[b"abc"]),
        "qZk+NkcGgWq6PiVxeFDCbJzQ2J0="
    );
    let sha256 = "YBWjp+qyV9PYdCTLkYJ9WLevegiRR86bT46jxfweu9I=";
    assert_eq!(base64::<Sha256RustCrypto>(&[b"checksum\n"]), sha256);
    assert_eq!(base64::<Sha256Minimal>(&[b"check", b"sum\n"]), sha256);
}

#[test]
fn an_s3_write_signs_the_checksum_of_each_kind() {
    let bucket = Bucket::new("https://s3.example.com", "bucket", "auto", Service::Aws).unwrap();
    let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", "secret", wipe).unwrap();
    let now = Timestamps::from_unix(1_787_400_000);
    let content = b"checksum\n";
    fn digest<C: Checksum>(content: &[u8]) -> Digest {
        let mut sum = C::default();
        sum.update(content);
        sum.finish()
    }
    for (kind, name, value) in [
        (
            ChecksumKind::Crc32,
            "x-amz-checksum-crc32",
            digest::<Crc32>(content),
        ),
        (
            ChecksumKind::Crc32c,
            "x-amz-checksum-crc32c",
            digest::<Crc32c>(content),
        ),
        (
            ChecksumKind::Crc64,
            "x-amz-checksum-crc64nvme",
            digest::<Crc64>(content),
        ),
        (
            ChecksumKind::Sha1,
            "x-amz-checksum-sha1",
            digest::<Sha1RustCrypto>(content),
        ),
        (
            ChecksumKind::Sha256,
            "x-amz-checksum-sha256",
            digest::<Sha256RustCrypto>(content),
        ),
    ] {
        let options = PutOptions {
            checksum: Some(kind),
            ..PutOptions::default()
        };
        let mut bytes = vec![0; 1024];
        let template = s3::prepare_put(
            &bucket,
            &credentials,
            &options,
            PayloadSigning::Unsigned,
            &mut bytes,
        )
        .unwrap();
        let item = PutItem {
            checksum: Some(ChecksumValue::Digest(value)),
            ..PutItem::new("object", Payload::Slice(content))
        };
        let mut buf = vec![0; 2048];
        let unsigned = s3::encode_put(&template, &bytes, &item, None, &now, &mut buf).unwrap();
        let request = s3::set_signature(&unsigned, &mut buf, &[0; 32]);
        let found = request
            .headers(&buf)
            .find(|(found, _)| *found == name)
            .map(|(_, value)| value.to_owned());
        let expected = match kind {
            ChecksumKind::Crc32 => base64::<Crc32>(&[content]),
            ChecksumKind::Crc32c => base64::<Crc32c>(&[content]),
            ChecksumKind::Sha1 => base64::<Sha1RustCrypto>(&[content]),
            ChecksumKind::Sha256 => base64::<Sha256RustCrypto>(&[content]),
            // S3 reads a CRC-64 big-endian, the reverse of the digest's bytes.
            _ => base64_of(&Crc64::of(content).to_be_bytes()),
        };
        assert_eq!(found, Some(expected), "{kind:?}");
        let authorization = request
            .headers(&buf)
            .find(|(found, _)| *found == "authorization")
            .unwrap()
            .1;
        assert!(authorization.contains(name), "{kind:?}: {authorization}");
    }
}

// Standard base64, written out here to check the encoder's.
fn base64_of(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let bits = chunk.iter().enumerate().fold(0u32, |bits, (at, byte)| {
            bits | u32::from(*byte) << (16 - 8 * at)
        });
        for at in 0..4 {
            text.push(if at <= chunk.len() {
                ALPHABET[(bits >> (18 - 6 * at) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    text
}
