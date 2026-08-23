//! Bounded inspection of Animation Compression Library (`compressed_tracks`) blobs.
//!
//! This module validates the public outer ACL 2.x container contract without
//! materializing the compressed payload. It deliberately does not claim to
//! decompress ACL tracks: bit-rate tables, segment data, and Tuanjie decoder-map
//! semantics require a separately verified decoder.

use crate::source::Region;
use crate::{Error, Result};

const RAW_HEADER_BYTES: u64 = 8;
const MINIMUM_TRACKS_BYTES: u64 = 32;
const MINIMUM_TRACKS_BYTES_USIZE: usize = 32;
const COMPRESSED_TRACKS_TAG: u32 = 0xAC11_AC11;
const FIRST_SUPPORTED_VERSION: u16 = 7;
const LATEST_SUPPORTED_VERSION: u16 = 10;
const UNIFORMLY_SAMPLED_ALGORITHM: u8 = 0;

/// Resource limits applied while inspecting one ACL `compressed_tracks` blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AclCompressedTracksLimits {
    pub maximum_compressed_bytes: u64,
    pub maximum_tracks: u32,
    pub maximum_samples_per_track: u32,
    /// Maximum number of scalar output values implied by the header.
    pub maximum_decompressed_values: u64,
}

impl Default for AclCompressedTracksLimits {
    fn default() -> Self {
        Self {
            maximum_compressed_bytes: 256 * 1024 * 1024,
            maximum_tracks: 2_000_000,
            maximum_samples_per_track: 2_000_000,
            maximum_decompressed_values: 128 * 1024 * 1024,
        }
    }
}

/// Limits for materializing the exact inputs required by an ACL decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AclDecoderInputLimits {
    pub compressed_tracks: AclCompressedTracksLimits,
    pub maximum_decoder_map_entries: usize,
    /// Combined compressed-track and serialized decoder-map bytes.
    pub maximum_materialized_bytes: u64,
}

impl Default for AclDecoderInputLimits {
    fn default() -> Self {
        Self {
            compressed_tracks: AclCompressedTracksLimits::default(),
            maximum_decoder_map_entries: 2_000_000,
            maximum_materialized_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Serialized ACL track type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclTrackType {
    Float1,
    Float2,
    Float3,
    Float4,
    Vector4,
    /// Quaternion, translation, and scale (4 + 3 + 3 scalar values).
    Qvv,
}

impl AclTrackType {
    fn from_serialized(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Float1),
            1 => Ok(Self::Float2),
            2 => Ok(Self::Float3),
            3 => Ok(Self::Float4),
            4 => Ok(Self::Vector4),
            12 => Ok(Self::Qvv),
            _ => Err(Error::invalid_data(format!(
                "ACL compressed tracks use unknown track type {value}"
            ))),
        }
    }

    #[must_use]
    pub const fn serialized_value(self) -> u8 {
        match self {
            Self::Float1 => 0,
            Self::Float2 => 1,
            Self::Float3 => 2,
            Self::Float4 => 3,
            Self::Vector4 => 4,
            Self::Qvv => 12,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Float1 => "float1f",
            Self::Float2 => "float2f",
            Self::Float3 => "float3f",
            Self::Float4 => "float4f",
            Self::Vector4 => "vector4f",
            Self::Qvv => "qvvf",
        }
    }

    const fn scalar_values_per_track(self) -> u64 {
        match self {
            Self::Float1 => 1,
            Self::Float2 => 2,
            Self::Float3 => 3,
            Self::Float4 | Self::Vector4 => 4,
            Self::Qvv => 10,
        }
    }
}

/// Validated ACL 2.x outer header and its source-bound compressed bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclCompressedTracks {
    pub region: Region,
    pub declared_size: u32,
    pub stored_hash: u32,
    pub version: u16,
    pub track_type: AclTrackType,
    pub num_tracks: u32,
    pub num_samples_per_track: u32,
    pub sample_rate_bits: u32,
    pub misc_packed: u32,
    pub decompressed_value_count: u64,
}

/// Fully bounded, owned input for a future pure-Rust or user-supplied decoder.
///
/// The compressed outer container has already passed [`inspect_compressed_tracks`].
/// No sample layout or Tuanjie decoder-map semantics are inferred here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclDecoderInput {
    pub header: AclCompressedTracks,
    pub compressed_tracks: Vec<u8>,
    pub decoder_map: Vec<u32>,
}

/// Metadata and validated bytes passed to an injected ACL decoder.
#[derive(Debug)]
pub struct AclDecodeRequest<'a> {
    pub frame_count: u32,
    pub bone_count: u32,
    pub sample_rate_bits: u32,
    pub declared_curve_count: Option<u32>,
    pub use_fast_sample_mode: Option<bool>,
    pub input: &'a AclDecoderInput,
}

impl AclDecodeRequest<'_> {
    #[must_use]
    pub const fn sample_rate(&self) -> f32 {
        f32::from_bits(self.sample_rate_bits)
    }
}

/// Frame-major scalar curves returned by an injected ACL decoder.
///
/// `binding_indices[column]` is the absolute Unity binding scalar index for
/// `values[frame * curve_count + column]`. Indices must be strictly increasing.
/// `following_curve_offset` is the binding offset for the ordinary streamed,
/// dense, and constant curves serialized beside ACL data.
#[derive(Debug, Clone, PartialEq)]
pub struct AclDecodedClip {
    pub times: Vec<f32>,
    pub binding_indices: Vec<u32>,
    pub values: Vec<f32>,
    pub following_curve_offset: u32,
}

/// Safe extension point for pure-Rust and language-binding ACL decoders.
///
/// Implementations receive validated, owned input and cannot access Unity
/// parser internals or raw pointers. Request-declared frame, curve, and scalar
/// output counts have already passed [`AclDecodeLimits`] before this method is
/// called; returned values still undergo complete structural validation.
pub trait AclDecoder: Send + Sync {
    fn decode(&self, request: &AclDecodeRequest<'_>) -> Result<AclDecodedClip>;
}

/// Resource limits applied to injected decoder output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AclDecodeLimits {
    pub input: AclDecoderInputLimits,
    pub maximum_frames: usize,
    pub maximum_curves: usize,
    pub maximum_values: usize,
}

impl Default for AclDecodeLimits {
    fn default() -> Self {
        Self {
            input: AclDecoderInputLimits::default(),
            maximum_frames: 2_000_000,
            maximum_curves: 2_000_000,
            maximum_values: 128 * 1024 * 1024,
        }
    }
}

/// Rejects declared decoder work that already exceeds the caller's limits.
pub fn validate_decode_request(
    request: &AclDecodeRequest<'_>,
    limits: AclDecodeLimits,
) -> Result<()> {
    let expected_frames = usize::try_from(request.frame_count)
        .map_err(|_| Error::invalid_data("ACL frame count does not fit this platform"))?;
    if expected_frames > limits.maximum_frames {
        return Err(Error::invalid_data(format!(
            "ACL frame count {expected_frames} exceeds limit {}",
            limits.maximum_frames
        )));
    }
    let Some(declared_curves) = request.declared_curve_count else {
        return Ok(());
    };
    let declared_curves = usize::try_from(declared_curves)
        .map_err(|_| Error::invalid_data("ACL curve count does not fit this platform"))?;
    if declared_curves > limits.maximum_curves {
        return Err(Error::invalid_data(format!(
            "ACL declares {declared_curves} curves, exceeding limit {}",
            limits.maximum_curves
        )));
    }
    let expected_values = expected_frames
        .checked_mul(declared_curves)
        .ok_or_else(|| Error::invalid_data("ACL decoded value count overflowed"))?;
    if expected_values > limits.maximum_values {
        return Err(Error::invalid_data(format!(
            "ACL decoder output requires {expected_values} values, exceeding limit {}",
            limits.maximum_values
        )));
    }
    Ok(())
}

/// Validates all structural and numerical invariants of injected decoder output.
pub fn validate_decoded_clip(
    decoded: &AclDecodedClip,
    request: &AclDecodeRequest<'_>,
    limits: AclDecodeLimits,
) -> Result<()> {
    validate_decode_request(request, limits)?;
    let expected_frames = usize::try_from(request.frame_count)
        .map_err(|_| Error::invalid_data("ACL frame count does not fit this platform"))?;
    if expected_frames > limits.maximum_frames {
        return Err(Error::invalid_data(format!(
            "ACL frame count {expected_frames} exceeds limit {}",
            limits.maximum_frames
        )));
    }
    if decoded.times.len() != expected_frames {
        return Err(Error::invalid_data(format!(
            "ACL decoder returned {} times for {expected_frames} declared frames",
            decoded.times.len()
        )));
    }
    let curve_count = decoded.binding_indices.len();
    if curve_count > limits.maximum_curves {
        return Err(Error::invalid_data(format!(
            "ACL decoder returned {curve_count} curves, exceeding limit {}",
            limits.maximum_curves
        )));
    }
    if let Some(declared) = request.declared_curve_count {
        let declared = usize::try_from(declared)
            .map_err(|_| Error::invalid_data("ACL curve count does not fit this platform"))?;
        if curve_count != declared {
            return Err(Error::invalid_data(format!(
                "ACL decoder returned {curve_count} curves for {declared} declared curves"
            )));
        }
    }
    for pair in decoded.binding_indices.windows(2) {
        if pair[0] >= pair[1] {
            return Err(Error::invalid_data(
                "ACL decoder binding indices are not strictly increasing",
            ));
        }
    }
    if decoded
        .binding_indices
        .last()
        .is_some_and(|index| *index >= decoded.following_curve_offset)
    {
        return Err(Error::invalid_data(
            "ACL decoder following-curve offset does not follow every decoded binding index",
        ));
    }
    let expected_values = expected_frames
        .checked_mul(curve_count)
        .ok_or_else(|| Error::invalid_data("ACL decoded value count overflowed"))?;
    if expected_values > limits.maximum_values {
        return Err(Error::invalid_data(format!(
            "ACL decoder output requires {expected_values} values, exceeding limit {}",
            limits.maximum_values
        )));
    }
    if decoded.values.len() != expected_values {
        return Err(Error::invalid_data(format!(
            "ACL decoder returned {} values; {expected_values} are required",
            decoded.values.len()
        )));
    }
    let mut previous_time = None;
    for &time in &decoded.times {
        if !time.is_finite() || previous_time.is_some_and(|previous| time < previous) {
            return Err(Error::invalid_data(
                "ACL decoder times must be finite and monotonically non-decreasing",
            ));
        }
        previous_time = Some(time);
    }
    if decoded.values.iter().any(|value| !value.is_finite()) {
        return Err(Error::invalid_data(
            "ACL decoder returned a non-finite curve value",
        ));
    }
    Ok(())
}

impl AclCompressedTracks {
    #[must_use]
    pub const fn sample_rate(&self) -> f32 {
        f32::from_bits(self.sample_rate_bits)
    }

    #[must_use]
    pub const fn has_metadata(&self) -> bool {
        self.misc_packed & (1 << 31) != 0
    }

    #[must_use]
    pub const fn is_wrap_optimized(&self) -> bool {
        self.misc_packed & (1 << 30) != 0
    }

    #[must_use]
    pub const fn has_database(&self) -> bool {
        matches!(self.track_type, AclTrackType::Qvv) && self.misc_packed & (1 << 8) != 0
    }

    #[must_use]
    pub const fn has_stripped_keyframes(&self) -> bool {
        matches!(self.track_type, AclTrackType::Qvv) && self.misc_packed & (1 << 10) != 0
    }
}

/// Validates the source-bound outer ACL 2.x `compressed_tracks` container.
///
/// The size, tag, version, algorithm, track type, finite sample rate, implied
/// output size, and FNV-1a payload hash are checked. Hashing is streamed in
/// fixed chunks and never materializes the whole compressed blob.
pub fn inspect_compressed_tracks(
    region: &Region,
    limits: AclCompressedTracksLimits,
) -> Result<AclCompressedTracks> {
    if region.len() < MINIMUM_TRACKS_BYTES {
        return Err(Error::invalid_data(format!(
            "ACL compressed tracks are {} bytes; at least {MINIMUM_TRACKS_BYTES} are required",
            region.len()
        )));
    }
    if region.len() > limits.maximum_compressed_bytes {
        return Err(Error::invalid_data(format!(
            "ACL compressed tracks are {} bytes, exceeding limit {}",
            region.len(),
            limits.maximum_compressed_bytes
        )));
    }

    let mut bytes = [0_u8; MINIMUM_TRACKS_BYTES_USIZE];
    region.read_exact_at(0, &mut bytes)?;
    let declared_size = read_u32(&bytes, 0);
    let declared_size_u64 = u64::from(declared_size);
    if declared_size_u64 != region.len() {
        return Err(Error::invalid_data(format!(
            "ACL compressed tracks declare {declared_size} bytes but the bounded region contains {}",
            region.len()
        )));
    }

    let stored_hash = read_u32(&bytes, 4);
    let tag = read_u32(&bytes, 8);
    if tag != COMPRESSED_TRACKS_TAG {
        return Err(Error::invalid_data(format!(
            "ACL compressed tracks tag is 0x{tag:08x}, expected 0x{COMPRESSED_TRACKS_TAG:08x}"
        )));
    }
    let version = read_u16(&bytes, 12);
    if !(FIRST_SUPPORTED_VERSION..=LATEST_SUPPORTED_VERSION).contains(&version) {
        return Err(Error::unsupported(format!(
            "ACL compressed tracks version {version}; supported versions are {FIRST_SUPPORTED_VERSION}..={LATEST_SUPPORTED_VERSION}"
        )));
    }
    let algorithm = bytes[14];
    if algorithm != UNIFORMLY_SAMPLED_ALGORITHM {
        return Err(Error::unsupported(format!(
            "ACL compressed tracks algorithm {algorithm}; only uniformly sampled algorithm 0 is supported"
        )));
    }
    let track_type = AclTrackType::from_serialized(bytes[15])?;
    let num_tracks = read_u32(&bytes, 16);
    if num_tracks > limits.maximum_tracks {
        return Err(Error::invalid_data(format!(
            "ACL compressed tracks contain {num_tracks} tracks, exceeding limit {}",
            limits.maximum_tracks
        )));
    }
    let num_samples_per_track = read_u32(&bytes, 20);
    if num_samples_per_track > limits.maximum_samples_per_track {
        return Err(Error::invalid_data(format!(
            "ACL compressed tracks contain {num_samples_per_track} samples per track, exceeding limit {}",
            limits.maximum_samples_per_track
        )));
    }
    let sample_rate_bits = read_u32(&bytes, 24);
    let sample_rate = f32::from_bits(sample_rate_bits);
    if !sample_rate.is_finite() || (num_samples_per_track != 0 && sample_rate <= 0.0) {
        return Err(Error::invalid_data(format!(
            "ACL compressed tracks sample rate {sample_rate} is not valid"
        )));
    }
    let misc_packed = read_u32(&bytes, 28);
    let decompressed_value_count = u64::from(num_tracks)
        .checked_mul(u64::from(num_samples_per_track))
        .and_then(|value| value.checked_mul(track_type.scalar_values_per_track()))
        .ok_or_else(|| Error::invalid_data("ACL decompressed value count overflowed"))?;
    if decompressed_value_count > limits.maximum_decompressed_values {
        return Err(Error::invalid_data(format!(
            "ACL compressed tracks imply {decompressed_value_count} scalar values, exceeding limit {}",
            limits.maximum_decompressed_values
        )));
    }

    let actual_hash = hash_region(
        region,
        RAW_HEADER_BYTES,
        declared_size_u64 - RAW_HEADER_BYTES,
    )?;
    if actual_hash != stored_hash {
        return Err(Error::invalid_data(format!(
            "ACL compressed tracks hash is 0x{stored_hash:08x}, calculated 0x{actual_hash:08x}"
        )));
    }

    Ok(AclCompressedTracks {
        region: region.clone(),
        declared_size,
        stored_hash,
        version,
        track_type,
        num_tracks,
        num_samples_per_track,
        sample_rate_bits,
        misc_packed,
        decompressed_value_count,
    })
}

fn hash_region(region: &Region, offset: u64, length: u64) -> Result<u32> {
    let mut hash = 2_166_136_261_u32;
    let mut consumed = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];
    while consumed < length {
        let remaining = length - consumed;
        let chunk_length = usize::try_from(
            remaining.min(u64::try_from(buffer.len()).expect("fixed buffer length fits u64")),
        )
        .expect("chunk length is bounded by the fixed buffer");
        region.read_exact_at(offset + consumed, &mut buffer[..chunk_length])?;
        for &byte in &buffer[..chunk_length] {
            hash = (hash ^ u32::from(byte)).wrapping_mul(16_777_619);
        }
        consumed += u64::try_from(chunk_length).expect("fixed chunk length fits u64");
    }
    Ok(hash)
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation_clip::{AclClip, ByteRegion, U32Array};
    use crate::endian::Endian;

    fn compressed_tracks(
        version: u16,
        track_type: u8,
        num_tracks: u32,
        num_samples: u32,
        sample_rate: f32,
        payload: &[u8],
    ) -> Vec<u8> {
        let payload_len = u64::try_from(payload.len()).unwrap();
        let size = u32::try_from(MINIMUM_TRACKS_BYTES + payload_len).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&COMPRESSED_TRACKS_TAG.to_le_bytes());
        bytes.extend_from_slice(&version.to_le_bytes());
        bytes.push(UNIFORMLY_SAMPLED_ALGORITHM);
        bytes.push(track_type);
        bytes.extend_from_slice(&num_tracks.to_le_bytes());
        bytes.extend_from_slice(&num_samples.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_bits().to_le_bytes());
        bytes.extend_from_slice(&0xC000_0500_u32.to_le_bytes());
        bytes.extend_from_slice(payload);
        let hash = fnv1a(&bytes[8..]);
        bytes[4..8].copy_from_slice(&hash.to_le_bytes());
        bytes
    }

    fn fnv1a(bytes: &[u8]) -> u32 {
        bytes.iter().fold(2_166_136_261_u32, |hash, byte| {
            (hash ^ u32::from(*byte)).wrapping_mul(16_777_619)
        })
    }

    #[test]
    fn inspects_source_bound_transform_tracks() {
        let bytes = compressed_tracks(10, 12, 3, 12, 30.0, &[1, 2, 3, 4]);
        let tracks = inspect_compressed_tracks(
            &Region::from_bytes(bytes),
            AclCompressedTracksLimits::default(),
        )
        .unwrap();
        assert_eq!(tracks.declared_size, 36);
        assert_eq!(tracks.version, 10);
        assert_eq!(tracks.track_type, AclTrackType::Qvv);
        assert_eq!(tracks.num_tracks, 3);
        assert_eq!(tracks.num_samples_per_track, 12);
        assert_eq!(tracks.sample_rate().to_bits(), 30.0_f32.to_bits());
        assert_eq!(tracks.decompressed_value_count, 360);
        assert!(tracks.has_metadata());
        assert!(tracks.is_wrap_optimized());
        assert!(tracks.has_database());
        assert!(tracks.has_stripped_keyframes());
        assert_eq!(tracks.region.read_to_vec(36).unwrap().len(), 36);
    }

    #[test]
    fn supports_every_public_acl_track_type() {
        for (serialized, expected, values) in [
            (0, AclTrackType::Float1, 6),
            (1, AclTrackType::Float2, 12),
            (2, AclTrackType::Float3, 18),
            (3, AclTrackType::Float4, 24),
            (4, AclTrackType::Vector4, 24),
            (12, AclTrackType::Qvv, 60),
        ] {
            let bytes = compressed_tracks(7, serialized, 2, 3, 60.0, &[]);
            let tracks = inspect_compressed_tracks(
                &Region::from_bytes(bytes),
                AclCompressedTracksLimits::default(),
            )
            .unwrap();
            assert_eq!(tracks.track_type, expected);
            assert_eq!(tracks.decompressed_value_count, values);
            assert_eq!(tracks.track_type.serialized_value(), serialized);
        }
    }

    #[test]
    fn rejects_wrong_size_tag_hash_version_algorithm_type_and_rate() {
        let base = compressed_tracks(10, 12, 3, 12, 30.0, &[]);
        for (description, mutate) in [
            ("declare", (0_usize, 0_u8)),
            ("tag", (8, 0)),
            ("hash", (4, 0)),
            ("version", (12, 6)),
            ("algorithm", (14, 1)),
            ("track type", (15, 11)),
        ] {
            let mut bytes = base.clone();
            bytes[mutate.0] = mutate.1;
            let error = inspect_compressed_tracks(
                &Region::from_bytes(bytes),
                AclCompressedTracksLimits::default(),
            )
            .unwrap_err();
            assert!(error.to_string().contains(description), "{error}");
        }

        let bytes = compressed_tracks(10, 12, 3, 12, f32::NAN, &[]);
        let error = inspect_compressed_tracks(
            &Region::from_bytes(bytes),
            AclCompressedTracksLimits::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("sample rate"));
    }

    #[test]
    fn rejects_all_resource_limits_before_decompression() {
        let bytes = compressed_tracks(10, 12, 3, 12, 30.0, &[0; 64]);
        for limits in [
            AclCompressedTracksLimits {
                maximum_compressed_bytes: 32,
                ..AclCompressedTracksLimits::default()
            },
            AclCompressedTracksLimits {
                maximum_tracks: 2,
                ..AclCompressedTracksLimits::default()
            },
            AclCompressedTracksLimits {
                maximum_samples_per_track: 11,
                ..AclCompressedTracksLimits::default()
            },
            AclCompressedTracksLimits {
                maximum_decompressed_values: 359,
                ..AclCompressedTracksLimits::default()
            },
        ] {
            assert!(inspect_compressed_tracks(&Region::from_bytes(bytes.clone()), limits).is_err());
        }
    }

    #[test]
    fn materializes_validated_decoder_inputs_with_one_combined_budget() {
        let tracks = compressed_tracks(10, 12, 3, 12, 30.0, &[1, 2, 3, 4]);
        let map_bytes = [0x10_u32.to_le_bytes(), 0x20_u32.to_le_bytes()].concat();
        let clip = AclClip {
            frame_count: 12,
            bone_count: 3,
            sample_rate_bits: 30.0_f32.to_bits(),
            curve_count: Some(7),
            tracks: ByteRegion {
                byte_length: u64::try_from(tracks.len()).unwrap(),
                region: Region::from_bytes(tracks.clone()),
            },
            decoder_map: U32Array {
                region: Region::from_bytes(map_bytes),
                count: 2,
                endian: Endian::Little,
            },
            use_fast_sample_mode: Some(true),
        };
        let input = clip
            .materialize_decoder_input(AclDecoderInputLimits {
                maximum_materialized_bytes: 44,
                ..AclDecoderInputLimits::default()
            })
            .unwrap();
        assert_eq!(input.header.track_type, AclTrackType::Qvv);
        assert_eq!(input.compressed_tracks, tracks);
        assert_eq!(input.decoder_map, [0x10, 0x20]);

        for limits in [
            AclDecoderInputLimits {
                maximum_decoder_map_entries: 1,
                ..AclDecoderInputLimits::default()
            },
            AclDecoderInputLimits {
                maximum_materialized_bytes: 43,
                ..AclDecoderInputLimits::default()
            },
        ] {
            assert!(clip.materialize_decoder_input(limits).is_err());
        }
    }

    struct FakeDecoder {
        output: AclDecodedClip,
    }

    impl AclDecoder for FakeDecoder {
        fn decode(&self, request: &AclDecodeRequest<'_>) -> Result<AclDecodedClip> {
            assert_eq!(request.frame_count, 2);
            assert_eq!(request.bone_count, 1);
            assert_eq!(request.sample_rate().to_bits(), 30.0_f32.to_bits());
            assert_eq!(request.declared_curve_count, Some(2));
            assert_eq!(request.input.decoder_map, [7, 8]);
            Ok(self.output.clone())
        }
    }

    struct MustNotRunDecoder;

    impl AclDecoder for MustNotRunDecoder {
        fn decode(&self, _request: &AclDecodeRequest<'_>) -> Result<AclDecodedClip> {
            panic!("ACL decoder ran before request limits were checked")
        }
    }

    fn decodable_clip() -> AclClip {
        let tracks = compressed_tracks(10, 12, 1, 2, 30.0, &[]);
        let map_bytes = [7_u32.to_le_bytes(), 8_u32.to_le_bytes()].concat();
        AclClip {
            frame_count: 2,
            bone_count: 1,
            sample_rate_bits: 30.0_f32.to_bits(),
            curve_count: Some(2),
            tracks: ByteRegion {
                byte_length: u64::try_from(tracks.len()).unwrap(),
                region: Region::from_bytes(tracks),
            },
            decoder_map: U32Array {
                region: Region::from_bytes(map_bytes),
                count: 2,
                endian: Endian::Little,
            },
            use_fast_sample_mode: Some(true),
        }
    }

    #[test]
    fn validates_injected_decoder_output_before_consumers_see_it() {
        let clip = decodable_clip();
        let valid = AclDecodedClip {
            times: vec![0.0, 1.0 / 30.0],
            binding_indices: vec![2, 3],
            values: vec![1.0, 2.0, 3.0, 4.0],
            following_curve_offset: 4,
        };
        assert_eq!(
            clip.decode_with(
                &FakeDecoder {
                    output: valid.clone(),
                },
                AclDecodeLimits::default(),
            )
            .unwrap(),
            valid
        );

        assert_rejected_shapes(&clip);
        assert_rejected_limits(&clip);
    }

    /// Each hostile output paired with the guard it must trip.
    ///
    /// Asserting only `is_err()` would let a case that fails for an unrelated
    /// reason stand in for a branch nothing exercises, which is how four of
    /// these went uncovered: a non-finite time, an offset that does not follow
    /// the last binding, a curve count disagreeing with the declared one, and
    /// each of the three resource ceilings.
    fn assert_rejected_shapes(clip: &AclClip) {
        let cases: &[(&str, AclDecodedClip)] = &[
            (
                "returned 1 times",
                AclDecodedClip {
                    times: vec![0.0],
                    binding_indices: vec![2, 3],
                    values: vec![1.0, 2.0],
                    following_curve_offset: 4,
                },
            ),
            (
                "monotonically non-decreasing",
                AclDecodedClip {
                    times: vec![0.0, -1.0],
                    binding_indices: vec![2, 3],
                    values: vec![1.0, 2.0, 3.0, 4.0],
                    following_curve_offset: 4,
                },
            ),
            (
                "monotonically non-decreasing",
                AclDecodedClip {
                    times: vec![0.0, f32::INFINITY],
                    binding_indices: vec![2, 3],
                    values: vec![1.0, 2.0, 3.0, 4.0],
                    following_curve_offset: 4,
                },
            ),
            (
                "strictly increasing",
                AclDecodedClip {
                    times: vec![0.0, 1.0],
                    binding_indices: vec![3, 2],
                    values: vec![1.0, 2.0, 3.0, 4.0],
                    following_curve_offset: 4,
                },
            ),
            (
                "non-finite curve value",
                AclDecodedClip {
                    times: vec![0.0, 1.0],
                    binding_indices: vec![2, 3],
                    values: vec![1.0, f32::NAN, 3.0, 4.0],
                    following_curve_offset: 4,
                },
            ),
            (
                "3 values",
                AclDecodedClip {
                    times: vec![0.0, 1.0],
                    binding_indices: vec![2, 3],
                    values: vec![1.0, 2.0, 3.0],
                    following_curve_offset: 4,
                },
            ),
            (
                "does not follow every decoded binding index",
                AclDecodedClip {
                    times: vec![0.0, 1.0],
                    binding_indices: vec![2, 3],
                    values: vec![1.0, 2.0, 3.0, 4.0],
                    following_curve_offset: 3,
                },
            ),
            (
                "for 2 declared curves",
                AclDecodedClip {
                    times: vec![0.0, 1.0],
                    binding_indices: vec![2, 3, 4],
                    values: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
                    following_curve_offset: 5,
                },
            ),
        ];
        for (reason, output) in cases {
            let error = clip
                .decode_with(
                    &FakeDecoder {
                        output: output.clone(),
                    },
                    AclDecodeLimits::default(),
                )
                .unwrap_err();
            assert!(
                error.to_string().contains(reason),
                "expected {reason:?}, got {error}"
            );
        }
    }

    /// The three resource ceilings, each tripped on its own so one cannot mask
    /// another.
    fn assert_rejected_limits(clip: &AclClip) {
        for (reason, limits) in [
            (
                "exceeds limit 1",
                AclDecodeLimits {
                    maximum_frames: 1,
                    ..AclDecodeLimits::default()
                },
            ),
            (
                "exceeding limit 1",
                AclDecodeLimits {
                    maximum_curves: 1,
                    ..AclDecodeLimits::default()
                },
            ),
            (
                "requires 4 values",
                AclDecodeLimits {
                    maximum_values: 3,
                    ..AclDecodeLimits::default()
                },
            ),
        ] {
            let error = clip.decode_with(&MustNotRunDecoder, limits).unwrap_err();
            assert!(
                error.to_string().contains(reason),
                "expected {reason:?}, got {error}"
            );
        }
    }
}
