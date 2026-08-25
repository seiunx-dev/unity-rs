//! `UnityCN` ("archive storage") bundle decryption.
//!
//! Some Unity China builds ship bundles whose blocks-info table and compressed
//! blocks are obfuscated with a scheme keyed by a 16-byte value the game holds.
//! The scheme is not Unity's and is not documented; the behaviour implemented
//! here was derived from the two public reference implementations that support
//! it, `K0lb3/UnityPy` and the `Razmoth/PGRStudio` fork it credits.
//!
//! This module ships no key material and never guesses one. A caller that has a
//! key supplies it through [`crate::bundle::BundleOpenOptions`], the same shape
//! the Oodle decoder already uses, so the crate stays a format library.
//!
//! Only the token, match-offset and length-extension bytes of an LZ4 stream are
//! obfuscated; literal runs are skipped untouched. Every read here is bounds
//! checked against the block, and both length-extension chains are capped, so a
//! hostile block returns an error rather than reading out of range or looping.

use crate::{Error, Result};

/// Plaintext a correct key reproduces from the signature vectors.
const SIGNATURE: &[u8; 16] = b"#$unity3dchina!@";

/// Bytes the encryption header occupies, after the archive flags.
///
/// One 32-bit word, then two vectors of 16 data bytes, 16 key bytes and one
/// reserved byte each.
pub(crate) const HEADER_BYTES: u64 = 70;

/// Marks encrypted data, both in the archive flags for the blocks-info table
/// and in a storage block's own flags.
pub(crate) const ARCHIVE_ENCRYPTED_FLAG: u32 = 0x100;

/// A caller-supplied `UnityCN` decryption key.
///
/// The crate neither ships nor derives keys. Obtaining one for a given title is
/// the caller's responsibility.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct UnityCnKey([u8; 16]);

impl UnityCnKey {
    #[must_use]
    pub const fn new(key: [u8; 16]) -> Self {
        Self(key)
    }
}

impl std::fmt::Debug for UnityCnKey {
    /// Never prints the key material.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("UnityCnKey(<redacted>)")
    }
}

/// The encryption header that follows the archive flags.
#[derive(Clone, Copy)]
pub(crate) struct UnityCnHeader {
    data: [u8; 16],
    key: [u8; 16],
    data_signature: [u8; 16],
    key_signature: [u8; 16],
}

impl UnityCnHeader {
    /// Parses the 70-byte header from `bytes`.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        let expected = usize::try_from(HEADER_BYTES)
            .map_err(|_| Error::invalid_data("UnityCN header size does not fit this platform"))?;
        if bytes.len() < expected {
            return Err(Error::invalid_data(
                "UnityCN encryption header is truncated",
            ));
        }
        // Skip the leading word, then read each vector's data and key, each
        // followed by one reserved byte.
        let vector = |start: usize| -> ([u8; 16], [u8; 16]) {
            let mut data = [0_u8; 16];
            let mut key = [0_u8; 16];
            data.copy_from_slice(&bytes[start..start + 16]);
            key.copy_from_slice(&bytes[start + 16..start + 32]);
            (data, key)
        };
        let (data, key) = vector(4);
        let (data_signature, key_signature) = vector(37);
        Ok(Self {
            data,
            key,
            data_signature,
            key_signature,
        })
    }
}

/// Substitution tables derived from the header and a caller-supplied key.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ArchiveDecryptor {
    /// 16 nibbles indexed by a ciphertext nibble.
    index: [u8; 16],
    /// 16 nibbles read as a transposed 4x4 matrix, indexed by the counter.
    substitute: [u8; 16],
}

/// Prints nothing about the header's key material.
///
/// The stored key is ciphertext rather than the caller's key, but it is half
/// of what derives the substitution tables, and a derived `Debug` is exactly
/// how such a value reaches a log by accident.
impl std::fmt::Debug for UnityCnHeader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("UnityCnHeader(<redacted>)")
    }
}

/// Prints nothing about the derived tables.
///
/// These are not the key; for an attacker they are better than the key, since
/// they decrypt this bundle without it. Redacting `UnityCnKey` alone would
/// leave the capability printable one struct away.
impl std::fmt::Debug for ArchiveDecryptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ArchiveDecryptor(<redacted>)")
    }
}

impl ArchiveDecryptor {
    /// Verifies `key` against the header's signature vectors and derives the
    /// substitution tables.
    pub(crate) fn new(header: &UnityCnHeader, key: UnityCnKey) -> Result<Self> {
        let signature = mask_with_key(&key.0, &header.key_signature, &header.data_signature);
        if &signature != SIGNATURE {
            return Err(Error::invalid_data(
                "the supplied UnityCN key does not match this bundle",
            ));
        }

        let material = mask_with_key(&key.0, &header.key, &header.data);
        let mut nibbles = [0_u8; 32];
        for (byte, pair) in material.iter().zip(nibbles.chunks_exact_mut(2)) {
            pair[0] = byte >> 4;
            pair[1] = byte & 0xF;
        }

        let mut index = [0_u8; 16];
        index.copy_from_slice(&nibbles[..16]);
        let mut substitute = [0_u8; 16];
        for column in 0..4 {
            for row in 0..4 {
                substitute[column * 4 + row] = nibbles[16 + row * 4 + column];
            }
        }
        Ok(Self { index, substitute })
    }

    /// Decrypts one block in place.
    ///
    /// `block_index` is the block's position in the storage table; the
    /// blocks-info table itself uses zero.
    pub(crate) fn decrypt_block(&self, data: &mut [u8], block_index: usize) -> Result<()> {
        // Only the low eight bits of the counter are ever consulted, so a
        // wrapping byte is exactly equivalent to the unbounded reference
        // counter and keeps every table index trivially in range.
        let mut counter = u8::try_from(block_index % 256).expect("a value below 256 fits u8");
        let mut offset = 0_usize;
        while offset < data.len() {
            offset = offset
                .checked_add(self.decrypt_token(data, offset, counter)?)
                .ok_or_else(|| Error::invalid_data("UnityCN block offset overflowed"))?;
            counter = counter.wrapping_add(1);
        }
        Ok(())
    }

    /// Decrypts one LZ4 token group, returning how many bytes it spans.
    fn decrypt_token(&self, data: &mut [u8], base: usize, mut counter: u8) -> Result<usize> {
        let remaining = data.len() - base;
        let mut local = 0_usize;

        let token = self.decrypt_byte(data, base, &mut local, &mut counter)?;
        let mut literal = usize::from(token >> 4);
        let match_length_extends = token & 0xF == 0xF;

        if literal == 0xF {
            literal =
                self.read_extension(data, base, &mut local, &mut counter, remaining, literal)?;
        }

        // The literal run itself is stored in the clear.
        local = local
            .checked_add(literal)
            .ok_or_else(|| Error::invalid_data("UnityCN literal run overflowed"))?;

        if local < remaining {
            self.decrypt_byte(data, base, &mut local, &mut counter)?;
            self.decrypt_byte(data, base, &mut local, &mut counter)?;
            if match_length_extends {
                self.read_extension(data, base, &mut local, &mut counter, remaining, 0)?;
            }
        }
        Ok(local)
    }

    /// Consumes a 0xFF-terminated length chain, accumulating onto `total`.
    ///
    /// The chain is capped by the bytes left in the block, so a block of all
    /// 0xFF cannot spin.
    fn read_extension(
        &self,
        data: &mut [u8],
        base: usize,
        local: &mut usize,
        counter: &mut u8,
        remaining: usize,
        mut total: usize,
    ) -> Result<usize> {
        let mut steps = 0_usize;
        loop {
            let value = self.decrypt_byte(data, base, local, counter)?;
            total = total
                .checked_add(usize::from(value))
                .ok_or_else(|| Error::invalid_data("UnityCN length extension overflowed"))?;
            if value != 0xFF {
                return Ok(total);
            }
            steps += 1;
            if steps > remaining {
                return Err(Error::invalid_data(
                    "UnityCN length extension runs past the block",
                ));
            }
        }
    }

    /// Decrypts the byte at `base + local`, advancing both cursors.
    fn decrypt_byte(
        &self,
        data: &mut [u8],
        base: usize,
        local: &mut usize,
        counter: &mut u8,
    ) -> Result<u8> {
        let position = base
            .checked_add(*local)
            .ok_or_else(|| Error::invalid_data("UnityCN byte offset overflowed"))?;
        let slot = data
            .get_mut(position)
            .ok_or_else(|| Error::invalid_data("UnityCN block ends inside an encrypted token"))?;
        let mask = self.counter_mask(*counter);
        let value = *slot;
        let low = self.index[usize::from(value & 0xF)].wrapping_sub(mask) & 0xF;
        let high = self.index[usize::from(value >> 4)]
            .wrapping_sub(mask)
            .wrapping_mul(0x10);
        *slot = high | low;
        *local += 1;
        *counter = counter.wrapping_add(1);
        Ok(*slot)
    }

    /// Folds four counter-selected table entries into this byte's mask.
    fn counter_mask(&self, counter: u8) -> u8 {
        self.substitute[usize::from((counter >> 2) & 3) + 4]
            .wrapping_add(self.substitute[usize::from(counter & 3)])
            .wrapping_add(self.substitute[usize::from((counter >> 4) & 3) + 8])
            .wrapping_add(self.substitute[usize::from(counter >> 6) + 12])
    }
}

/// Returns `AES-128-encrypt(key, source) XOR data`.
///
/// Both the key check and the table derivation use this same primitive; note it
/// is the *encryption* direction in both cases.
fn mask_with_key(key: &[u8; 16], source: &[u8; 16], data: &[u8; 16]) -> [u8; 16] {
    let mut block = *source;
    aes128_encrypt_block(key, &mut block);
    let mut output = [0_u8; 16];
    for (slot, (left, right)) in output.iter_mut().zip(data.iter().zip(block.iter())) {
        *slot = left ^ right;
    }
    output
}

/// AES S-box, derived from the multiplicative inverse in GF(2^8) followed by
/// the affine transform, per FIPS-197.
#[rustfmt::skip]
const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b,
    0xfe, 0xd7, 0xab, 0x76, 0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0,
    0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0, 0xb7, 0xfd, 0x93, 0x26,
    0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2,
    0xeb, 0x27, 0xb2, 0x75, 0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0,
    0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84, 0x53, 0xd1, 0x00, 0xed,
    0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f,
    0x50, 0x3c, 0x9f, 0xa8, 0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5,
    0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2, 0xcd, 0x0c, 0x13, 0xec,
    0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14,
    0xde, 0x5e, 0x0b, 0xdb, 0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c,
    0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79, 0xe7, 0xc8, 0x37, 0x6d,
    0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f,
    0x4b, 0xbd, 0x8b, 0x8a, 0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e,
    0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e, 0xe1, 0xf8, 0x98, 0x11,
    0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f,
    0xb0, 0x54, 0xbb, 0x16,
];

const ROUND_COUNT: usize = 10;

/// Encrypts one 16-byte block with AES-128, in place.
///
/// Only the forward direction is implemented: the `UnityCN` scheme performs two
/// single-block encryptions and never decrypts. Validated against the FIPS-197
/// Appendix B and C.1 vectors.
fn aes128_encrypt_block(key: &[u8; 16], block: &mut [u8; 16]) {
    let round_keys = expand_key(key);
    let (first, rest) = round_keys.split_first().expect("AES-128 has 11 round keys");
    add_round_key(block, first);
    for round_key in &rest[..ROUND_COUNT - 1] {
        substitute_bytes(block);
        shift_rows(block);
        mix_columns(block);
        add_round_key(block, round_key);
    }
    substitute_bytes(block);
    shift_rows(block);
    add_round_key(block, &round_keys[ROUND_COUNT]);
}

fn expand_key(key: &[u8; 16]) -> [[u8; 16]; ROUND_COUNT + 1] {
    let mut words = [[0_u8; 4]; 4 * (ROUND_COUNT + 1)];
    for (word, chunk) in words.iter_mut().zip(key.chunks_exact(4)) {
        word.copy_from_slice(chunk);
    }
    let mut round_constant = 1_u8;
    for index in 4..words.len() {
        let mut temporary = words[index - 1];
        if index % 4 == 0 {
            temporary.rotate_left(1);
            for byte in &mut temporary {
                *byte = SBOX[usize::from(*byte)];
            }
            temporary[0] ^= round_constant;
            round_constant = xtime(round_constant);
        }
        let previous = words[index - 4];
        for (slot, (left, right)) in words[index]
            .iter_mut()
            .zip(previous.iter().zip(temporary.iter()))
        {
            *slot = left ^ right;
        }
    }

    let mut round_keys = [[0_u8; 16]; ROUND_COUNT + 1];
    for (round, round_key) in round_keys.iter_mut().enumerate() {
        for (position, word) in words[round * 4..round * 4 + 4].iter().enumerate() {
            round_key[position * 4..position * 4 + 4].copy_from_slice(word);
        }
    }
    round_keys
}

fn add_round_key(block: &mut [u8; 16], round_key: &[u8; 16]) {
    for (slot, key) in block.iter_mut().zip(round_key.iter()) {
        *slot ^= key;
    }
}

fn substitute_bytes(block: &mut [u8; 16]) {
    for slot in block.iter_mut() {
        *slot = SBOX[usize::from(*slot)];
    }
}

/// Rotates row `r` left by `r`, on the column-major AES state.
fn shift_rows(block: &mut [u8; 16]) {
    let source = *block;
    for row in 1..4 {
        for column in 0..4 {
            block[row + 4 * column] = source[row + 4 * ((column + row) % 4)];
        }
    }
}

fn mix_columns(block: &mut [u8; 16]) {
    for column in block.chunks_exact_mut(4) {
        let a = [column[0], column[1], column[2], column[3]];
        for (position, slot) in column.iter_mut().enumerate() {
            let next = a[(position + 1) % 4];
            *slot = xtime(a[position])
                ^ (xtime(next) ^ next)
                ^ a[(position + 2) % 4]
                ^ a[(position + 3) % 4];
        }
    }
}

/// Multiplies by x in GF(2^8) modulo the AES polynomial.
fn xtime(value: u8) -> u8 {
    (value << 1) ^ if value & 0x80 == 0 { 0 } else { 0x1B }
}

#[cfg(test)]
mod tests {
    use super::{
        ArchiveDecryptor, HEADER_BYTES, UnityCnHeader, UnityCnKey, aes128_encrypt_block,
        mask_with_key,
    };

    #[test]
    fn aes128_matches_the_fips_197_vectors() {
        // FIPS-197 Appendix B.
        let mut block = [
            0x32, 0x43, 0xf6, 0xa8, 0x88, 0x5a, 0x30, 0x8d, 0x31, 0x31, 0x98, 0xa2, 0xe0, 0x37,
            0x07, 0x34,
        ];
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        aes128_encrypt_block(&key, &mut block);
        assert_eq!(
            block,
            [
                0x39, 0x25, 0x84, 0x1d, 0x02, 0xdc, 0x09, 0xfb, 0xdc, 0x11, 0x85, 0x97, 0x19, 0x6a,
                0x0b, 0x32
            ]
        );

        // FIPS-197 Appendix C.1.
        let mut block = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let key = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        aes128_encrypt_block(&key, &mut block);
        assert_eq!(
            block,
            [
                0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4,
                0xc5, 0x5a
            ]
        );
    }

    /// Builds a header whose signature vectors verify against `key`.
    fn header_for(key: &[u8; 16], material: [u8; 16]) -> Vec<u8> {
        // data = signature XOR encrypt(key, key_vector), inverting mask_with_key.
        let key_signature = [0x5a_u8; 16];
        let data_signature = mask_with_key(key, &key_signature, super::SIGNATURE);
        let key_vector = [0xa5_u8; 16];
        let data_vector = mask_with_key(key, &key_vector, &material);

        let mut header = vec![0_u8; 4];
        header.extend_from_slice(&data_vector);
        header.extend_from_slice(&key_vector);
        header.push(0);
        header.extend_from_slice(&data_signature);
        header.extend_from_slice(&key_signature);
        header.push(0);
        assert_eq!(header.len(), usize::try_from(HEADER_BYTES).unwrap());
        header
    }

    #[test]
    fn rejects_a_key_that_does_not_reproduce_the_signature() {
        let key = [7_u8; 16];
        let header = header_for(&key, [0x1b; 16]);
        let parsed = UnityCnHeader::parse(&header).unwrap();

        assert!(ArchiveDecryptor::new(&parsed, UnityCnKey::new(key)).is_ok());
        let error = ArchiveDecryptor::new(&parsed, UnityCnKey::new([8_u8; 16])).unwrap_err();
        assert!(error.to_string().contains("does not match this bundle"));
    }

    #[test]
    fn rejects_a_truncated_header() {
        let key = [7_u8; 16];
        let header = header_for(&key, [0x1b; 16]);
        let error = UnityCnHeader::parse(&header[..header.len() - 1]).unwrap_err();
        assert!(error.to_string().contains("truncated"));
    }

    /// A decryptor whose tables make the byte transform the identity, so a test
    /// can drive the token traversal with literal LZ4 bytes instead of having
    /// to invert the substitution.
    fn identity_decryptor() -> ArchiveDecryptor {
        ArchiveDecryptor {
            index: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
            substitute: [0; 16],
        }
    }

    /// Builds key material whose derived `index` table is the permutation
    /// `i * 7 + 3 (mod 16)` and whose substitute nibbles are non-trivial.
    ///
    /// A permutation is what makes the byte transform invertible, which the
    /// round-trip test needs; real key material is not guaranteed to be one.
    fn invertible_material() -> [u8; 16] {
        let mut nibbles = [0_u8; 32];
        for (position, slot) in nibbles[..16].iter_mut().enumerate() {
            *slot = u8::try_from((position * 7 + 3) % 16).expect("a nibble fits u8");
        }
        for (position, slot) in nibbles[16..].iter_mut().enumerate() {
            *slot = u8::try_from((position * 5 + 1) % 16).expect("a nibble fits u8");
        }
        let mut material = [0_u8; 16];
        for (byte, pair) in material.iter_mut().zip(nibbles.chunks_exact(2)) {
            *byte = (pair[0] << 4) | pair[1];
        }
        material
    }

    /// Test-only inverse of the byte transform.
    fn encrypt_byte(decryptor: &ArchiveDecryptor, plain: u8, counter: u8) -> u8 {
        let mask = decryptor.counter_mask(counter);
        let find = |target: u8| -> u8 {
            u8::try_from(
                (0..16_usize)
                    .find(|candidate| {
                        decryptor.index[*candidate].wrapping_sub(mask) & 0xF == target
                    })
                    .expect("the fixture's index table is a permutation"),
            )
            .expect("a nibble fits u8")
        };
        (find(plain >> 4) << 4) | find(plain & 0xF)
    }

    /// Test-only inverse of [`ArchiveDecryptor::decrypt_block`].
    ///
    /// The traversal is driven by the decrypted bytes, so walking the plaintext
    /// makes exactly the decisions the decryptor will make on the ciphertext.
    fn encrypt_block(decryptor: &ArchiveDecryptor, plain: &[u8], block_index: usize) -> Vec<u8> {
        let mut output = plain.to_vec();
        let mut group = u8::try_from(block_index % 256).expect("a value below 256 fits u8");
        let mut offset = 0_usize;
        while offset < plain.len() {
            let remaining = plain.len() - offset;
            let mut counter = group;
            let mut local = 0_usize;
            let take = |local: &mut usize, counter: &mut u8, output: &mut Vec<u8>| -> u8 {
                let value = plain[offset + *local];
                output[offset + *local] = encrypt_byte(decryptor, value, *counter);
                *local += 1;
                *counter = counter.wrapping_add(1);
                value
            };

            let token = take(&mut local, &mut counter, &mut output);
            let mut literal = usize::from(token >> 4);
            let extends_match = token & 0xF == 0xF;
            if literal == 0xF {
                loop {
                    let value = take(&mut local, &mut counter, &mut output);
                    literal += usize::from(value);
                    if value != 0xFF {
                        break;
                    }
                }
            }
            local += literal;
            if local < remaining {
                take(&mut local, &mut counter, &mut output);
                take(&mut local, &mut counter, &mut output);
                if extends_match {
                    while take(&mut local, &mut counter, &mut output) == 0xFF {}
                }
            }
            offset += local;
            group = group.wrapping_add(1);
        }
        output
    }

    #[test]
    fn decrypting_inverts_encrypting_across_token_shapes_and_block_indices() {
        let key = [0x31_u8; 16];
        let parsed = UnityCnHeader::parse(&header_for(&key, invertible_material())).unwrap();
        let decryptor = ArchiveDecryptor::new(&parsed, UnityCnKey::new(key)).unwrap();

        let streams: [Vec<u8>; 4] = [
            // A short literal run and a plain match.
            vec![0x30, 0xAA, 0xBB, 0xCC, 0x04, 0x00],
            // An extending literal length, so the 0xFF chain is exercised.
            vec![0xF0, 0x02, 0x01, 0x00],
            // An extending match length after the offset pair.
            vec![0x2F, 0x11, 0x22, 0x08, 0x00, 0x03],
            // Several groups back to back, so the per-group counter advances.
            vec![0x10, 0x41, 0x02, 0x00, 0x20, 0x51, 0x52, 0x03, 0x00],
        ];

        for stream in &streams {
            for block_index in [0_usize, 1, 7, 255, 256, 4097] {
                let encrypted = encrypt_block(&decryptor, stream, block_index);
                assert_ne!(
                    &encrypted, stream,
                    "the fixture should not be a fixed point"
                );
                let mut roundtrip = encrypted;
                decryptor
                    .decrypt_block(&mut roundtrip, block_index)
                    .unwrap();
                assert_eq!(&roundtrip, stream, "block index {block_index}");
            }
        }
    }

    #[test]
    fn traverses_a_well_formed_token_group_without_touching_the_literal_run() {
        // Token 0x30: a three-byte literal run and a non-extending match, then
        // the two match-offset bytes. The identity tables mean a correct
        // traversal leaves every byte untouched.
        let original = [0x30_u8, 0xAA, 0xBB, 0xCC, 0x01, 0x00];
        let mut block = original;
        identity_decryptor().decrypt_block(&mut block, 0).unwrap();
        assert_eq!(block, original);
    }

    #[test]
    fn a_hostile_block_errors_rather_than_reading_out_of_range_or_spinning() {
        // Every byte 0xFF makes the token claim an extending literal length and
        // an extending match length, and every extension byte extends again.
        // The traversal must run out of block and report it, not spin.
        let decryptor = identity_decryptor();
        for length in [1_usize, 2, 7, 64, 255, 4096] {
            let mut block = vec![0xFF_u8; length];
            let error = decryptor
                .decrypt_block(&mut block, 0)
                .expect_err("an all-0xFF block cannot be a valid token stream");
            assert!(
                error.to_string().contains("UnityCN"),
                "length {length}: {error}"
            );
        }
    }

    #[test]
    fn a_real_key_decrypts_without_panicking_on_arbitrary_blocks() {
        let key = [7_u8; 16];
        let parsed = UnityCnHeader::parse(&header_for(&key, [0x1b; 16])).unwrap();
        let decryptor = ArchiveDecryptor::new(&parsed, UnityCnKey::new(key)).unwrap();

        // Whatever the derived tables happen to be, every input either decrypts
        // or reports an error, in bounded time. Reaching the end of this loop is
        // the assertion.
        for length in 0_usize..96 {
            for fill in [0x00_u8, 0x0F, 0xF0, 0xFF, 0x5A] {
                let mut block = vec![fill; length];
                let _ = decryptor.decrypt_block(&mut block, length);
            }
        }
    }

    /// No type on the decryption path prints key material.
    ///
    /// The key itself is the obvious one. The header carries the encrypted key
    /// blob, and the decryptor carries the tables derived from both -- which
    /// are better than the key for an attacker, since they decrypt this bundle
    /// without it. Both of those derived `Debug` until this test was widened,
    /// so redacting only the key left the capability printable one struct away.
    #[test]
    fn no_key_material_appears_in_debug_output() {
        let bytes = [0xab_u8; 16];
        let key = UnityCnKey::new(bytes);
        assert_eq!(format!("{key:?}"), "UnityCnKey(<redacted>)");

        let header_bytes = header_for(&bytes, [0x1b; 16]);
        let header = UnityCnHeader::parse(&header_bytes).unwrap();
        assert_eq!(format!("{header:?}"), "UnityCnHeader(<redacted>)");

        let decryptor = ArchiveDecryptor::new(&header, UnityCnKey::new(bytes)).unwrap();
        assert_eq!(format!("{decryptor:?}"), "ArchiveDecryptor(<redacted>)");

        // Nested in another value, which is how such a thing actually reaches a
        // log: a container's derived `Debug` calls each field's.
        let nested = format!("{:?}", (key, header, decryptor));
        assert!(!nested.contains("ab"), "{nested}");
        assert!(!nested.contains("171"), "{nested}");
    }
}
