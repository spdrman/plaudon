//! Spec 13.8: the envelope the password-based encryption accepts, and what a
//! reader does with a body that is outside it.
//!
//! The header of a `.recording-archive` is attacker-controlled: it is read
//! before the password is known and it decides how much memory the restore is
//! about to spend and how many bytes it is about to allocate per chunk. Every
//! test here feeds the reader a header or a body our writer will never produce
//! and asserts the typed refusal, not a message.

use std::io::{Read as _, Write as _};

use plaudon_export::archive::crypto::{
    DecryptingReader, EncryptingWriter, EncryptionHeader, KdfParams, KdfProfile, AEAD_FAILURE,
    CHUNK_BYTES, FRAMING_FAILURE, MAX_P_COST, TAG_BYTES,
};
use plaudon_export::error::{ArchiveRejection, ExportError};

const PASSWORD: &str = "correct horse battery staple";
const AAD: &[u8] = b"header-bytes";

fn rejection(error: &ExportError) -> String {
    match error {
        ExportError::ArchiveRejected(ArchiveRejection::KdfParametersRejected(detail)) => {
            detail.clone()
        }
        other => panic!("expected a key-derivation rejection, got {other:?}"),
    }
}

fn test_params() -> KdfParams {
    KdfParams::generate(KdfProfile::Test)
}

fn header_with_chunk(chunk_bytes: u32) -> EncryptionHeader {
    let mut header = EncryptionHeader::generate(KdfProfile::Test);
    header.chunk_bytes = chunk_bytes;
    header
}

fn seal(plaintext: &[u8], header: &EncryptionHeader) -> Vec<u8> {
    let mut writer = EncryptingWriter::new(Vec::new(), header, AAD, PASSWORD).unwrap();
    writer.write_all(plaintext).unwrap();
    writer.finish().unwrap()
}

fn read_all(body: &[u8], header: &EncryptionHeader) -> std::io::Result<Vec<u8>> {
    let mut reader = DecryptingReader::new(body, header, AAD, PASSWORD).unwrap();
    let mut out = Vec::new();
    reader.read_to_end(&mut out)?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Key-derivation parameters declared by the archive
// ---------------------------------------------------------------------------

#[test]
fn an_argon2_version_we_do_not_implement_is_refused() {
    let mut params = test_params();
    params.version = 16;

    assert_eq!(
        rejection(&params.validate().unwrap_err()),
        "unsupported Argon2 version 16"
    );
    // `derive` validates first, so no hashing happens for a header we reject.
    assert_eq!(
        rejection(&params.derive(PASSWORD).unwrap_err()),
        "unsupported Argon2 version 16"
    );
}

#[test]
fn a_derived_key_length_other_than_the_cipher_key_size_is_refused() {
    let mut params = test_params();
    params.key_bytes = 16;

    assert_eq!(
        rejection(&params.validate().unwrap_err()),
        "unsupported key length 16"
    );
    assert_eq!(
        rejection(&params.derive(PASSWORD).unwrap_err()),
        "unsupported key length 16"
    );
}

#[test]
fn a_parallelism_outside_the_accepted_range_is_refused_at_both_ends() {
    let mut zero = test_params();
    zero.p_cost = 0;
    assert_eq!(
        rejection(&zero.validate().unwrap_err()),
        "parallelism 0 is out of range"
    );

    let mut huge = test_params();
    huge.p_cost = MAX_P_COST + 1;
    assert_eq!(
        rejection(&huge.validate().unwrap_err()),
        format!("parallelism {} is out of range", MAX_P_COST + 1)
    );
    assert!(huge.derive(PASSWORD).is_err());
}

#[test]
fn a_memory_cost_too_small_for_its_lanes_is_refused_before_argon2_sees_it() {
    let mut params = test_params();
    params.p_cost = 4;
    // Argon2 needs 8 KiB per lane; 31 KiB for four lanes is not a
    // configuration the library can run, so we name it rather than letting the
    // KDF fail with its own wording.
    params.m_cost_kib = 31;

    assert_eq!(
        rejection(&params.validate().unwrap_err()),
        "memory cost is below the Argon2 minimum"
    );
    assert_eq!(
        rejection(&params.derive(PASSWORD).unwrap_err()),
        "memory cost is below the Argon2 minimum"
    );
}

#[test]
fn a_salt_that_is_too_short_or_not_hex_is_refused() {
    let mut short = test_params();
    short.salt_hex = "00112233".to_owned(); // four bytes, decodes cleanly
    assert_eq!(
        rejection(&short.validate().unwrap_err()),
        "salt is too short"
    );
    assert!(short.derive(PASSWORD).is_err());

    let mut not_hex = test_params();
    not_hex.salt_hex = "nonsense".to_owned();
    assert_eq!(
        rejection(&not_hex.validate().unwrap_err()),
        "salt is not hex"
    );
}

#[test]
fn a_header_declaring_an_outsized_chunk_is_refused_before_any_buffer_is_sized() {
    let mut header = EncryptionHeader::generate(KdfProfile::Test);
    header.chunk_bytes = u32::try_from(8 * CHUNK_BYTES).unwrap() + 1;

    assert_eq!(
        rejection(&header.validate().unwrap_err()),
        format!("unsupported chunk size {}", header.chunk_bytes)
    );
    assert!(EncryptingWriter::new(Vec::new(), &header, AAD, PASSWORD).is_err());
    assert!(DecryptingReader::new(&[][..], &header, AAD, PASSWORD).is_err());
}

// ---------------------------------------------------------------------------
// Body framing
// ---------------------------------------------------------------------------

#[test]
fn an_empty_body_is_malformed_rather_than_an_empty_archive() {
    let header = header_with_chunk(64);

    let error = read_all(&[], &header).unwrap_err();
    assert!(
        error.to_string().contains(FRAMING_FAILURE),
        "unexpected {error}"
    );
}

#[test]
fn a_chunk_declaring_more_bytes_than_the_header_allows_is_refused_before_allocating() {
    let header = header_with_chunk(64);
    let mut body = 1_000_000u32.to_le_bytes().to_vec();
    body.push(0); // "more chunks follow"

    let error = read_all(&body, &header).unwrap_err();
    assert!(
        error.to_string().contains(FRAMING_FAILURE),
        "unexpected {error}"
    );
}

#[test]
fn a_chunk_too_short_to_carry_an_authentication_tag_is_refused() {
    let header = header_with_chunk(64);
    let short = u32::try_from(TAG_BYTES).unwrap() - 1;
    let mut body = short.to_le_bytes().to_vec();
    body.push(1); // "final chunk"
    body.extend_from_slice(&[0u8; TAG_BYTES - 1]);

    let error = read_all(&body, &header).unwrap_err();
    assert!(
        error.to_string().contains(FRAMING_FAILURE),
        "unexpected {error}"
    );
}

#[test]
fn a_chunk_header_whose_ciphertext_never_arrives_is_refused() {
    let header = header_with_chunk(64);
    let mut body = 32u32.to_le_bytes().to_vec();
    body.push(1);
    // The 32 ciphertext bytes the frame promises are simply not there.

    let error = read_all(&body, &header).unwrap_err();
    assert!(
        error.to_string().contains(FRAMING_FAILURE),
        "unexpected {error}"
    );
}

#[test]
fn a_reader_that_failed_authentication_does_not_resume_on_the_next_chunk() {
    let header = header_with_chunk(16);
    let body = seal(&[7u8; 48], &header);
    assert_eq!(read_all(&body, &header).unwrap(), vec![7u8; 48]);

    let mut tampered = body;
    tampered[5] ^= 0x01; // first ciphertext byte of the first chunk

    let mut reader = DecryptingReader::new(&tampered[..], &header, AAD, PASSWORD).unwrap();
    let mut buffer = [0u8; 16];
    let first = reader.read(&mut buffer).unwrap_err();
    assert!(
        first.to_string().contains(AEAD_FAILURE),
        "unexpected {first}"
    );

    // The STREAM decryptor was consumed by the failed chunk. A caller that
    // ignores the error and reads again gets another refusal, never the
    // plaintext of the chunks behind it.
    let second = reader.read(&mut buffer).unwrap_err();
    assert!(
        second.to_string().contains(FRAMING_FAILURE),
        "unexpected {second}"
    );
    assert_eq!(buffer, [0u8; 16]);
}
