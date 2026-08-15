//! Pure-Rust audio container inspection, decoding, and bounded WAV writing.
//!
//! This module handles existing RIFF/WAVE payloads, raw legacy PCM16, PCM
//! streams stored in an FSB5 sample bank, FMOD/Xbox IMA-ADPCM, Nintendo
//! DSP/GC-ADPCM, Sony VAG/PS-ADPCM and HEVAG, FMOD FADPCM, and mono/stereo
//! MPEG Layer II/III, FSB5 Vorbis, and mono/stereo FSB5 Opus. Other FMOD
//! codecs remain source-bound raw data until a verified pure-Rust decoder is
//! available.

use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};

use lewton::audio::{PreviousWindowRight, get_decoded_sample_count, read_audio_packet};
use lewton::header::{IdentHeader, SetupHeader, read_header_ident, read_header_setup};
use ruopus::{OpusDecoder, Packet as OpusPacket};
use symphonia::core::codecs::audio::well_known::{CODEC_ID_MP2, CODEC_ID_MP3};
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::packet::PacketRef;
use symphonia::core::units::{Duration, Timestamp};
use symphonia::default::codecs::MpaDecoder;

use crate::fsb_vorbis::setup_header;
use crate::source::Region;
use crate::{Error, Result};

const FSB5_VERSION_0_HEADER_BYTES: u64 = 0x40;
const FSB5_VERSION_1_HEADER_BYTES: u64 = 0x3c;
const WAV_HEADER_BYTES: u64 = 44;
const MPEG_FRAME_HEADER_BYTES: u64 = 4;
const MAX_MPEG_FRAME_BYTES: usize = 4096;
const MAX_MPEG_FRAME_SAMPLES: usize = 1152;
const MAX_MPEG_CHANNELS: usize = 2;
const FSB5_OPUS_ENCODER_DELAY: u64 = 312;
const MAX_OPUS_PACKET_BYTES: usize = 65_535;
const MAX_OPUS_CHANNELS: u16 = 2;
const FSB5_VORBIS_SHORT_BLOCK_EXPONENT: u8 = 8;
const FSB5_VORBIS_LONG_BLOCK_EXPONENT: u8 = 11;
const FSB5_VORBIS_MIN_PACKET_FRAMES: u64 = 128;
const MAX_VORBIS_CHANNELS: u16 = 8;
const MAX_VORBIS_PACKET_BYTES: usize = 65_535;
const MAX_VORBIS_PADDING_BYTES: u64 = 31;

/// PCM representation carried by a directly writable FSB5 sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcmSampleFormat {
    Unsigned8,
    Signed16,
    Signed24,
    Signed32,
    Float32,
}

impl PcmSampleFormat {
    const fn byte_width(self) -> u16 {
        match self {
            Self::Unsigned8 => 1,
            Self::Signed16 => 2,
            Self::Signed24 => 3,
            Self::Signed32 | Self::Float32 => 4,
        }
    }

    const fn wave_format(self) -> u16 {
        match self {
            Self::Float32 => 3,
            Self::Unsigned8 | Self::Signed16 | Self::Signed24 | Self::Signed32 => 1,
        }
    }
}

/// A validated first FSB5 subsound whose PCM bytes remain in the source region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fsb5PcmStream {
    pub data_offset: u64,
    pub data_length: u64,
    pub channels: u16,
    pub sample_rate: u32,
    pub sample_format: PcmSampleFormat,
    pub big_endian: bool,
    pub convert_float_to_pcm16: bool,
}

/// A validated FSB5 IMA-ADPCM stream that can be decoded to signed PCM16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fsb5ImaStream {
    pub data_offset: u64,
    pub compressed_length: u64,
    pub frame_count: u64,
    pub channels: u16,
    pub sample_rate: u32,
}

/// A validated FSB5 Nintendo DSP/GC-ADPCM stream that decodes to PCM16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fsb5DspStream {
    pub data_offset: u64,
    pub data_length: u64,
    pub compressed_length: u64,
    pub coefficients_offset: u64,
    pub coefficients_length: u64,
    pub frame_count: u64,
    pub channels: u16,
    pub sample_rate: u32,
    pub non_interleaved: bool,
}

/// A validated FSB5 Sony VAG/PS-ADPCM stream that decodes to PCM16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fsb5VagStream {
    pub data_offset: u64,
    pub data_length: u64,
    pub compressed_length: u64,
    pub frame_count: u64,
    pub channels: u16,
    pub sample_rate: u32,
    pub non_interleaved: bool,
}

/// A validated FSB5 Sony HEVAG stream that decodes to PCM16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fsb5HevagStream {
    pub data_offset: u64,
    pub data_length: u64,
    pub compressed_length: u64,
    pub frame_count: u64,
    pub channels: u16,
    pub sample_rate: u32,
}

/// A validated FSB5 FMOD FADPCM stream that decodes to PCM16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fsb5FadpcmStream {
    pub data_offset: u64,
    pub data_length: u64,
    pub compressed_length: u64,
    pub frame_count: u64,
    pub channels: u16,
    pub sample_rate: u32,
}

/// MPEG audio layer carried by an FSB5 stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fsb5MpegLayer {
    Layer2,
    Layer3,
}

/// A validated mono/stereo FSB5 MPEG stream that decodes to PCM16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fsb5MpegStream {
    pub data_offset: u64,
    pub data_length: u64,
    pub compressed_length: u64,
    pub frame_count: u64,
    pub channels: u16,
    pub sample_rate: u32,
    pub layer: Fsb5MpegLayer,
}

/// A validated mono/stereo FSB5 Opus stream that decodes to PCM16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fsb5OpusStream {
    pub data_offset: u64,
    pub data_length: u64,
    pub compressed_length: u64,
    pub frame_count: u64,
    pub channels: u16,
    pub sample_rate: u32,
    pub encoder_delay: u64,
}

/// A validated FSB5 Vorbis stream whose setup header is bundled by CRC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fsb5VorbisStream {
    pub data_offset: u64,
    pub data_length: u64,
    pub compressed_length: u64,
    pub frame_count: u64,
    pub channels: u16,
    pub sample_rate: u32,
    pub setup_crc: u32,
}

#[derive(Debug, Clone, Copy)]
struct Fsb5Header {
    version: u32,
    base_header_size: u64,
    sample_count: u64,
    sample_headers_size: u64,
    name_table_size: u64,
    sample_data_size: u64,
    codec: u32,
    flags: u32,
}

#[derive(Debug, Clone, Copy)]
struct Fsb5Sample {
    next_header_offset: u64,
    data_offset: u64,
    frame_count: u64,
    channels: u16,
    sample_rate: u32,
    dsp_coefficients: Option<(u64, u64)>,
    vorbis_setup_crc: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct Fsb5FirstStream {
    data_offset: u64,
    available_data: u64,
    frame_count: u64,
    channels: u16,
    sample_rate: u32,
    dsp_coefficients: Option<(u64, u64)>,
    vorbis_setup_crc: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct PcmWriteSource {
    data_offset: u64,
    data_length: u64,
    channels: u16,
    sample_rate: u32,
    sample_format: PcmSampleFormat,
    big_endian: bool,
    convert_float_to_pcm16: bool,
}

#[derive(Debug, Clone, Copy)]
struct PcmWaveLayout {
    output_format: PcmSampleFormat,
    output_width: u16,
    data_size: u32,
    output_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MpegFrameHeader {
    byte_length: u64,
    samples: u16,
    channels: u16,
    sample_rate: u32,
    layer: Fsb5MpegLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MpegScan {
    compressed_length: u64,
    layer: Fsb5MpegLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpusScan {
    compressed_length: u64,
    decoded_frames: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VorbisScan {
    compressed_length: u64,
    decoded_frames: u64,
}

/// WAV operation implemented entirely by the Rust core without a native codec library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectWavKind {
    /// The payload is already a RIFF/WAVE file and can be streamed unchanged.
    ExistingWave,
    /// Unity before 2.6 stores raw signed PCM16 with these stream parameters.
    LegacyPcm16 { channels: u16, sample_rate: u32 },
    /// The first subsound in a validated FSB5 bank is uncompressed PCM.
    Fsb5Pcm(Fsb5PcmStream),
    /// The first subsound uses FMOD/Xbox IMA-ADPCM and decodes to PCM16.
    Fsb5Ima(Fsb5ImaStream),
    /// The first subsound uses Nintendo DSP/GC-ADPCM and decodes to PCM16.
    Fsb5Dsp(Fsb5DspStream),
    /// The first subsound uses Sony VAG/PS-ADPCM and decodes to PCM16.
    Fsb5Vag(Fsb5VagStream),
    /// The first subsound uses Sony HEVAG and decodes to PCM16.
    Fsb5Hevag(Fsb5HevagStream),
    /// The first subsound uses FMOD FADPCM and decodes to PCM16.
    Fsb5Fadpcm(Fsb5FadpcmStream),
    /// The first subsound uses mono/stereo MPEG Layer II/III and decodes to PCM16.
    Fsb5Mpeg(Fsb5MpegStream),
    /// The first subsound uses mono/stereo Opus and decodes to PCM16.
    Fsb5Opus(Fsb5OpusStream),
    /// The first subsound uses Vorbis and decodes to PCM16.
    Fsb5Vorbis(Fsb5VorbisStream),
}

/// Returns the complete WAV byte count after validating the direct-write layout.
pub fn direct_wav_output_size(payload: &Region, kind: DirectWavKind) -> Result<u64> {
    match kind {
        DirectWavKind::ExistingWave => {
            if !is_riff_wave(payload)? {
                return Err(Error::invalid_data(
                    "direct WAV payload does not have a RIFF/WAVE header",
                ));
            }
            Ok(payload.len())
        }
        DirectWavKind::LegacyPcm16 {
            channels,
            sample_rate,
        } => pcm_wave_layout(
            payload,
            PcmWriteSource {
                data_offset: 0,
                data_length: payload.len(),
                channels,
                sample_rate,
                sample_format: PcmSampleFormat::Signed16,
                big_endian: false,
                convert_float_to_pcm16: false,
            },
        )
        .map(|layout| layout.output_size),
        DirectWavKind::Fsb5Pcm(stream) => pcm_wave_layout(
            payload,
            PcmWriteSource {
                data_offset: stream.data_offset,
                data_length: stream.data_length,
                channels: stream.channels,
                sample_rate: stream.sample_rate,
                sample_format: stream.sample_format,
                big_endian: stream.big_endian,
                convert_float_to_pcm16: stream.convert_float_to_pcm16,
            },
        )
        .map(|layout| layout.output_size),
        DirectWavKind::Fsb5Ima(stream) => ima_wave_output_size(payload, stream),
        DirectWavKind::Fsb5Dsp(stream) => dsp_wave_output_size(payload, stream),
        DirectWavKind::Fsb5Vag(stream) => vag_wave_output_size(payload, stream),
        DirectWavKind::Fsb5Hevag(stream) => hevag_wave_output_size(payload, stream),
        DirectWavKind::Fsb5Fadpcm(stream) => fadpcm_wave_output_size(payload, stream),
        DirectWavKind::Fsb5Mpeg(stream) => mpeg_wave_output_size(payload, stream),
        DirectWavKind::Fsb5Opus(stream) => opus_wave_output_size(payload, stream),
        DirectWavKind::Fsb5Vorbis(stream) => vorbis_wave_output_size(payload, stream),
    }
}

/// Detects a pure-Rust WAV path without materializing the payload.
///
/// FSB5 probing is conservative: malformed or unsupported banks return no WAV
/// path so callers can still preserve the original bytes. `decoded_bits` is
/// the Unity `AudioClip.m_BitsPerSample` preference used by the managed exporter
/// for PCM float (16 converts to signed PCM16; 32 remains IEEE float).
pub fn detect_direct_wav(
    payload: &Region,
    decoded_bits: Option<u16>,
) -> Result<Option<DirectWavKind>> {
    if is_riff_wave(payload)? {
        return Ok(Some(DirectWavKind::ExistingWave));
    }
    if !has_magic(payload, *b"FSB5")? {
        return Ok(None);
    }
    if let Some(stream) = parse_fsb5_pcm(payload, decoded_bits).ok().flatten() {
        return Ok(Some(DirectWavKind::Fsb5Pcm(stream)));
    }
    if let Some(stream) = parse_fsb5_ima(payload).ok().flatten() {
        return Ok(Some(DirectWavKind::Fsb5Ima(stream)));
    }
    if let Some(stream) = parse_fsb5_dsp(payload).ok().flatten() {
        return Ok(Some(DirectWavKind::Fsb5Dsp(stream)));
    }
    if let Some(stream) = parse_fsb5_vag(payload).ok().flatten() {
        return Ok(Some(DirectWavKind::Fsb5Vag(stream)));
    }
    if let Some(stream) = parse_fsb5_hevag(payload).ok().flatten() {
        return Ok(Some(DirectWavKind::Fsb5Hevag(stream)));
    }
    if let Some(stream) = parse_fsb5_fadpcm(payload).ok().flatten() {
        return Ok(Some(DirectWavKind::Fsb5Fadpcm(stream)));
    }
    if let Some(stream) = parse_fsb5_mpeg(payload).ok().flatten() {
        return Ok(Some(DirectWavKind::Fsb5Mpeg(stream)));
    }
    if let Some(stream) = parse_fsb5_opus(payload).ok().flatten() {
        return Ok(Some(DirectWavKind::Fsb5Opus(stream)));
    }
    Ok(parse_fsb5_vorbis(payload)
        .ok()
        .flatten()
        .map(DirectWavKind::Fsb5Vorbis))
}

/// Parses the first PCM subsound from an FSB5 bank.
///
/// Returns `Ok(None)` for compressed codecs or an unsupported PCM-float output
/// preference. Structural corruption is reported as invalid data.
pub fn parse_fsb5_pcm(
    payload: &Region,
    decoded_bits: Option<u16>,
) -> Result<Option<Fsb5PcmStream>> {
    if !has_magic(payload, *b"FSB5")? {
        return Ok(None);
    }
    let header = read_fsb5_header(payload)?;
    let sample_format = match header.codec {
        1 => PcmSampleFormat::Unsigned8,
        2 => PcmSampleFormat::Signed16,
        3 => PcmSampleFormat::Signed24,
        4 => PcmSampleFormat::Signed32,
        5 => PcmSampleFormat::Float32,
        _ => return Ok(None),
    };
    let convert_float_to_pcm16 = match (sample_format, decoded_bits) {
        (PcmSampleFormat::Float32, Some(16)) => true,
        (PcmSampleFormat::Float32, Some(32) | None) => false,
        (PcmSampleFormat::Float32, Some(_)) => return Ok(None),
        _ => false,
    };
    let first = read_fsb5_first_stream(payload, header)?;
    let frame_width = u64::from(first.channels)
        .checked_mul(u64::from(sample_format.byte_width()))
        .ok_or_else(|| Error::invalid_data("FSB5 PCM frame size overflowed"))?;
    let data_length = first
        .frame_count
        .checked_mul(frame_width)
        .ok_or_else(|| Error::invalid_data("FSB5 PCM byte count overflowed"))?;
    if data_length > first.available_data {
        return Err(Error::invalid_data(format!(
            "FSB5 PCM data requires {data_length} bytes but the first subsound has {}",
            first.available_data
        )));
    }
    let big_endian = header.version == 1 && header.flags & 1 != 0;
    Ok(Some(Fsb5PcmStream {
        data_offset: first.data_offset,
        data_length,
        channels: first.channels,
        sample_rate: first.sample_rate,
        sample_format,
        big_endian,
        convert_float_to_pcm16,
    }))
}

/// Parses the first FMOD/Xbox IMA-ADPCM subsound from an FSB5 bank.
pub fn parse_fsb5_ima(payload: &Region) -> Result<Option<Fsb5ImaStream>> {
    if !has_magic(payload, *b"FSB5")? {
        return Ok(None);
    }
    let header = read_fsb5_header(payload)?;
    if header.codec != 7 {
        return Ok(None);
    }
    let first = read_fsb5_first_stream(payload, header)?;
    if first.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 IMA-ADPCM subsound contains no sample frames",
        ));
    }
    let block_size = u64::from(first.channels)
        .checked_mul(36)
        .ok_or_else(|| Error::invalid_data("FSB5 IMA block size overflowed"))?;
    let block_count = first
        .frame_count
        .checked_add(63)
        .ok_or_else(|| Error::invalid_data("FSB5 IMA frame count overflowed"))?
        / 64;
    let compressed_length = block_count
        .checked_mul(block_size)
        .ok_or_else(|| Error::invalid_data("FSB5 IMA byte count overflowed"))?;
    if compressed_length > first.available_data {
        return Err(Error::invalid_data(format!(
            "FSB5 IMA data requires {compressed_length} bytes but the first subsound has {}",
            first.available_data
        )));
    }
    Ok(Some(Fsb5ImaStream {
        data_offset: first.data_offset,
        compressed_length,
        frame_count: first.frame_count,
        channels: first.channels,
        sample_rate: first.sample_rate,
    }))
}

/// Parses the first Nintendo DSP/GC-ADPCM subsound from an FSB5 bank.
pub fn parse_fsb5_dsp(payload: &Region) -> Result<Option<Fsb5DspStream>> {
    if !has_magic(payload, *b"FSB5")? {
        return Ok(None);
    }
    let header = read_fsb5_header(payload)?;
    if header.codec != 6 {
        return Ok(None);
    }
    let first = read_fsb5_first_stream(payload, header)?;
    if first.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 DSP-ADPCM subsound contains no sample frames",
        ));
    }
    let (coefficients_offset, coefficients_length) = first.dsp_coefficients.ok_or_else(|| {
        Error::invalid_data("FSB5 DSP-ADPCM subsound has no coefficient metadata")
    })?;
    let minimum_coefficients = u64::from(first.channels)
        .checked_mul(0x2e)
        .ok_or_else(|| Error::invalid_data("FSB5 DSP coefficient size overflowed"))?;
    if coefficients_length < minimum_coefficients {
        return Err(Error::invalid_data(format!(
            "FSB5 DSP coefficient metadata has {coefficients_length} bytes but {minimum_coefficients} are required"
        )));
    }
    let encoded_frames = first.frame_count.div_ceil(14);
    let compressed_length = encoded_frames
        .checked_mul(8)
        .and_then(|value| value.checked_mul(u64::from(first.channels)))
        .ok_or_else(|| Error::invalid_data("FSB5 DSP encoded byte count overflowed"))?;
    if compressed_length > first.available_data {
        return Err(Error::invalid_data(format!(
            "FSB5 DSP data requires {compressed_length} bytes but the first subsound has {}",
            first.available_data
        )));
    }
    let non_interleaved = header.flags & 0x02 != 0;
    if non_interleaved {
        let stride = first.available_data / u64::from(first.channels);
        let channel_bytes = encoded_frames * 8;
        if stride < channel_bytes {
            return Err(Error::invalid_data(
                "FSB5 non-interleaved DSP channel stride is shorter than its encoded frames",
            ));
        }
    }
    Ok(Some(Fsb5DspStream {
        data_offset: first.data_offset,
        data_length: first.available_data,
        compressed_length,
        coefficients_offset,
        coefficients_length,
        frame_count: first.frame_count,
        channels: first.channels,
        sample_rate: first.sample_rate,
        non_interleaved,
    }))
}

/// Parses the first Sony VAG/PS-ADPCM subsound from an FSB5 bank.
pub fn parse_fsb5_vag(payload: &Region) -> Result<Option<Fsb5VagStream>> {
    if !has_magic(payload, *b"FSB5")? {
        return Ok(None);
    }
    let header = read_fsb5_header(payload)?;
    if header.codec != 8 {
        return Ok(None);
    }
    let first = read_fsb5_first_stream(payload, header)?;
    if first.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 VAG subsound contains no sample frames",
        ));
    }
    let encoded_frames = first.frame_count.div_ceil(28);
    let compressed_length = encoded_frames
        .checked_mul(16)
        .and_then(|value| value.checked_mul(u64::from(first.channels)))
        .ok_or_else(|| Error::invalid_data("FSB5 VAG encoded byte count overflowed"))?;
    if compressed_length > first.available_data {
        return Err(Error::invalid_data(format!(
            "FSB5 VAG data requires {compressed_length} bytes but the first subsound has {}",
            first.available_data
        )));
    }
    let non_interleaved = header.flags & 0x02 != 0;
    if non_interleaved {
        let stride = first.available_data / u64::from(first.channels);
        let channel_bytes = encoded_frames * 16;
        if stride < channel_bytes {
            return Err(Error::invalid_data(
                "FSB5 non-interleaved VAG channel stride is shorter than its encoded frames",
            ));
        }
    }
    Ok(Some(Fsb5VagStream {
        data_offset: first.data_offset,
        data_length: first.available_data,
        compressed_length,
        frame_count: first.frame_count,
        channels: first.channels,
        sample_rate: first.sample_rate,
        non_interleaved,
    }))
}

/// Parses the first Sony HEVAG subsound from an FSB5 bank.
pub fn parse_fsb5_hevag(payload: &Region) -> Result<Option<Fsb5HevagStream>> {
    if !has_magic(payload, *b"FSB5")? {
        return Ok(None);
    }
    let header = read_fsb5_header(payload)?;
    if header.codec != 9 {
        return Ok(None);
    }
    let first = read_fsb5_first_stream(payload, header)?;
    if first.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 HEVAG subsound contains no sample frames",
        ));
    }
    let compressed_length = first
        .frame_count
        .div_ceil(28)
        .checked_mul(16)
        .and_then(|value| value.checked_mul(u64::from(first.channels)))
        .ok_or_else(|| Error::invalid_data("FSB5 HEVAG encoded byte count overflowed"))?;
    if compressed_length > first.available_data {
        return Err(Error::invalid_data(format!(
            "FSB5 HEVAG data requires {compressed_length} bytes but the first subsound has {}",
            first.available_data
        )));
    }
    Ok(Some(Fsb5HevagStream {
        data_offset: first.data_offset,
        data_length: first.available_data,
        compressed_length,
        frame_count: first.frame_count,
        channels: first.channels,
        sample_rate: first.sample_rate,
    }))
}

/// Parses the first FMOD FADPCM subsound from an FSB5 bank.
pub fn parse_fsb5_fadpcm(payload: &Region) -> Result<Option<Fsb5FadpcmStream>> {
    if !has_magic(payload, *b"FSB5")? {
        return Ok(None);
    }
    let header = read_fsb5_header(payload)?;
    if header.codec != 16 {
        return Ok(None);
    }
    let first = read_fsb5_first_stream(payload, header)?;
    if first.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 FADPCM subsound contains no sample frames",
        ));
    }
    let compressed_length = first
        .frame_count
        .div_ceil(256)
        .checked_mul(0x8c)
        .and_then(|value| value.checked_mul(u64::from(first.channels)))
        .ok_or_else(|| Error::invalid_data("FSB5 FADPCM encoded byte count overflowed"))?;
    if compressed_length > first.available_data {
        return Err(Error::invalid_data(format!(
            "FSB5 FADPCM data requires {compressed_length} bytes but the first subsound has {}",
            first.available_data
        )));
    }
    Ok(Some(Fsb5FadpcmStream {
        data_offset: first.data_offset,
        data_length: first.available_data,
        compressed_length,
        frame_count: first.frame_count,
        channels: first.channels,
        sample_rate: first.sample_rate,
    }))
}

/// Parses the first mono/stereo MPEG Layer II/III subsound from an FSB5 bank.
///
/// FMOD pads every MPEG frame to a four-byte boundary inside FSB5. The parser
/// validates the complete declared sample range and records the exact padded
/// source span; decoding later re-reads only one bounded frame at a time.
pub fn parse_fsb5_mpeg(payload: &Region) -> Result<Option<Fsb5MpegStream>> {
    if !has_magic(payload, *b"FSB5")? {
        return Ok(None);
    }
    let header = read_fsb5_header(payload)?;
    if header.codec != 11 {
        return Ok(None);
    }
    let first = read_fsb5_first_stream(payload, header)?;
    if first.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 MPEG subsound contains no sample frames",
        ));
    }
    if first.channels > 2 {
        return Err(Error::unsupported(format!(
            "FSB5 MPEG with {} channels uses multiple interleaved MPEG streams",
            first.channels
        )));
    }
    let scan = scan_mpeg_frames(
        payload,
        first.data_offset,
        first.available_data,
        first.frame_count,
        first.channels,
        first.sample_rate,
    )?;
    Ok(Some(Fsb5MpegStream {
        data_offset: first.data_offset,
        data_length: first.available_data,
        compressed_length: scan.compressed_length,
        frame_count: first.frame_count,
        channels: first.channels,
        sample_rate: first.sample_rate,
        layer: scan.layer,
    }))
}

/// Parses the first mono/stereo Opus subsound from an FSB5 bank.
///
/// FSB stores each raw Opus packet behind a little-endian 16-bit byte count.
/// FMOD Opus banks use a fixed 48 kHz timeline and discard 312 encoder-delay
/// frames before exposing the declared sample count.
pub fn parse_fsb5_opus(payload: &Region) -> Result<Option<Fsb5OpusStream>> {
    if !has_magic(payload, *b"FSB5")? {
        return Ok(None);
    }
    let header = read_fsb5_header(payload)?;
    if header.codec != 17 {
        return Ok(None);
    }
    let first = read_fsb5_first_stream(payload, header)?;
    if first.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 Opus subsound contains no sample frames",
        ));
    }
    if first.channels > MAX_OPUS_CHANNELS {
        return Err(Error::unsupported(format!(
            "FSB5 Opus with {} channels requires a multistream mapping",
            first.channels
        )));
    }
    if first.sample_rate != 48_000 {
        return Err(Error::unsupported(format!(
            "FSB5 Opus sample rate {} is not the verified 48000 Hz layout",
            first.sample_rate
        )));
    }
    let scan = scan_opus_packets(
        payload,
        first.data_offset,
        first.available_data,
        first.frame_count,
        FSB5_OPUS_ENCODER_DELAY,
    )?;
    Ok(Some(Fsb5OpusStream {
        data_offset: first.data_offset,
        data_length: first.available_data,
        compressed_length: scan.compressed_length,
        frame_count: first.frame_count,
        channels: first.channels,
        sample_rate: first.sample_rate,
        encoder_delay: FSB5_OPUS_ENCODER_DELAY,
    }))
}

/// Parses the first FSB5 Vorbis subsound using its externally referenced setup header.
pub fn parse_fsb5_vorbis(payload: &Region) -> Result<Option<Fsb5VorbisStream>> {
    if !has_magic(payload, *b"FSB5")? {
        return Ok(None);
    }
    let header = read_fsb5_header(payload)?;
    if header.codec != 15 {
        return Ok(None);
    }
    let first = read_fsb5_first_stream(payload, header)?;
    if first.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 Vorbis subsound contains no sample frames",
        ));
    }
    if first.channels == 0 || first.channels > MAX_VORBIS_CHANNELS {
        return Err(Error::unsupported(format!(
            "FSB5 Vorbis channel count {} is outside the verified 1..={MAX_VORBIS_CHANNELS} range",
            first.channels
        )));
    }
    let setup_crc = first.vorbis_setup_crc.ok_or_else(|| {
        Error::invalid_data("FSB5 Vorbis subsound has no setup-header CRC metadata")
    })?;
    let (identification, setup) = vorbis_headers(first.channels, first.sample_rate, setup_crc)?;
    let scan = scan_vorbis_packets(
        payload,
        first.data_offset,
        first.available_data,
        first.frame_count,
        &identification,
        &setup,
    )?;
    if scan.decoded_frames < first.frame_count {
        return Err(Error::invalid_data(format!(
            "FSB5 Vorbis packets decode to {} frames but the subsound declares {}",
            scan.decoded_frames, first.frame_count
        )));
    }
    Ok(Some(Fsb5VorbisStream {
        data_offset: first.data_offset,
        data_length: first.available_data,
        compressed_length: scan.compressed_length,
        frame_count: first.frame_count,
        channels: first.channels,
        sample_rate: first.sample_rate,
        setup_crc,
    }))
}

fn read_fsb5_first_stream(payload: &Region, header: Fsb5Header) -> Result<Fsb5FirstStream> {
    let header_end = header
        .base_header_size
        .checked_add(header.sample_headers_size)
        .ok_or_else(|| Error::invalid_data("FSB5 sample-header range overflowed"))?;
    let data_base = header_end
        .checked_add(header.name_table_size)
        .ok_or_else(|| Error::invalid_data("FSB5 data offset overflowed"))?;
    let mut cursor = header.base_header_size;
    let mut previous_data_offset = None;
    let mut second_data_offset = None;
    let mut first = None;

    // Managed export selects the first subsound. Reading at most the first two
    // headers is sufficient to establish its exact data boundary and prevents
    // banks with millions of irrelevant subsounds from amplifying CPU work.
    for sample_index in 0..header.sample_count.min(2) {
        let sample = read_fsb5_sample(payload, cursor, header_end)?;
        cursor = sample.next_header_offset;
        if sample.data_offset > header.sample_data_size {
            return Err(Error::invalid_data(
                "FSB5 subsound starts outside the sample-data section",
            ));
        }
        if let Some(previous) = previous_data_offset
            && sample.data_offset <= previous
        {
            return Err(Error::invalid_data(
                "FSB5 subsound data offsets are not strictly increasing",
            ));
        }
        previous_data_offset = Some(sample.data_offset);
        if sample_index == 0 {
            first = Some(sample);
        } else if sample_index == 1 {
            second_data_offset = Some(sample.data_offset);
        }
    }
    let first = first.ok_or_else(|| Error::invalid_data("FSB5 first subsound is missing"))?;
    let first_end = second_data_offset.unwrap_or(header.sample_data_size);
    if first_end < first.data_offset {
        return Err(Error::invalid_data("FSB5 first subsound range is reversed"));
    }
    let data_offset = data_base
        .checked_add(first.data_offset)
        .ok_or_else(|| Error::invalid_data("FSB5 PCM source offset overflowed"))?;
    Ok(Fsb5FirstStream {
        data_offset,
        available_data: first_end - first.data_offset,
        frame_count: first.frame_count,
        channels: first.channels,
        sample_rate: first.sample_rate,
        dsp_coefficients: first.dsp_coefficients,
        vorbis_setup_crc: first.vorbis_setup_crc,
    })
}

fn read_fsb5_header(payload: &Region) -> Result<Fsb5Header> {
    if payload.len() < 0x1c {
        return Err(Error::invalid_data("FSB5 header is truncated"));
    }
    let version = read_u32_le(payload, 0x04)?;
    let base_header_size = match version {
        0 => FSB5_VERSION_0_HEADER_BYTES,
        1 => FSB5_VERSION_1_HEADER_BYTES,
        _ => {
            return Err(Error::unsupported(format!(
                "FSB5 header version {version} is not supported"
            )));
        }
    };
    let sample_count = u64::from(read_u32_le(payload, 0x08)?);
    if sample_count == 0 {
        return Err(Error::invalid_data("FSB5 contains no subsounds"));
    }
    let sample_headers_size = u64::from(read_u32_le(payload, 0x0c)?);
    let name_table_size = u64::from(read_u32_le(payload, 0x10)?);
    let sample_data_size = u64::from(read_u32_le(payload, 0x14)?);
    let codec = read_u32_le(payload, 0x18)?;
    let flags = if version == 1 {
        read_u32_le(payload, 0x20)?
    } else {
        0
    };
    let expected_file_size = base_header_size
        .checked_add(sample_headers_size)
        .and_then(|value| value.checked_add(name_table_size))
        .and_then(|value| value.checked_add(sample_data_size))
        .ok_or_else(|| Error::invalid_data("FSB5 declared size overflowed"))?;
    if expected_file_size != payload.len() {
        return Err(Error::invalid_data(format!(
            "FSB5 declared size {expected_file_size} does not match payload size {}",
            payload.len()
        )));
    }
    let minimum_sample_headers = sample_count
        .checked_mul(8)
        .ok_or_else(|| Error::invalid_data("FSB5 sample header count overflowed"))?;
    if minimum_sample_headers > sample_headers_size {
        return Err(Error::invalid_data(
            "FSB5 sample count exceeds the declared sample-header table",
        ));
    }
    Ok(Fsb5Header {
        version,
        base_header_size,
        sample_count,
        sample_headers_size,
        name_table_size,
        sample_data_size,
        codec,
        flags,
    })
}

fn read_fsb5_sample(payload: &Region, offset: u64, header_end: u64) -> Result<Fsb5Sample> {
    let sample_mode = read_u64_le_bounded(payload, offset, header_end, "FSB5 sample mode")?;
    let mut cursor = offset + 8;
    let mut channels = match (sample_mode >> 5) & 0x03 {
        0 => 1_u16,
        1 => 2,
        2 => 6,
        3 => 8,
        _ => unreachable!(),
    };
    let frequency_code =
        u8::try_from((sample_mode >> 1) & 0x0f).expect("four-bit FSB5 frequency code fits in u8");
    let mut sample_rate = compact_sample_rate(frequency_code);
    let mut dsp_coefficients = None;
    let mut vorbis_setup_crc = None;
    let mut has_chunk = sample_mode & 1 != 0;
    while has_chunk {
        let chunk_header =
            read_u32_le_bounded(payload, cursor, header_end, "FSB5 metadata header")?;
        cursor += 4;
        has_chunk = chunk_header & 1 != 0;
        let chunk_size = u64::from((chunk_header >> 1) & 0x00ff_ffff);
        let chunk_end = cursor
            .checked_add(chunk_size)
            .ok_or_else(|| Error::invalid_data("FSB5 metadata range overflowed"))?;
        if chunk_end > header_end {
            return Err(Error::invalid_data(
                "FSB5 metadata exceeds the sample-header table",
            ));
        }
        match chunk_header >> 25 {
            1 => channels = read_fsb5_channels(payload, cursor, chunk_size)?,
            2 => sample_rate = Some(read_fsb5_sample_rate(payload, cursor, chunk_size)?),
            7 => dsp_coefficients = Some((cursor, chunk_size)),
            11 => {
                if chunk_size < 4 {
                    return Err(Error::invalid_data(
                        "FSB5 Vorbis metadata is shorter than its setup CRC",
                    ));
                }
                vorbis_setup_crc = Some(read_u32_le(payload, cursor)?);
            }
            14 => {
                if chunk_size < 4 {
                    return Err(Error::invalid_data(
                        "FSB5 Vorbis layer metadata is truncated",
                    ));
                }
                let layers = read_u32_le(payload, cursor)?;
                let expanded = u32::from(channels)
                    .checked_mul(layers)
                    .ok_or_else(|| Error::invalid_data("FSB5 Vorbis layer count overflowed"))?;
                channels = u16::try_from(expanded).map_err(|_| {
                    Error::invalid_data("FSB5 Vorbis layer channel count exceeds u16")
                })?;
                if channels == 0 {
                    return Err(Error::invalid_data(
                        "FSB5 Vorbis layer count cannot be zero",
                    ));
                }
            }
            _ => {}
        }
        cursor = chunk_end;
    }
    let sample_rate = sample_rate
        .filter(|value| *value > 0)
        .ok_or_else(|| Error::invalid_data("FSB5 sample rate is invalid"))?;
    let data_offset = ((sample_mode >> 7) & 0x07ff_ffff)
        .checked_mul(32)
        .ok_or_else(|| Error::invalid_data("FSB5 sample data offset overflowed"))?;
    Ok(Fsb5Sample {
        next_header_offset: cursor,
        data_offset,
        frame_count: (sample_mode >> 34) & 0x3fff_ffff,
        channels,
        sample_rate,
        dsp_coefficients,
        vorbis_setup_crc,
    })
}

fn read_fsb5_channels(payload: &Region, offset: u64, chunk_size: u64) -> Result<u16> {
    if chunk_size < 1 {
        return Err(Error::invalid_data("FSB5 channel metadata is empty"));
    }
    let channels = u16::from(read_u8(payload, offset)?);
    if channels == 0 {
        return Err(Error::invalid_data("FSB5 channel count cannot be zero"));
    }
    Ok(channels)
}

fn read_fsb5_sample_rate(payload: &Region, offset: u64, chunk_size: u64) -> Result<u32> {
    if chunk_size < 4 {
        return Err(Error::invalid_data(
            "FSB5 sample-rate metadata is truncated",
        ));
    }
    read_u32_le(payload, offset)
}

/// Streams or decodes a pure-Rust WAV payload and returns the complete byte count.
pub fn write_direct_wav(
    payload: &Region,
    kind: DirectWavKind,
    maximum_output_bytes: u64,
    output: &mut impl Write,
) -> Result<u64> {
    match kind {
        DirectWavKind::ExistingWave => write_existing_wave(payload, maximum_output_bytes, output),
        DirectWavKind::LegacyPcm16 {
            channels,
            sample_rate,
        } => write_pcm_wav(
            payload,
            PcmWriteSource {
                data_offset: 0,
                data_length: payload.len(),
                channels,
                sample_rate,
                sample_format: PcmSampleFormat::Signed16,
                big_endian: false,
                convert_float_to_pcm16: false,
            },
            maximum_output_bytes,
            output,
        ),
        DirectWavKind::Fsb5Pcm(stream) => write_pcm_wav(
            payload,
            PcmWriteSource {
                data_offset: stream.data_offset,
                data_length: stream.data_length,
                channels: stream.channels,
                sample_rate: stream.sample_rate,
                sample_format: stream.sample_format,
                big_endian: stream.big_endian,
                convert_float_to_pcm16: stream.convert_float_to_pcm16,
            },
            maximum_output_bytes,
            output,
        ),
        DirectWavKind::Fsb5Ima(stream) => {
            write_fsb5_ima_wav(payload, stream, maximum_output_bytes, output)
        }
        DirectWavKind::Fsb5Dsp(stream) => {
            write_fsb5_dsp_wav(payload, stream, maximum_output_bytes, output)
        }
        DirectWavKind::Fsb5Vag(stream) => {
            write_fsb5_vag_wav(payload, stream, maximum_output_bytes, output)
        }
        DirectWavKind::Fsb5Hevag(stream) => {
            write_fsb5_hevag_wav(payload, stream, maximum_output_bytes, output)
        }
        DirectWavKind::Fsb5Fadpcm(stream) => {
            write_fsb5_fadpcm_wav(payload, stream, maximum_output_bytes, output)
        }
        DirectWavKind::Fsb5Mpeg(stream) => {
            write_fsb5_mpeg_wav(payload, stream, maximum_output_bytes, output)
        }
        DirectWavKind::Fsb5Opus(stream) => {
            write_fsb5_opus_wav(payload, stream, maximum_output_bytes, output)
        }
        DirectWavKind::Fsb5Vorbis(stream) => {
            write_fsb5_vorbis_wav(payload, stream, maximum_output_bytes, output)
        }
    }
}

fn write_existing_wave(
    payload: &Region,
    maximum_output_bytes: u64,
    output: &mut impl Write,
) -> Result<u64> {
    if !is_riff_wave(payload)? {
        return Err(Error::invalid_data(
            "direct WAV payload does not have a RIFF/WAVE header",
        ));
    }
    if payload.len() > maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "WAV payload is {} bytes, exceeding limit {maximum_output_bytes}",
            payload.len()
        )));
    }
    payload.copy_range(0, payload.len(), output)
}

fn ima_wave_output_size(payload: &Region, stream: Fsb5ImaStream) -> Result<u64> {
    validate_ima_source(payload, stream)?;
    let data_length = stream
        .frame_count
        .checked_mul(u64::from(stream.channels))
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| Error::invalid_data("FSB5 IMA decoded byte count overflowed"))?;
    let data_size = u32::try_from(data_length)
        .map_err(|_| Error::invalid_data("WAV PCM payload exceeds its 32-bit data chunk"))?;
    WAV_HEADER_BYTES
        .checked_add(u64::from(data_size))
        .ok_or_else(|| Error::invalid_data("WAV output size overflowed"))
}

fn validate_ima_source(payload: &Region, stream: Fsb5ImaStream) -> Result<()> {
    if stream.channels == 0 || stream.sample_rate == 0 || stream.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 IMA channels, sample rate, and frame count must be nonzero",
        ));
    }
    if stream.channels > 255 {
        return Err(Error::invalid_data(
            "FSB5 IMA channel count exceeds the format metadata width",
        ));
    }
    let source_end = stream
        .data_offset
        .checked_add(stream.compressed_length)
        .ok_or_else(|| Error::invalid_data("FSB5 IMA source range overflowed"))?;
    if source_end > payload.len() {
        return Err(Error::invalid_data(format!(
            "FSB5 IMA source range {}..{source_end} exceeds payload length {}",
            stream.data_offset,
            payload.len()
        )));
    }
    let block_size = u64::from(stream.channels)
        .checked_mul(36)
        .ok_or_else(|| Error::invalid_data("FSB5 IMA block size overflowed"))?;
    let block_count = stream.frame_count.div_ceil(64);
    let required = block_count
        .checked_mul(block_size)
        .ok_or_else(|| Error::invalid_data("FSB5 IMA byte count overflowed"))?;
    if required != stream.compressed_length {
        return Err(Error::invalid_data(format!(
            "FSB5 IMA source declares {} bytes but {required} are required",
            stream.compressed_length
        )));
    }
    Ok(())
}

fn write_fsb5_ima_wav(
    payload: &Region,
    stream: Fsb5ImaStream,
    maximum_output_bytes: u64,
    output: &mut impl Write,
) -> Result<u64> {
    let output_size = ima_wave_output_size(payload, stream)?;
    if output_size > maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "WAV output is {output_size} bytes, exceeding limit {maximum_output_bytes}"
        )));
    }
    let data_size = u32::try_from(output_size - WAV_HEADER_BYTES)
        .expect("IMA WAV output size validation bounded the data chunk");
    write_wav_header(
        output,
        data_size,
        PcmSampleFormat::Signed16.wave_format(),
        stream.channels,
        stream.sample_rate,
        16,
    )?;
    decode_fsb5_ima(payload, stream, output)?;
    Ok(output_size)
}

fn decode_fsb5_ima(payload: &Region, stream: Fsb5ImaStream, output: &mut impl Write) -> Result<()> {
    const MAX_CHANNELS: usize = 255;
    let channels = usize::from(stream.channels);
    let block_size = u64::from(stream.channels) * 36;
    let mut histories = [0_i32; MAX_CHANNELS];
    let mut step_indices = [0_i32; MAX_CHANNELS];
    let mut frame_bytes = [0_u8; MAX_CHANNELS * 2];
    let mut remaining_frames = stream.frame_count;
    let mut block_offset = stream.data_offset;

    while remaining_frames > 0 {
        read_ima_block_headers(
            payload,
            block_offset,
            channels,
            &mut histories,
            &mut step_indices,
        )?;
        let block_frames = remaining_frames.min(64);
        write_ima_frame(&histories[..channels], &mut frame_bytes, output)?;
        for frame_index in 1..block_frames {
            for channel in 0..channels {
                let byte_offset =
                    ima_nibble_byte_offset(block_offset, channels, channel, frame_index)?;
                let byte = read_u8(payload, byte_offset)?;
                let nibble = if (frame_index - 1) & 1 == 0 {
                    byte & 0x0f
                } else {
                    byte >> 4
                };
                expand_ima_nibble(nibble, &mut histories[channel], &mut step_indices[channel]);
            }
            write_ima_frame(&histories[..channels], &mut frame_bytes, output)?;
        }
        remaining_frames -= block_frames;
        block_offset = block_offset
            .checked_add(block_size)
            .ok_or_else(|| Error::invalid_data("FSB5 IMA block offset overflowed"))?;
    }
    Ok(())
}

fn read_ima_block_headers(
    payload: &Region,
    block_offset: u64,
    channels: usize,
    histories: &mut [i32; 255],
    step_indices: &mut [i32; 255],
) -> Result<()> {
    let channel_bytes = u64::try_from(channels).expect("FSB5 channel count fits u64");
    for channel in 0..channels {
        let channel_offset = u64::try_from(channel).expect("channel index fits u64");
        let (history_offset, step_offset) = if channels <= 2 {
            let header = block_offset
                .checked_add(channel_offset * 4)
                .ok_or_else(|| Error::invalid_data("FSB5 IMA header offset overflowed"))?;
            (header, header + 2)
        } else {
            (
                block_offset + channel_offset * 2,
                block_offset + channel_bytes * 2 + channel_offset * 2,
            )
        };
        histories[channel] = i32::from(read_i16_le(payload, history_offset)?);
        let index = i32::from(i8::from_ne_bytes([read_u8(payload, step_offset)?]));
        step_indices[channel] = index.clamp(0, 88);
    }
    Ok(())
}

fn ima_nibble_byte_offset(
    block_offset: u64,
    channels: usize,
    channel: usize,
    frame_index: u64,
) -> Result<u64> {
    let channels = u64::try_from(channels).expect("FSB5 channel count fits u64");
    let channel = u64::try_from(channel).expect("channel index fits u64");
    let nibble_index = frame_index - 1;
    let relative = if channels == 1 {
        4 + nibble_index / 2
    } else if channels == 2 {
        8 + channel * 4 + (nibble_index / 8) * 8 + (nibble_index % 8) / 2
    } else {
        4 * channels + 2 * channel + (nibble_index / 4) * 2 * channels + (nibble_index % 4) / 2
    };
    block_offset
        .checked_add(relative)
        .ok_or_else(|| Error::invalid_data("FSB5 IMA nibble offset overflowed"))
}

fn write_ima_frame(
    histories: &[i32],
    buffer: &mut [u8; 510],
    output: &mut impl Write,
) -> Result<()> {
    let byte_length = histories
        .len()
        .checked_mul(2)
        .ok_or_else(|| Error::invalid_data("FSB5 IMA frame byte count overflowed"))?;
    for (history, destination) in histories
        .iter()
        .zip(buffer[..byte_length].chunks_exact_mut(2))
    {
        destination.copy_from_slice(
            &i16::try_from(*history)
                .unwrap_or(if *history < 0 { i16::MIN } else { i16::MAX })
                .to_le_bytes(),
        );
    }
    output.write_all(&buffer[..byte_length])?;
    Ok(())
}

fn expand_ima_nibble(nibble: u8, history: &mut i32, step_index: &mut i32) {
    const STEP_TABLE: [i32; 89] = [
        7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60,
        66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371,
        408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878,
        2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845,
        8630, 9493, 10_442, 11_487, 12_635, 13_899, 15_289, 16_818, 18_500, 20_350, 22_385, 24_623,
        27_086, 29_794, 32_767,
    ];
    const INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
    let step = STEP_TABLE[usize::try_from(*step_index).expect("IMA index is clamped")];
    let mut delta = step >> 3;
    if nibble & 1 != 0 {
        delta += step >> 2;
    }
    if nibble & 2 != 0 {
        delta += step >> 1;
    }
    if nibble & 4 != 0 {
        delta += step;
    }
    if nibble & 8 != 0 {
        delta = -delta;
    }
    *history = (*history + delta).clamp(i32::from(i16::MIN), i32::from(i16::MAX));
    *step_index = (*step_index + INDEX_TABLE[usize::from(nibble)]).clamp(0, 88);
}

fn dsp_wave_output_size(payload: &Region, stream: Fsb5DspStream) -> Result<u64> {
    validate_dsp_source(payload, stream)?;
    let data_length = stream
        .frame_count
        .checked_mul(u64::from(stream.channels))
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| Error::invalid_data("FSB5 DSP decoded byte count overflowed"))?;
    let data_size = u32::try_from(data_length)
        .map_err(|_| Error::invalid_data("WAV PCM payload exceeds its 32-bit data chunk"))?;
    WAV_HEADER_BYTES
        .checked_add(u64::from(data_size))
        .ok_or_else(|| Error::invalid_data("WAV output size overflowed"))
}

fn validate_dsp_source(payload: &Region, stream: Fsb5DspStream) -> Result<()> {
    if stream.channels == 0 || stream.sample_rate == 0 || stream.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 DSP channels, sample rate, and frame count must be nonzero",
        ));
    }
    if stream.channels > 255 {
        return Err(Error::invalid_data(
            "FSB5 DSP channel count exceeds the format metadata width",
        ));
    }
    let data_end = stream
        .data_offset
        .checked_add(stream.data_length)
        .ok_or_else(|| Error::invalid_data("FSB5 DSP source range overflowed"))?;
    if data_end > payload.len() {
        return Err(Error::invalid_data(format!(
            "FSB5 DSP source range {}..{data_end} exceeds payload length {}",
            stream.data_offset,
            payload.len()
        )));
    }
    let coefficient_end = stream
        .coefficients_offset
        .checked_add(stream.coefficients_length)
        .ok_or_else(|| Error::invalid_data("FSB5 DSP coefficient range overflowed"))?;
    if coefficient_end > payload.len() {
        return Err(Error::invalid_data(
            "FSB5 DSP coefficient metadata exceeds the payload",
        ));
    }
    let minimum_coefficients = u64::from(stream.channels)
        .checked_mul(0x2e)
        .ok_or_else(|| Error::invalid_data("FSB5 DSP coefficient size overflowed"))?;
    if stream.coefficients_length < minimum_coefficients {
        return Err(Error::invalid_data(
            "FSB5 DSP coefficient metadata is shorter than the channel table",
        ));
    }
    let channel_bytes = stream
        .frame_count
        .div_ceil(14)
        .checked_mul(8)
        .ok_or_else(|| Error::invalid_data("FSB5 DSP channel byte count overflowed"))?;
    let required = channel_bytes
        .checked_mul(u64::from(stream.channels))
        .ok_or_else(|| Error::invalid_data("FSB5 DSP encoded byte count overflowed"))?;
    if required != stream.compressed_length {
        return Err(Error::invalid_data(format!(
            "FSB5 DSP source declares {} encoded bytes but {required} are required",
            stream.compressed_length
        )));
    }
    if required > stream.data_length {
        return Err(Error::invalid_data(
            "FSB5 DSP encoded frames exceed the source data range",
        ));
    }
    if stream.non_interleaved {
        let stride = stream.data_length / u64::from(stream.channels);
        if stride < channel_bytes {
            return Err(Error::invalid_data(
                "FSB5 non-interleaved DSP channel stride is too short",
            ));
        }
        let last_channel_start = stride
            .checked_mul(u64::from(stream.channels - 1))
            .ok_or_else(|| Error::invalid_data("FSB5 DSP channel offset overflowed"))?;
        let last_channel_end = last_channel_start
            .checked_add(channel_bytes)
            .ok_or_else(|| Error::invalid_data("FSB5 DSP channel range overflowed"))?;
        if last_channel_end > stream.data_length {
            return Err(Error::invalid_data(
                "FSB5 non-interleaved DSP channel exceeds the source data range",
            ));
        }
    }
    Ok(())
}

fn write_fsb5_dsp_wav(
    payload: &Region,
    stream: Fsb5DspStream,
    maximum_output_bytes: u64,
    output: &mut impl Write,
) -> Result<u64> {
    let output_size = dsp_wave_output_size(payload, stream)?;
    if output_size > maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "WAV output is {output_size} bytes, exceeding limit {maximum_output_bytes}"
        )));
    }
    validate_dsp_predictors(payload, stream)?;
    let data_size = u32::try_from(output_size - WAV_HEADER_BYTES)
        .expect("DSP WAV output size validation bounded the data chunk");
    write_wav_header(
        output,
        data_size,
        PcmSampleFormat::Signed16.wave_format(),
        stream.channels,
        stream.sample_rate,
        16,
    )?;
    decode_fsb5_dsp(payload, stream, output)?;
    Ok(output_size)
}

fn validate_dsp_predictors(payload: &Region, stream: Fsb5DspStream) -> Result<()> {
    let encoded_frames = stream.frame_count.div_ceil(14);
    for frame_index in 0..encoded_frames {
        for channel in 0..usize::from(stream.channels) {
            let mut frame = [0_u8; 8];
            read_dsp_frame(payload, stream, frame_index, channel, &mut frame)?;
            let predictor = frame[0] >> 4;
            if predictor >= 8 {
                return Err(Error::invalid_data(format!(
                    "FSB5 DSP frame uses invalid coefficient pair {predictor}"
                )));
            }
        }
    }
    Ok(())
}

fn decode_fsb5_dsp(payload: &Region, stream: Fsb5DspStream, output: &mut impl Write) -> Result<()> {
    const MAX_CHANNELS: usize = 255;
    const COEFFICIENTS_PER_CHANNEL: usize = 16;
    const SAMPLES_PER_FRAME: usize = 14;
    let channels = usize::from(stream.channels);
    let mut coefficients = [0_i16; MAX_CHANNELS * COEFFICIENTS_PER_CHANNEL];
    let mut histories_1 = [0_i32; MAX_CHANNELS];
    let mut histories_2 = [0_i32; MAX_CHANNELS];
    let mut samples = [0_i16; MAX_CHANNELS * SAMPLES_PER_FRAME];
    let mut output_bytes = [0_u8; MAX_CHANNELS * SAMPLES_PER_FRAME * 2];
    read_dsp_coefficients(payload, stream, &mut coefficients)?;

    let encoded_frames = stream.frame_count.div_ceil(14);
    let mut remaining_samples = stream.frame_count;
    for frame_index in 0..encoded_frames {
        let frame_samples = usize::try_from(remaining_samples.min(14))
            .expect("DSP frame sample count is at most fourteen");
        for channel in 0..channels {
            let mut frame = [0_u8; 8];
            read_dsp_frame(payload, stream, frame_index, channel, &mut frame)?;
            decode_dsp_channel_frame(
                frame,
                &coefficients
                    [channel * COEFFICIENTS_PER_CHANNEL..(channel + 1) * COEFFICIENTS_PER_CHANNEL],
                &mut histories_1[channel],
                &mut histories_2[channel],
                &mut samples[channel * SAMPLES_PER_FRAME..][..frame_samples],
            )?;
        }
        let output_length = frame_samples
            .checked_mul(channels)
            .and_then(|value| value.checked_mul(2))
            .expect("fixed DSP frame dimensions cannot overflow usize");
        for sample_index in 0..frame_samples {
            for channel in 0..channels {
                let source = samples[channel * SAMPLES_PER_FRAME + sample_index];
                let destination = (sample_index * channels + channel) * 2;
                output_bytes[destination..destination + 2].copy_from_slice(&source.to_le_bytes());
            }
        }
        output.write_all(&output_bytes[..output_length])?;
        remaining_samples -= u64::try_from(frame_samples).expect("frame sample count fits u64");
    }
    Ok(())
}

fn read_dsp_coefficients(
    payload: &Region,
    stream: Fsb5DspStream,
    coefficients: &mut [i16; 255 * 16],
) -> Result<()> {
    for channel in 0..usize::from(stream.channels) {
        let channel_offset = stream
            .coefficients_offset
            .checked_add(u64::try_from(channel).expect("channel index fits u64") * 0x2e)
            .ok_or_else(|| Error::invalid_data("FSB5 DSP coefficient offset overflowed"))?;
        for coefficient in 0..16 {
            let offset = channel_offset
                .checked_add(u64::try_from(coefficient * 2).expect("coefficient offset fits u64"))
                .ok_or_else(|| Error::invalid_data("FSB5 DSP coefficient offset overflowed"))?;
            coefficients[channel * 16 + coefficient] = read_i16_be(payload, offset)?;
        }
    }
    Ok(())
}

fn read_dsp_frame(
    payload: &Region,
    stream: Fsb5DspStream,
    frame_index: u64,
    channel: usize,
    frame: &mut [u8; 8],
) -> Result<()> {
    let channel = u64::try_from(channel).expect("channel index fits u64");
    if stream.non_interleaved {
        let stride = stream.data_length / u64::from(stream.channels);
        let offset = stream
            .data_offset
            .checked_add(channel * stride)
            .and_then(|value| value.checked_add(frame_index * 8))
            .ok_or_else(|| Error::invalid_data("FSB5 DSP frame offset overflowed"))?;
        payload.read_exact_at(offset, frame)?;
        return Ok(());
    }
    let channels = u64::from(stream.channels);
    let frame_base = stream
        .data_offset
        .checked_add(frame_index * 8 * channels)
        .ok_or_else(|| Error::invalid_data("FSB5 DSP frame offset overflowed"))?;
    for (byte_index, destination) in frame.iter_mut().enumerate() {
        let byte_index = u64::try_from(byte_index).expect("DSP byte index fits u64");
        let offset = frame_base
            .checked_add((byte_index / 2) * 2 * channels)
            .and_then(|value| value.checked_add(byte_index % 2))
            .and_then(|value| value.checked_add(2 * channel))
            .ok_or_else(|| Error::invalid_data("FSB5 DSP subinterleave offset overflowed"))?;
        *destination = read_u8(payload, offset)?;
    }
    Ok(())
}

fn decode_dsp_channel_frame(
    frame: [u8; 8],
    coefficients: &[i16],
    history_1: &mut i32,
    history_2: &mut i32,
    output: &mut [i16],
) -> Result<()> {
    let predictor = usize::from(frame[0] >> 4);
    if predictor >= 8 {
        return Err(Error::invalid_data(format!(
            "FSB5 DSP frame uses invalid coefficient pair {predictor}"
        )));
    }
    let scale = 1_i64 << (frame[0] & 0x0f);
    let coefficient_1 = i64::from(coefficients[predictor * 2]);
    let coefficient_2 = i64::from(coefficients[predictor * 2 + 1]);
    for (sample_index, destination) in output.iter_mut().enumerate() {
        let byte = frame[1 + sample_index / 2];
        let nibble = if sample_index.is_multiple_of(2) {
            byte >> 4
        } else {
            byte & 0x0f
        };
        let signed = if nibble >= 8 {
            i64::from(nibble) - 16
        } else {
            i64::from(nibble)
        };
        let decoded = ((signed * scale) * 2048
            + 1024
            + coefficient_1 * i64::from(*history_1)
            + coefficient_2 * i64::from(*history_2))
            >> 11;
        let decoded = decoded.clamp(i64::from(i16::MIN), i64::from(i16::MAX));
        let sample = i16::try_from(decoded).expect("DSP sample was clamped to i16");
        *destination = sample;
        *history_2 = *history_1;
        *history_1 = i32::from(sample);
    }
    Ok(())
}

fn vag_wave_output_size(payload: &Region, stream: Fsb5VagStream) -> Result<u64> {
    validate_vag_source(payload, stream)?;
    let data_length = stream
        .frame_count
        .checked_mul(u64::from(stream.channels))
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| Error::invalid_data("FSB5 VAG decoded byte count overflowed"))?;
    let data_size = u32::try_from(data_length)
        .map_err(|_| Error::invalid_data("WAV PCM payload exceeds its 32-bit data chunk"))?;
    WAV_HEADER_BYTES
        .checked_add(u64::from(data_size))
        .ok_or_else(|| Error::invalid_data("WAV output size overflowed"))
}

fn validate_vag_source(payload: &Region, stream: Fsb5VagStream) -> Result<()> {
    if stream.channels == 0 || stream.sample_rate == 0 || stream.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 VAG channels, sample rate, and frame count must be nonzero",
        ));
    }
    if stream.channels > 255 {
        return Err(Error::invalid_data(
            "FSB5 VAG channel count exceeds the format metadata width",
        ));
    }
    let data_end = stream
        .data_offset
        .checked_add(stream.data_length)
        .ok_or_else(|| Error::invalid_data("FSB5 VAG source range overflowed"))?;
    if data_end > payload.len() {
        return Err(Error::invalid_data(format!(
            "FSB5 VAG source range {}..{data_end} exceeds payload length {}",
            stream.data_offset,
            payload.len()
        )));
    }
    let channel_bytes = stream
        .frame_count
        .div_ceil(28)
        .checked_mul(16)
        .ok_or_else(|| Error::invalid_data("FSB5 VAG channel byte count overflowed"))?;
    let required = channel_bytes
        .checked_mul(u64::from(stream.channels))
        .ok_or_else(|| Error::invalid_data("FSB5 VAG encoded byte count overflowed"))?;
    if required != stream.compressed_length {
        return Err(Error::invalid_data(format!(
            "FSB5 VAG source declares {} encoded bytes but {required} are required",
            stream.compressed_length
        )));
    }
    if required > stream.data_length {
        return Err(Error::invalid_data(
            "FSB5 VAG encoded frames exceed the source data range",
        ));
    }
    if stream.non_interleaved {
        let stride = stream.data_length / u64::from(stream.channels);
        if stride < channel_bytes {
            return Err(Error::invalid_data(
                "FSB5 non-interleaved VAG channel stride is too short",
            ));
        }
        let last_channel_start = stride
            .checked_mul(u64::from(stream.channels - 1))
            .ok_or_else(|| Error::invalid_data("FSB5 VAG channel offset overflowed"))?;
        let last_channel_end = last_channel_start
            .checked_add(channel_bytes)
            .ok_or_else(|| Error::invalid_data("FSB5 VAG channel range overflowed"))?;
        if last_channel_end > stream.data_length {
            return Err(Error::invalid_data(
                "FSB5 non-interleaved VAG channel exceeds the source data range",
            ));
        }
    }
    Ok(())
}

fn write_fsb5_vag_wav(
    payload: &Region,
    stream: Fsb5VagStream,
    maximum_output_bytes: u64,
    output: &mut impl Write,
) -> Result<u64> {
    let output_size = vag_wave_output_size(payload, stream)?;
    if output_size > maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "WAV output is {output_size} bytes, exceeding limit {maximum_output_bytes}"
        )));
    }
    let data_size = u32::try_from(output_size - WAV_HEADER_BYTES)
        .expect("VAG WAV output size validation bounded the data chunk");
    write_wav_header(
        output,
        data_size,
        PcmSampleFormat::Signed16.wave_format(),
        stream.channels,
        stream.sample_rate,
        16,
    )?;
    decode_fsb5_vag(payload, stream, output)?;
    Ok(output_size)
}

fn decode_fsb5_vag(payload: &Region, stream: Fsb5VagStream, output: &mut impl Write) -> Result<()> {
    const MAX_CHANNELS: usize = 255;
    const SAMPLES_PER_FRAME: usize = 28;
    let channels = usize::from(stream.channels);
    let mut histories_1 = [0_i32; MAX_CHANNELS];
    let mut histories_2 = [0_i32; MAX_CHANNELS];
    let mut samples = [0_i16; MAX_CHANNELS * SAMPLES_PER_FRAME];
    let mut output_bytes = [0_u8; MAX_CHANNELS * SAMPLES_PER_FRAME * 2];
    let encoded_frames = stream.frame_count.div_ceil(28);
    let mut remaining_samples = stream.frame_count;

    for frame_index in 0..encoded_frames {
        let frame_samples = usize::try_from(remaining_samples.min(28))
            .expect("VAG frame sample count is at most twenty-eight");
        for channel in 0..channels {
            let mut frame = [0_u8; 16];
            read_vag_frame(payload, stream, frame_index, channel, &mut frame)?;
            decode_vag_channel_frame(
                frame,
                &mut histories_1[channel],
                &mut histories_2[channel],
                &mut samples[channel * SAMPLES_PER_FRAME..][..frame_samples],
            );
        }
        let output_length = frame_samples
            .checked_mul(channels)
            .and_then(|value| value.checked_mul(2))
            .expect("fixed VAG frame dimensions cannot overflow usize");
        for sample_index in 0..frame_samples {
            for channel in 0..channels {
                let source = samples[channel * SAMPLES_PER_FRAME + sample_index];
                let destination = (sample_index * channels + channel) * 2;
                output_bytes[destination..destination + 2].copy_from_slice(&source.to_le_bytes());
            }
        }
        output.write_all(&output_bytes[..output_length])?;
        remaining_samples -= u64::try_from(frame_samples).expect("frame sample count fits u64");
    }
    Ok(())
}

fn read_vag_frame(
    payload: &Region,
    stream: Fsb5VagStream,
    frame_index: u64,
    channel: usize,
    frame: &mut [u8; 16],
) -> Result<()> {
    let channel = u64::try_from(channel).expect("channel index fits u64");
    let offset = if stream.non_interleaved {
        let stride = stream.data_length / u64::from(stream.channels);
        stream
            .data_offset
            .checked_add(channel * stride)
            .and_then(|value| value.checked_add(frame_index * 16))
    } else {
        stream.data_offset.checked_add(
            frame_index
                .checked_mul(u64::from(stream.channels))
                .and_then(|value| value.checked_add(channel))
                .and_then(|value| value.checked_mul(16))
                .ok_or_else(|| Error::invalid_data("FSB5 VAG frame offset overflowed"))?,
        )
    }
    .ok_or_else(|| Error::invalid_data("FSB5 VAG frame offset overflowed"))?;
    payload.read_exact_at(offset, frame)
}

fn decode_vag_channel_frame(
    frame: [u8; 16],
    history_1: &mut i32,
    history_2: &mut i32,
    output: &mut [i16],
) {
    const COEFFICIENTS: [(i64, i64); 6] =
        [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60), (30, 0)];
    let predictor = usize::from(frame[0] >> 4);
    let predictor = if predictor > 5 { 0 } else { predictor };
    let shift = u32::from(frame[0] & 0x0f);
    let shift = if shift > 12 { 9 } else { shift };
    let flag = frame[1];
    let (coefficient_1, coefficient_2) = COEFFICIENTS[predictor];
    for (sample_index, destination) in output.iter_mut().enumerate() {
        let decoded = if flag < 7 {
            let byte = frame[2 + sample_index / 2];
            let nibble = if sample_index.is_multiple_of(2) {
                byte & 0x0f
            } else {
                byte >> 4
            };
            let signed = if nibble >= 8 {
                i64::from(nibble) - 16
            } else {
                i64::from(nibble)
            };
            let scaled = signed << (20 - shift);
            let prediction =
                4 * (coefficient_1 * i64::from(*history_1) + coefficient_2 * i64::from(*history_2));
            (scaled + prediction) >> 8
        } else {
            0
        };
        let history = decoded.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
        let history = i32::try_from(history).expect("VAG history was clamped to i32");
        let sample = history.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        *destination = i16::try_from(sample).expect("VAG sample was clamped to i16");
        *history_2 = *history_1;
        *history_1 = history;
    }
}

fn hevag_wave_output_size(payload: &Region, stream: Fsb5HevagStream) -> Result<u64> {
    validate_hevag_source(payload, stream)?;
    let data_length = stream
        .frame_count
        .checked_mul(u64::from(stream.channels))
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| Error::invalid_data("FSB5 HEVAG decoded byte count overflowed"))?;
    let data_size = u32::try_from(data_length)
        .map_err(|_| Error::invalid_data("WAV PCM payload exceeds its 32-bit data chunk"))?;
    WAV_HEADER_BYTES
        .checked_add(u64::from(data_size))
        .ok_or_else(|| Error::invalid_data("WAV output size overflowed"))
}

fn validate_hevag_source(payload: &Region, stream: Fsb5HevagStream) -> Result<()> {
    if stream.channels == 0 || stream.sample_rate == 0 || stream.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 HEVAG channels, sample rate, and frame count must be nonzero",
        ));
    }
    if stream.channels > 255 {
        return Err(Error::invalid_data(
            "FSB5 HEVAG channel count exceeds the format metadata width",
        ));
    }
    let data_end = stream
        .data_offset
        .checked_add(stream.data_length)
        .ok_or_else(|| Error::invalid_data("FSB5 HEVAG source range overflowed"))?;
    if data_end > payload.len() {
        return Err(Error::invalid_data(format!(
            "FSB5 HEVAG source range {}..{data_end} exceeds payload length {}",
            stream.data_offset,
            payload.len()
        )));
    }
    let required = stream
        .frame_count
        .div_ceil(28)
        .checked_mul(16)
        .and_then(|value| value.checked_mul(u64::from(stream.channels)))
        .ok_or_else(|| Error::invalid_data("FSB5 HEVAG encoded byte count overflowed"))?;
    if required != stream.compressed_length {
        return Err(Error::invalid_data(format!(
            "FSB5 HEVAG source declares {} encoded bytes but {required} are required",
            stream.compressed_length
        )));
    }
    if required > stream.data_length {
        return Err(Error::invalid_data(
            "FSB5 HEVAG encoded frames exceed the source data range",
        ));
    }
    Ok(())
}

fn write_fsb5_hevag_wav(
    payload: &Region,
    stream: Fsb5HevagStream,
    maximum_output_bytes: u64,
    output: &mut impl Write,
) -> Result<u64> {
    let output_size = hevag_wave_output_size(payload, stream)?;
    if output_size > maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "WAV output is {output_size} bytes, exceeding limit {maximum_output_bytes}"
        )));
    }
    let data_size = u32::try_from(output_size - WAV_HEADER_BYTES)
        .expect("HEVAG WAV output size validation bounded the data chunk");
    write_wav_header(
        output,
        data_size,
        PcmSampleFormat::Signed16.wave_format(),
        stream.channels,
        stream.sample_rate,
        16,
    )?;
    decode_fsb5_hevag(payload, stream, output)?;
    Ok(output_size)
}

fn decode_fsb5_hevag(
    payload: &Region,
    stream: Fsb5HevagStream,
    output: &mut impl Write,
) -> Result<()> {
    const MAX_CHANNELS: usize = 255;
    const SAMPLES_PER_FRAME: usize = 28;
    let channels = usize::from(stream.channels);
    let mut histories = [[0_i32; 4]; MAX_CHANNELS];
    let mut samples = [0_i16; MAX_CHANNELS * SAMPLES_PER_FRAME];
    let mut output_bytes = [0_u8; MAX_CHANNELS * SAMPLES_PER_FRAME * 2];
    let encoded_frames = stream.frame_count.div_ceil(28);
    let mut remaining_samples = stream.frame_count;

    for frame_index in 0..encoded_frames {
        let frame_samples = usize::try_from(remaining_samples.min(28))
            .expect("HEVAG frame sample count is at most twenty-eight");
        for channel in 0..channels {
            let mut frame = [0_u8; 16];
            read_hevag_frame(payload, stream, frame_index, channel, &mut frame)?;
            decode_hevag_channel_frame(
                frame,
                &mut histories[channel],
                &mut samples[channel * SAMPLES_PER_FRAME..][..frame_samples],
            );
        }
        let output_length = frame_samples
            .checked_mul(channels)
            .and_then(|value| value.checked_mul(2))
            .expect("fixed HEVAG frame dimensions cannot overflow usize");
        for sample_index in 0..frame_samples {
            for channel in 0..channels {
                let source = samples[channel * SAMPLES_PER_FRAME + sample_index];
                let destination = (sample_index * channels + channel) * 2;
                output_bytes[destination..destination + 2].copy_from_slice(&source.to_le_bytes());
            }
        }
        output.write_all(&output_bytes[..output_length])?;
        remaining_samples -= u64::try_from(frame_samples).expect("frame sample count fits u64");
    }
    Ok(())
}

fn read_hevag_frame(
    payload: &Region,
    stream: Fsb5HevagStream,
    frame_index: u64,
    channel: usize,
    frame: &mut [u8; 16],
) -> Result<()> {
    let channel = u64::try_from(channel).expect("channel index fits u64");
    let frame_number = frame_index
        .checked_mul(u64::from(stream.channels))
        .and_then(|value| value.checked_add(channel))
        .ok_or_else(|| Error::invalid_data("FSB5 HEVAG frame index overflowed"))?;
    let offset = stream
        .data_offset
        .checked_add(
            frame_number
                .checked_mul(16)
                .ok_or_else(|| Error::invalid_data("FSB5 HEVAG frame offset overflowed"))?,
        )
        .ok_or_else(|| Error::invalid_data("FSB5 HEVAG frame offset overflowed"))?;
    payload.read_exact_at(offset, frame)
}

fn decode_hevag_channel_frame(frame: [u8; 16], histories: &mut [i32; 4], output: &mut [i16]) {
    let mut predictor = usize::from(frame[1] & 0xf0) | usize::from(frame[0] >> 4);
    if predictor >= HEVAG_COEFFICIENTS.len() {
        predictor = 0;
    }
    let shift = u32::from(frame[0] & 0x0f);
    let flag = frame[1] & 0x0f;
    let coefficients = HEVAG_COEFFICIENTS[predictor];
    for (sample_index, destination) in output.iter_mut().enumerate() {
        let history = if flag < 7 {
            let byte = frame[2 + sample_index / 2];
            let nibble = if sample_index.is_multiple_of(2) {
                byte & 0x0f
            } else {
                byte >> 4
            };
            let signed = if nibble >= 8 {
                i16::from(nibble) - 16
            } else {
                i16::from(nibble)
            };
            let code = i32::from(signed) << 12 >> shift;
            let prediction = hevag_i32_to_f32(histories[0]) * (f32::from(coefficients[0]) / 8192.0)
                + hevag_i32_to_f32(histories[1]) * (f32::from(coefficients[1]) / 8192.0)
                + hevag_i32_to_f32(histories[2]) * (f32::from(coefficients[2]) / 8192.0)
                + hevag_i32_to_f32(histories[3]) * (f32::from(coefficients[3]) / 8192.0);
            hevag_f32_to_i32(
                f32::from(i16::try_from(code).expect("HEVAG scaled nibble fits i16")) + prediction,
            )
        } else {
            0
        };
        let sample = history.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        *destination = i16::try_from(sample).expect("HEVAG sample was clamped to i16");
        histories.rotate_right(1);
        histories[0] = history;
    }
}

// The reference decoder uses f32 prediction with unclamped i32 histories. These
// two conversions intentionally reproduce that arithmetic at the narrow edge.
#[allow(clippy::cast_precision_loss)]
fn hevag_i32_to_f32(value: i32) -> f32 {
    value as f32
}

#[allow(clippy::cast_possible_truncation)]
fn hevag_f32_to_i32(value: f32) -> i32 {
    value as i32
}

const HEVAG_COEFFICIENTS: [[i16; 4]; 128] = [
    [0, 0, 0, 0],
    [7680, 0, 0, 0],
    [14720, -6656, 0, 0],
    [12544, -7040, 0, 0],
    [15616, -7680, 0, 0],
    [14731, -7059, 0, 0],
    [14506, -7365, 0, 0],
    [13920, -7521, 0, 0],
    [13132, -7680, 0, 0],
    [12027, -7680, 0, 0],
    [10763, -7680, 0, 0],
    [9359, -7680, 0, 0],
    [7832, -7680, 0, 0],
    [6201, -7680, 0, 0],
    [4487, -7680, 0, 0],
    [2717, -7680, 0, 0],
    [909, -7680, 0, 0],
    [-909, -7680, 0, 0],
    [-2717, -7680, 0, 0],
    [-4487, -7680, 0, 0],
    [-6201, -7680, 0, 0],
    [-7832, -7680, 0, 0],
    [-9359, -7680, 0, 0],
    [-10763, -7680, 0, 0],
    [-12027, -7680, 0, 0],
    [-13132, -7680, 0, 0],
    [-13920, -7521, 0, 0],
    [-14506, -7365, 0, 0],
    [-14731, -7059, 0, 0],
    [5376, -9216, 3328, -3072],
    [-6400, -7168, -3328, -2304],
    [-10496, -7424, -3584, -1024],
    [-166, -2721, -493, -540],
    [-7429, -2220, -2298, 423],
    [-8000, -3166, -2814, 288],
    [6017, -4749, 2649, -1298],
    [3798, -6945, 3874, -1216],
    [-8237, -2595, -2071, 227],
    [9198, 1982, -1381, -2315],
    [13020, -3043, -3791, 1267],
    [13111, -4486, -2249, 1664],
    [-1667, -3744, -6456, 839],
    [7818, -4327, 2111, -505],
    [9571, -1336, -757, 486],
    [10032, -2561, 300, 198],
    [-4744, -4122, -5485, -1493],
    [-5895, 2377, -4787, -6946],
    [-1192, -9116, -1237, -3113],
    [2783, -7107, -1574, -1446],
    [-7333, -2061, -2211, 445],
    [6126, -2577, -314, -17],
    [9456, -1857, 102, 258],
    [7875, -4482, 2125, -537],
    [-7171, -1794, -2069, 482],
    [-7358, -2102, -2233, 440],
    [-9170, -3509, -2674, -390],
    [-2637, -2647, -1928, -1636],
    [1873, 9183, 1859, -5746],
    [9214, 1858, -1123, -2427],
    [13203, -3011, -4138, 1370],
    [12437, -4792, -256, 621],
    [-2653, -1144, -3181, -6878],
    [9331, -1048, -828, 506],
    [1641, -620, -946, -4228],
    [4246, -7584, -533, -2259],
    [-8988, -3891, -2807, 44],
    [-2561, -2734, -1729, -1898],
    [3181, -483, -713, -1420],
    [7936, -3843, 2820, -1019],
    [10069, -2609, 313, 195],
    [8399, -3296, 1550, -154],
    [-8529, -2775, -2432, -336],
    [9477, -1882, 108, 256],
    [74, -2240, -297, -6937],
    [-9143, -4160, -2963, 4],
    [-7269, -1957, -2155, 460],
    [-2740, 3744, 5936, -1088],
    [8992, 1948, -682, -2703],
    [13101, -2835, -3853, 1055],
    [9543, -1960, 130, 249],
    [5272, -4269, 3124, -3157],
    [-7695, -3383, -2907, -455],
    [7308, 2523, 434, -2461],
    [10275, -2867, 390, 172],
    [10939, -3720, 665, 96],
    [24, -310, -1261, 320],
    [-8122, -2410, -2310, -271],
    [-8510, -3067, -2336, 163],
    [326, -3845, 419, -932],
    [8894, 2194, -540, -2880],
    [12073, -1876, -2016, -601],
    [8729, -3423, 1673, -169],
    [12949, -3846, -3007, 1946],
    [10038, -2569, 301, 198],
    [9385, -2756, 1008, 40],
    [-4720, -5005, -2851, -1160],
    [7869, -4325, 2135, -501],
    [2450, -8597, 1299, -2780],
    [10191, -2762, 359, 181],
    [11312, -4213, 832, 53],
    [10154, -2716, 345, 185],
    [9638, -1416, -736, 482],
    [3853, -4553, 2843, -3396],
    [6698, -5659, 2248, -1074],
    [11081, -3907, 728, 80],
    [-1025, -9810, -805, -3461],
    [10396, -3745, 1367, -96],
    [10286, 988, -1915, -1437],
    [7953, 3877, -764, -3263],
    [12689, -3374, -3354, 2079],
    [6641, 3166, 230, -2088],
    [-2347, -7354, -1944, -4122],
    [9289, -4038, 1885, -246],
    [4633, -6402, 1748, -1619],
    [11246, -4125, 802, 61],
    [9807, -2283, 218, 221],
    [9736, -1536, -706, 473],
    [8439, -3435, 1562, -176],
    [9307, -1021, -834, 508],
    [1697, -9025, 688, -3037],
    [10214, -2790, 368, 179],
    [8389, 3248, -758, -2988],
    [7200, 3316, 46, -2614],
    [-88, -7808, -537, -4571],
    [6193, -5188, 2759, -1245],
    [12324, -1289, -3284, 252],
    [13064, -4074, -2823, 1877],
    [5333, 2999, 774, -1131],
];

fn fadpcm_wave_output_size(payload: &Region, stream: Fsb5FadpcmStream) -> Result<u64> {
    validate_fadpcm_source(payload, stream)?;
    let data_length = stream
        .frame_count
        .checked_mul(u64::from(stream.channels))
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| Error::invalid_data("FSB5 FADPCM decoded byte count overflowed"))?;
    let data_size = u32::try_from(data_length)
        .map_err(|_| Error::invalid_data("WAV PCM payload exceeds its 32-bit data chunk"))?;
    WAV_HEADER_BYTES
        .checked_add(u64::from(data_size))
        .ok_or_else(|| Error::invalid_data("WAV output size overflowed"))
}

fn validate_fadpcm_source(payload: &Region, stream: Fsb5FadpcmStream) -> Result<()> {
    if stream.channels == 0 || stream.sample_rate == 0 || stream.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 FADPCM channels, sample rate, and frame count must be nonzero",
        ));
    }
    if stream.channels > 255 {
        return Err(Error::invalid_data(
            "FSB5 FADPCM channel count exceeds the format metadata width",
        ));
    }
    let data_end = stream
        .data_offset
        .checked_add(stream.data_length)
        .ok_or_else(|| Error::invalid_data("FSB5 FADPCM source range overflowed"))?;
    if data_end > payload.len() {
        return Err(Error::invalid_data(format!(
            "FSB5 FADPCM source range {}..{data_end} exceeds payload length {}",
            stream.data_offset,
            payload.len()
        )));
    }
    let required = stream
        .frame_count
        .div_ceil(256)
        .checked_mul(0x8c)
        .and_then(|value| value.checked_mul(u64::from(stream.channels)))
        .ok_or_else(|| Error::invalid_data("FSB5 FADPCM encoded byte count overflowed"))?;
    if required != stream.compressed_length {
        return Err(Error::invalid_data(format!(
            "FSB5 FADPCM source declares {} encoded bytes but {required} are required",
            stream.compressed_length
        )));
    }
    if required > stream.data_length {
        return Err(Error::invalid_data(
            "FSB5 FADPCM encoded frames exceed the source data range",
        ));
    }
    Ok(())
}

fn write_fsb5_fadpcm_wav(
    payload: &Region,
    stream: Fsb5FadpcmStream,
    maximum_output_bytes: u64,
    output: &mut impl Write,
) -> Result<u64> {
    let output_size = fadpcm_wave_output_size(payload, stream)?;
    if output_size > maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "WAV output is {output_size} bytes, exceeding limit {maximum_output_bytes}"
        )));
    }
    let data_size = u32::try_from(output_size - WAV_HEADER_BYTES)
        .expect("FADPCM WAV output size validation bounded the data chunk");
    write_wav_header(
        output,
        data_size,
        PcmSampleFormat::Signed16.wave_format(),
        stream.channels,
        stream.sample_rate,
        16,
    )?;
    decode_fsb5_fadpcm(payload, stream, output)?;
    Ok(output_size)
}

fn decode_fsb5_fadpcm(
    payload: &Region,
    stream: Fsb5FadpcmStream,
    output: &mut impl Write,
) -> Result<()> {
    const MAX_CHANNELS: usize = 255;
    const FRAME_BYTES: usize = 0x8c;
    let channels = usize::from(stream.channels);
    let frame_buffer_bytes = channels
        .checked_mul(FRAME_BYTES)
        .ok_or_else(|| Error::invalid_data("FADPCM frame buffer size overflowed"))?;
    let mut frames = Vec::new();
    frames
        .try_reserve_exact(frame_buffer_bytes)
        .map_err(|_| Error::invalid_data("FADPCM frame buffer allocation failed"))?;
    frames.resize(frame_buffer_bytes, 0);
    let mut coefficients = [0_u32; MAX_CHANNELS];
    let mut shifts = [0_u32; MAX_CHANNELS];
    let mut histories_1 = [0_i32; MAX_CHANNELS];
    let mut histories_2 = [0_i32; MAX_CHANNELS];
    let mut output_frame = [0_u8; MAX_CHANNELS * 2];
    let encoded_frames = stream.frame_count.div_ceil(256);
    let mut remaining_samples = stream.frame_count;

    for frame_index in 0..encoded_frames {
        for channel in 0..channels {
            let frame = &mut frames[channel * FRAME_BYTES..(channel + 1) * FRAME_BYTES];
            let channel = u64::try_from(channel).expect("channel index fits u64");
            let offset = stream
                .data_offset
                .checked_add(
                    frame_index
                        .checked_mul(u64::from(stream.channels))
                        .and_then(|value| value.checked_add(channel))
                        .and_then(|value| value.checked_mul(0x8c))
                        .ok_or_else(|| {
                            Error::invalid_data("FSB5 FADPCM frame offset overflowed")
                        })?,
                )
                .ok_or_else(|| Error::invalid_data("FSB5 FADPCM frame offset overflowed"))?;
            payload.read_exact_at(offset, frame)?;
            coefficients[usize::try_from(channel).expect("channel index fits usize")] =
                u32::from_le_bytes(frame[..4].try_into().expect("FADPCM coefficient header"));
            shifts[usize::try_from(channel).expect("channel index fits usize")] =
                u32::from_le_bytes(frame[4..8].try_into().expect("FADPCM shift header"));
            histories_1[usize::try_from(channel).expect("channel index fits usize")] = i32::from(
                i16::from_le_bytes(frame[8..10].try_into().expect("FADPCM first history")),
            );
            histories_2[usize::try_from(channel).expect("channel index fits usize")] = i32::from(
                i16::from_le_bytes(frame[10..12].try_into().expect("FADPCM second history")),
            );
        }
        let frame_samples = usize::try_from(remaining_samples.min(256))
            .expect("FADPCM frame sample count is at most 256");
        for sample_index in 0..frame_samples {
            for channel in 0..channels {
                let frame = &frames[channel * FRAME_BYTES..(channel + 1) * FRAME_BYTES];
                let sample = decode_fadpcm_sample(
                    frame,
                    coefficients[channel],
                    shifts[channel],
                    sample_index,
                    &mut histories_1[channel],
                    &mut histories_2[channel],
                );
                output_frame[channel * 2..channel * 2 + 2].copy_from_slice(&sample.to_le_bytes());
            }
            output.write_all(&output_frame[..channels * 2])?;
        }
        remaining_samples -= u64::try_from(frame_samples).expect("frame sample count fits u64");
    }
    Ok(())
}

fn decode_fadpcm_sample(
    frame: &[u8],
    coefficients: u32,
    shifts: u32,
    sample_index: usize,
    history_1: &mut i32,
    history_2: &mut i32,
) -> i16 {
    const COEFFICIENTS: [(i64, i64); 7] = [
        (0, 0),
        (60, 0),
        (122, 60),
        (115, 52),
        (98, 55),
        (0, 0),
        (0, 0),
    ];
    let set = sample_index / 32;
    let within_set = sample_index % 32;
    let group = within_set / 8;
    let nibble_index = within_set % 8;
    let packed_offset = 0x0c + set * 0x10 + group * 4;
    let packed = u32::from_le_bytes(
        frame[packed_offset..packed_offset + 4]
            .try_into()
            .expect("FADPCM packed group"),
    );
    let nibble = u8::try_from((packed >> (nibble_index * 4)) & 0x0f)
        .expect("four-bit FADPCM nibble fits u8");
    let signed = if nibble >= 8 {
        i64::from(nibble) - 16
    } else {
        i64::from(nibble)
    };
    let coefficient_index = usize::try_from((coefficients >> (set * 4)) & 0x0f)
        .expect("four-bit coefficient index fits usize")
        % 7;
    let shift = (shifts >> (set * 4)) & 0x0f;
    let (coefficient_1, coefficient_2) = COEFFICIENTS[coefficient_index];
    let scaled = signed << (6 + shift);
    let decoded = (scaled - i64::from(*history_2) * coefficient_2
        + i64::from(*history_1) * coefficient_1)
        >> 6;
    let sample = decoded.clamp(i64::from(i16::MIN), i64::from(i16::MAX));
    let sample = i16::try_from(sample).expect("FADPCM sample was clamped to i16");
    *history_2 = *history_1;
    *history_1 = i32::from(sample);
    sample
}

fn mpeg_wave_output_size(payload: &Region, stream: Fsb5MpegStream) -> Result<u64> {
    validate_mpeg_source(payload, stream)?;
    let data_length = stream
        .frame_count
        .checked_mul(u64::from(stream.channels))
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| Error::invalid_data("FSB5 MPEG decoded byte count overflowed"))?;
    let data_size = u32::try_from(data_length)
        .map_err(|_| Error::invalid_data("WAV PCM payload exceeds its 32-bit data chunk"))?;
    WAV_HEADER_BYTES
        .checked_add(u64::from(data_size))
        .ok_or_else(|| Error::invalid_data("WAV output size overflowed"))
}

fn validate_mpeg_source(payload: &Region, stream: Fsb5MpegStream) -> Result<()> {
    if stream.channels == 0 || stream.sample_rate == 0 || stream.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 MPEG channels, sample rate, and frame count must be nonzero",
        ));
    }
    if stream.channels > 2 {
        return Err(Error::unsupported(
            "FSB5 multistream MPEG decoding is not implemented",
        ));
    }
    let data_end = stream
        .data_offset
        .checked_add(stream.data_length)
        .ok_or_else(|| Error::invalid_data("FSB5 MPEG source range overflowed"))?;
    if data_end > payload.len() {
        return Err(Error::invalid_data(format!(
            "FSB5 MPEG source range {}..{data_end} exceeds payload length {}",
            stream.data_offset,
            payload.len()
        )));
    }
    let scan = scan_mpeg_frames(
        payload,
        stream.data_offset,
        stream.data_length,
        stream.frame_count,
        stream.channels,
        stream.sample_rate,
    )?;
    if scan.compressed_length != stream.compressed_length || scan.layer != stream.layer {
        return Err(Error::invalid_data(
            "FSB5 MPEG stream metadata does not match its encoded frames",
        ));
    }
    Ok(())
}

fn scan_mpeg_frames(
    payload: &Region,
    data_offset: u64,
    data_length: u64,
    frame_count: u64,
    channels: u16,
    sample_rate: u32,
) -> Result<MpegScan> {
    let data_end = data_offset
        .checked_add(data_length)
        .ok_or_else(|| Error::invalid_data("FSB5 MPEG source range overflowed"))?;
    if data_end > payload.len() {
        return Err(Error::invalid_data("FSB5 MPEG source exceeds its payload"));
    }
    let mut offset = data_offset;
    let mut decoded_frames = 0_u64;
    let mut first_layer = None;
    while decoded_frames < frame_count {
        let header = read_mpeg_frame_header(payload, offset)?;
        validate_mpeg_frame_spec(header, channels, sample_rate, first_layer)?;
        first_layer.get_or_insert(header.layer);
        let padded_length = align_mpeg_frame(header.byte_length)?;
        let next = offset
            .checked_add(padded_length)
            .ok_or_else(|| Error::invalid_data("FSB5 MPEG frame range overflowed"))?;
        if next > data_end {
            return Err(Error::invalid_data(format!(
                "FSB5 MPEG frame at {offset} requires {padded_length} padded bytes"
            )));
        }
        offset = next;
        decoded_frames = decoded_frames
            .checked_add(u64::from(header.samples))
            .ok_or_else(|| Error::invalid_data("FSB5 MPEG sample count overflowed"))?;
    }
    if data_end - offset >= 32 {
        return Err(Error::invalid_data(
            "FSB5 MPEG source has more than 31 trailing alignment bytes",
        ));
    }
    Ok(MpegScan {
        compressed_length: offset - data_offset,
        layer: first_layer.expect("nonzero FSB5 frame count parsed at least one MPEG frame"),
    })
}

fn validate_mpeg_frame_spec(
    header: MpegFrameHeader,
    channels: u16,
    sample_rate: u32,
    expected_layer: Option<Fsb5MpegLayer>,
) -> Result<()> {
    if header.channels != channels {
        return Err(Error::invalid_data(format!(
            "FSB5 declares {channels} MPEG channels but a frame declares {}",
            header.channels
        )));
    }
    if header.sample_rate != sample_rate {
        return Err(Error::invalid_data(format!(
            "FSB5 declares MPEG sample rate {sample_rate} but a frame declares {}",
            header.sample_rate
        )));
    }
    if expected_layer.is_some_and(|layer| layer != header.layer) {
        return Err(Error::invalid_data(
            "FSB5 MPEG stream changes audio layer between frames",
        ));
    }
    Ok(())
}

fn read_mpeg_frame_header(payload: &Region, offset: u64) -> Result<MpegFrameHeader> {
    let header_end = offset
        .checked_add(MPEG_FRAME_HEADER_BYTES)
        .ok_or_else(|| Error::invalid_data("MPEG frame-header range overflowed"))?;
    if header_end > payload.len() {
        return Err(Error::invalid_data("FSB5 MPEG frame header is truncated"));
    }
    let mut bytes = [0_u8; 4];
    payload.read_exact_at(offset, &mut bytes)?;
    parse_mpeg_frame_header(u32::from_be_bytes(bytes))
}

fn parse_mpeg_frame_header(word: u32) -> Result<MpegFrameHeader> {
    if word >> 21 != 0x07ff {
        return Err(Error::invalid_data("FSB5 MPEG frame has no sync word"));
    }
    let version_id = u8::try_from((word >> 19) & 0x03).expect("two MPEG version bits fit u8");
    if version_id == 1 {
        return Err(Error::invalid_data(
            "FSB5 MPEG frame has a reserved version",
        ));
    }
    let layer = match (word >> 17) & 0x03 {
        1 => Fsb5MpegLayer::Layer3,
        2 => Fsb5MpegLayer::Layer2,
        3 => return Err(Error::unsupported("FSB5 MPEG Layer I is not supported")),
        _ => return Err(Error::invalid_data("FSB5 MPEG frame has a reserved layer")),
    };
    let bitrate_index =
        usize::try_from((word >> 12) & 0x0f).expect("four MPEG bitrate bits fit usize");
    let bitrate = mpeg_bitrate_kbps(version_id, layer, bitrate_index)?;
    let rate_index =
        usize::try_from((word >> 10) & 0x03).expect("two MPEG sample-rate bits fit usize");
    let sample_rate = mpeg_sample_rate(version_id, rate_index)?;
    let padding = u64::from((word >> 9) & 1);
    let coefficient = if layer == Fsb5MpegLayer::Layer3 && version_id != 3 {
        72_u64
    } else {
        144
    };
    let byte_length = coefficient
        .checked_mul(u64::from(bitrate) * 1000)
        .and_then(|value| value.checked_div(u64::from(sample_rate)))
        .and_then(|value| value.checked_add(padding))
        .ok_or_else(|| Error::invalid_data("FSB5 MPEG frame size overflowed"))?;
    if !(MPEG_FRAME_HEADER_BYTES..=u64::try_from(MAX_MPEG_FRAME_BYTES).unwrap())
        .contains(&byte_length)
    {
        return Err(Error::invalid_data(format!(
            "FSB5 MPEG frame size {byte_length} is outside the supported bound"
        )));
    }
    let channels = if (word >> 6) & 0x03 == 3 { 1 } else { 2 };
    let samples = if layer == Fsb5MpegLayer::Layer3 && version_id != 3 {
        576
    } else {
        1152
    };
    Ok(MpegFrameHeader {
        byte_length,
        samples,
        channels,
        sample_rate,
        layer,
    })
}

fn mpeg_bitrate_kbps(version_id: u8, layer: Fsb5MpegLayer, index: usize) -> Result<u32> {
    const MPEG1_LAYER2: [u32; 16] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0,
    ];
    const MPEG1_LAYER3: [u32; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    const MPEG2_LAYER23: [u32; 16] = [
        0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
    ];
    let bitrate = match (version_id, layer) {
        (3, Fsb5MpegLayer::Layer2) => MPEG1_LAYER2[index],
        (3, Fsb5MpegLayer::Layer3) => MPEG1_LAYER3[index],
        (_, Fsb5MpegLayer::Layer2 | Fsb5MpegLayer::Layer3) => MPEG2_LAYER23[index],
    };
    if bitrate == 0 {
        return Err(Error::unsupported(format!(
            "FSB5 MPEG bitrate index {index} is free or reserved"
        )));
    }
    Ok(bitrate)
}

fn mpeg_sample_rate(version_id: u8, index: usize) -> Result<u32> {
    const MPEG1_RATES: [u32; 3] = [44_100, 48_000, 32_000];
    let base = *MPEG1_RATES
        .get(index)
        .ok_or_else(|| Error::invalid_data("FSB5 MPEG sample-rate index is reserved"))?;
    Ok(match version_id {
        3 => base,
        2 => base / 2,
        0 => base / 4,
        _ => return Err(Error::invalid_data("FSB5 MPEG version is reserved")),
    })
}

fn align_mpeg_frame(length: u64) -> Result<u64> {
    length
        .checked_add(3)
        .map(|value| value & !3)
        .ok_or_else(|| Error::invalid_data("FSB5 MPEG padding size overflowed"))
}

fn write_fsb5_mpeg_wav(
    payload: &Region,
    stream: Fsb5MpegStream,
    maximum_output_bytes: u64,
    output: &mut impl Write,
) -> Result<u64> {
    let output_size = mpeg_wave_output_size(payload, stream)?;
    if output_size > maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "WAV output is {output_size} bytes, exceeding limit {maximum_output_bytes}"
        )));
    }
    let data_size = u32::try_from(output_size - WAV_HEADER_BYTES)
        .expect("MPEG WAV output size validation bounded the data chunk");
    write_wav_header(
        output,
        data_size,
        PcmSampleFormat::Signed16.wave_format(),
        stream.channels,
        stream.sample_rate,
        16,
    )?;
    decode_fsb5_mpeg(payload, stream, output)?;
    Ok(output_size)
}

fn decode_fsb5_mpeg(
    payload: &Region,
    stream: Fsb5MpegStream,
    output: &mut impl Write,
) -> Result<()> {
    let codec = match stream.layer {
        Fsb5MpegLayer::Layer2 => CODEC_ID_MP2,
        Fsb5MpegLayer::Layer3 => CODEC_ID_MP3,
    };
    let mut parameters = AudioCodecParameters::new();
    parameters
        .for_codec(codec)
        .with_sample_rate(stream.sample_rate);
    let options = AudioDecoderOptions::default().gapless(false);
    let mut decoder = MpaDecoder::try_new(&parameters, &options)
        .map_err(|error| Error::invalid_data(format!("cannot create MPEG decoder: {error}")))?;
    let mut frame_bytes = [0_u8; MAX_MPEG_FRAME_BYTES];
    let mut pcm_samples = [0_i16; MAX_MPEG_FRAME_SAMPLES * MAX_MPEG_CHANNELS];
    let mut pcm_bytes = [0_u8; MAX_MPEG_FRAME_SAMPLES * MAX_MPEG_CHANNELS * 2];
    let mut offset = stream.data_offset;
    let mut remaining_frames = stream.frame_count;

    while remaining_frames > 0 {
        let header = read_mpeg_frame_header(payload, offset)?;
        validate_mpeg_frame_spec(
            header,
            stream.channels,
            stream.sample_rate,
            Some(stream.layer),
        )?;
        let frame_length = usize::try_from(header.byte_length)
            .expect("validated MPEG frame size fits the fixed frame buffer");
        payload.read_exact_at(offset, &mut frame_bytes[..frame_length])?;
        let packet = PacketRef::new(
            0,
            Timestamp::ZERO,
            Duration::from(u64::from(header.samples)),
            &frame_bytes[..frame_length],
        );
        let audio_buffer = decoder
            .decode_ref(&packet)
            .map_err(|error| Error::invalid_data(format!("FSB5 MPEG decode failed: {error}")))?;
        validate_decoded_mpeg_spec(&audio_buffer, stream, header)?;
        let write_frames = audio_buffer
            .frames()
            .min(usize::try_from(remaining_frames).unwrap_or(usize::MAX));
        let sample_count = write_frames
            .checked_mul(usize::from(stream.channels))
            .ok_or_else(|| Error::invalid_data("FSB5 MPEG PCM sample count overflowed"))?;
        audio_buffer
            .slice(..write_frames)
            .copy_to_slice_interleaved::<i16, _>(&mut pcm_samples[..sample_count]);
        for (sample, bytes) in pcm_samples[..sample_count]
            .iter()
            .zip(pcm_bytes[..sample_count * 2].chunks_exact_mut(2))
        {
            bytes.copy_from_slice(&sample.to_le_bytes());
        }
        output.write_all(&pcm_bytes[..sample_count * 2])?;
        remaining_frames -= u64::try_from(write_frames).expect("MPEG frame count fits u64");
        offset = offset
            .checked_add(align_mpeg_frame(header.byte_length)?)
            .ok_or_else(|| Error::invalid_data("FSB5 MPEG frame offset overflowed"))?;
    }
    Ok(())
}

fn validate_decoded_mpeg_spec(
    decoded: &symphonia::core::audio::GenericAudioBufferRef<'_>,
    stream: Fsb5MpegStream,
    header: MpegFrameHeader,
) -> Result<()> {
    if decoded.spec().rate() != stream.sample_rate
        || decoded.spec().channels().count() != usize::from(stream.channels)
    {
        return Err(Error::invalid_data(
            "decoded MPEG signal specification differs from FSB5 metadata",
        ));
    }
    if decoded.frames() != usize::from(header.samples) {
        return Err(Error::invalid_data(format!(
            "decoded MPEG frame produced {} samples instead of {}",
            decoded.frames(),
            header.samples
        )));
    }
    Ok(())
}

fn opus_wave_output_size(payload: &Region, stream: Fsb5OpusStream) -> Result<u64> {
    validate_opus_source(payload, stream)?;
    let data_length = stream
        .frame_count
        .checked_mul(u64::from(stream.channels))
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| Error::invalid_data("FSB5 Opus decoded byte count overflowed"))?;
    let data_size = u32::try_from(data_length)
        .map_err(|_| Error::invalid_data("WAV PCM payload exceeds its 32-bit data chunk"))?;
    WAV_HEADER_BYTES
        .checked_add(u64::from(data_size))
        .ok_or_else(|| Error::invalid_data("WAV output size overflowed"))
}

fn validate_opus_source(payload: &Region, stream: Fsb5OpusStream) -> Result<()> {
    if stream.channels == 0 || stream.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 Opus channels and frame count must be nonzero",
        ));
    }
    if stream.channels > MAX_OPUS_CHANNELS {
        return Err(Error::unsupported(
            "FSB5 multistream Opus decoding is not implemented",
        ));
    }
    if stream.sample_rate != 48_000 {
        return Err(Error::unsupported(
            "FSB5 Opus decoding is verified only for 48000 Hz streams",
        ));
    }
    let data_end = stream
        .data_offset
        .checked_add(stream.data_length)
        .ok_or_else(|| Error::invalid_data("FSB5 Opus source range overflowed"))?;
    if data_end > payload.len() {
        return Err(Error::invalid_data(format!(
            "FSB5 Opus source range {}..{data_end} exceeds payload length {}",
            stream.data_offset,
            payload.len()
        )));
    }
    let scan = scan_opus_packets(
        payload,
        stream.data_offset,
        stream.data_length,
        stream.frame_count,
        stream.encoder_delay,
    )?;
    if scan.compressed_length != stream.compressed_length {
        return Err(Error::invalid_data(
            "FSB5 Opus stream metadata does not match its encoded packets",
        ));
    }
    Ok(())
}

fn scan_opus_packets(
    payload: &Region,
    data_offset: u64,
    data_length: u64,
    frame_count: u64,
    encoder_delay: u64,
) -> Result<OpusScan> {
    let data_end = data_offset
        .checked_add(data_length)
        .ok_or_else(|| Error::invalid_data("FSB5 Opus source range overflowed"))?;
    if data_end > payload.len() {
        return Err(Error::invalid_data("FSB5 Opus source exceeds its payload"));
    }
    let required_frames = frame_count
        .checked_add(encoder_delay)
        .ok_or_else(|| Error::invalid_data("FSB5 Opus sample count overflowed"))?;
    let mut packet_bytes = opus_packet_buffer()?;
    let mut offset = data_offset;
    let mut decoded_frames = 0_u64;
    let mut compressed_end = data_offset;
    while offset < data_end {
        if data_end - offset < 2 {
            validate_opus_padding(payload, offset, data_end)?;
            break;
        }
        let packet_size = usize::from(read_u16_le(payload, offset)?);
        offset += 2;
        compressed_end = offset;
        if packet_size == 0 {
            validate_opus_padding(payload, offset, data_end)?;
            break;
        }
        let packet_size_u64 = u64::try_from(packet_size).expect("u16 packet size fits u64");
        let packet_end = offset
            .checked_add(packet_size_u64)
            .ok_or_else(|| Error::invalid_data("FSB5 Opus packet range overflowed"))?;
        if packet_end > data_end {
            return Err(Error::invalid_data(format!(
                "FSB5 Opus packet at {} declares {packet_size} bytes beyond its source range",
                offset - 2
            )));
        }
        payload.read_exact_at(offset, &mut packet_bytes[..packet_size])?;
        let packet = OpusPacket::parse(&packet_bytes[..packet_size])
            .map_err(|error| Error::invalid_data(format!("invalid FSB5 Opus packet: {error}")))?;
        let packet_frames = packet
            .toc()
            .frame_size()
            .samples_per_channel_48k()
            .checked_mul(packet.frames().len())
            .ok_or_else(|| Error::invalid_data("FSB5 Opus packet sample count overflowed"))?;
        decoded_frames = decoded_frames
            .checked_add(
                u64::try_from(packet_frames).expect("bounded Opus packet frame count fits u64"),
            )
            .ok_or_else(|| Error::invalid_data("FSB5 Opus sample count overflowed"))?;
        offset = packet_end;
        compressed_end = offset;
    }
    if decoded_frames < required_frames {
        return Err(Error::invalid_data(format!(
            "FSB5 Opus packets decode to {decoded_frames} frames but {required_frames} are required including encoder delay"
        )));
    }
    Ok(OpusScan {
        compressed_length: compressed_end - data_offset,
        decoded_frames,
    })
}

fn opus_packet_buffer() -> Result<Vec<u8>> {
    let mut packet = Vec::new();
    packet
        .try_reserve_exact(MAX_OPUS_PACKET_BYTES)
        .map_err(|_| Error::invalid_data("cannot allocate the bounded FSB5 Opus packet buffer"))?;
    packet.resize(MAX_OPUS_PACKET_BYTES, 0);
    Ok(packet)
}

fn validate_opus_padding(payload: &Region, offset: u64, end: u64) -> Result<()> {
    let length = end
        .checked_sub(offset)
        .ok_or_else(|| Error::invalid_data("FSB5 Opus padding range is reversed"))?;
    if length >= 32 {
        return Err(Error::invalid_data(
            "FSB5 Opus source has more than 31 trailing alignment bytes",
        ));
    }
    let mut byte = [0_u8; 1];
    for position in offset..end {
        payload.read_exact_at(position, &mut byte)?;
        if byte[0] != 0 {
            return Err(Error::invalid_data(
                "FSB5 Opus trailing alignment contains nonzero bytes",
            ));
        }
    }
    Ok(())
}

fn write_fsb5_opus_wav(
    payload: &Region,
    stream: Fsb5OpusStream,
    maximum_output_bytes: u64,
    output: &mut impl Write,
) -> Result<u64> {
    let output_size = opus_wave_output_size(payload, stream)?;
    if output_size > maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "WAV output is {output_size} bytes, exceeding limit {maximum_output_bytes}"
        )));
    }
    let data_size = u32::try_from(output_size - WAV_HEADER_BYTES)
        .expect("Opus WAV output size validation bounded the data chunk");
    write_wav_header(
        output,
        data_size,
        PcmSampleFormat::Signed16.wave_format(),
        stream.channels,
        stream.sample_rate,
        16,
    )?;
    decode_fsb5_opus(payload, stream, output)?;
    Ok(output_size)
}

fn decode_fsb5_opus(
    payload: &Region,
    stream: Fsb5OpusStream,
    output: &mut impl Write,
) -> Result<()> {
    let mut decoder = OpusDecoder::new(usize::from(stream.channels));
    let mut packet_bytes = opus_packet_buffer()?;
    let mut offset = stream.data_offset;
    let mut skip_frames = stream.encoder_delay;
    let mut remaining_frames = stream.frame_count;
    while remaining_frames > 0 {
        let packet_size = usize::from(read_u16_le(payload, offset)?);
        offset += 2;
        if packet_size == 0 {
            return Err(Error::invalid_data(
                "FSB5 Opus stream ended before its declared sample count",
            ));
        }
        payload.read_exact_at(offset, &mut packet_bytes[..packet_size])?;
        let pcm_samples = catch_unwind(AssertUnwindSafe(|| {
            decoder.decode_packet_i16(&packet_bytes[..packet_size])
        }))
        .map_err(|_| Error::invalid_data("FSB5 Opus decoder panicked on a malformed packet"))?
        .map_err(|error| Error::invalid_data(format!("FSB5 Opus decode failed: {error}")))?;
        let channels = usize::from(stream.channels);
        if !pcm_samples.len().is_multiple_of(channels) {
            return Err(Error::invalid_data(
                "FSB5 Opus decoder produced a channel-misaligned sample buffer",
            ));
        }
        let decoded_frames = pcm_samples.len() / channels;
        let packet_skip = usize::try_from(skip_frames)
            .unwrap_or(usize::MAX)
            .min(decoded_frames);
        skip_frames -= u64::try_from(packet_skip).expect("packet frame count fits u64");
        let available_frames = decoded_frames - packet_skip;
        let write_frames =
            available_frames.min(usize::try_from(remaining_frames).unwrap_or(usize::MAX));
        let first_sample = packet_skip
            .checked_mul(channels)
            .ok_or_else(|| Error::invalid_data("FSB5 Opus PCM offset overflowed"))?;
        let sample_count = write_frames
            .checked_mul(channels)
            .ok_or_else(|| Error::invalid_data("FSB5 Opus PCM sample count overflowed"))?;
        write_i16_le_samples(
            &pcm_samples[first_sample..first_sample + sample_count],
            output,
        )?;
        remaining_frames -= u64::try_from(write_frames).expect("packet frame count fits u64");
        offset = offset
            .checked_add(u64::try_from(packet_size).expect("u16 packet size fits u64"))
            .ok_or_else(|| Error::invalid_data("FSB5 Opus packet offset overflowed"))?;
    }
    Ok(())
}

fn vorbis_wave_output_size(payload: &Region, stream: Fsb5VorbisStream) -> Result<u64> {
    validate_vorbis_source(payload, stream)?;
    let data_length = stream
        .frame_count
        .checked_mul(u64::from(stream.channels))
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| Error::invalid_data("FSB5 Vorbis decoded byte count overflowed"))?;
    let data_size = u32::try_from(data_length)
        .map_err(|_| Error::invalid_data("WAV PCM payload exceeds its 32-bit data chunk"))?;
    WAV_HEADER_BYTES
        .checked_add(u64::from(data_size))
        .ok_or_else(|| Error::invalid_data("WAV output size overflowed"))
}

fn validate_vorbis_source(payload: &Region, stream: Fsb5VorbisStream) -> Result<()> {
    if stream.channels == 0 || stream.frame_count == 0 {
        return Err(Error::invalid_data(
            "FSB5 Vorbis channels and frame count must be nonzero",
        ));
    }
    if stream.channels > MAX_VORBIS_CHANNELS {
        return Err(Error::unsupported(
            "FSB5 Vorbis channel count exceeds the verified decoder range",
        ));
    }
    let data_end = stream
        .data_offset
        .checked_add(stream.data_length)
        .ok_or_else(|| Error::invalid_data("FSB5 Vorbis source range overflowed"))?;
    if data_end > payload.len() {
        return Err(Error::invalid_data(format!(
            "FSB5 Vorbis source range {}..{data_end} exceeds payload length {}",
            stream.data_offset,
            payload.len()
        )));
    }
    let (identification, setup) =
        vorbis_headers(stream.channels, stream.sample_rate, stream.setup_crc)?;
    let scan = scan_vorbis_packets(
        payload,
        stream.data_offset,
        stream.data_length,
        stream.frame_count,
        &identification,
        &setup,
    )?;
    if scan.compressed_length != stream.compressed_length
        || scan.decoded_frames < stream.frame_count
    {
        return Err(Error::invalid_data(
            "FSB5 Vorbis stream metadata does not match its encoded packets",
        ));
    }
    Ok(())
}

fn vorbis_headers(
    channels: u16,
    sample_rate: u32,
    setup_crc: u32,
) -> Result<(IdentHeader, SetupHeader)> {
    let channels_u8 = u8::try_from(channels)
        .map_err(|_| Error::unsupported("FSB5 Vorbis channel count exceeds u8"))?;
    let setup_packet = setup_header(setup_crc).ok_or_else(|| {
        Error::unsupported(format!(
            "FSB5 Vorbis setup header {setup_crc:08x} is not in the bundled dictionary"
        ))
    })?;
    let mut identification_packet = [0_u8; 30];
    identification_packet[0] = 1;
    identification_packet[1..7].copy_from_slice(b"vorbis");
    identification_packet[11] = channels_u8;
    identification_packet[12..16].copy_from_slice(&sample_rate.to_le_bytes());
    identification_packet[28] =
        (FSB5_VORBIS_LONG_BLOCK_EXPONENT << 4) | FSB5_VORBIS_SHORT_BLOCK_EXPONENT;
    identification_packet[29] = 1;
    catch_unwind(AssertUnwindSafe(|| {
        let identification = read_header_ident(&identification_packet).map_err(|error| {
            Error::invalid_data(format!("FSB5 Vorbis identity header: {error}"))
        })?;
        let setup = read_header_setup(
            setup_packet,
            channels_u8,
            (
                FSB5_VORBIS_SHORT_BLOCK_EXPONENT,
                FSB5_VORBIS_LONG_BLOCK_EXPONENT,
            ),
        )
        .map_err(|error| Error::invalid_data(format!("FSB5 Vorbis setup header: {error}")))?;
        Ok((identification, setup))
    }))
    .map_err(|_| Error::invalid_data("FSB5 Vorbis header decoder panicked"))?
}

fn scan_vorbis_packets(
    payload: &Region,
    data_offset: u64,
    data_length: u64,
    frame_count: u64,
    identification: &IdentHeader,
    setup: &SetupHeader,
) -> Result<VorbisScan> {
    let data_end = data_offset
        .checked_add(data_length)
        .ok_or_else(|| Error::invalid_data("FSB5 Vorbis source range overflowed"))?;
    if data_end > payload.len() {
        return Err(Error::invalid_data(
            "FSB5 Vorbis source exceeds its payload",
        ));
    }
    let maximum_packets = frame_count
        .div_ceil(FSB5_VORBIS_MIN_PACKET_FRAMES)
        .checked_add(2)
        .ok_or_else(|| Error::invalid_data("FSB5 Vorbis packet limit overflowed"))?;
    let mut packet_bytes = vorbis_packet_buffer()?;
    let mut offset = data_offset;
    let mut decoded_frames = 0_u64;
    let mut compressed_end = data_offset;
    let mut packet_count = 0_u64;
    while offset < data_end {
        if data_end - offset < 2 {
            validate_vorbis_padding(payload, offset, data_end)?;
            break;
        }
        let packet_size = usize::from(read_u16_le(payload, offset)?);
        offset += 2;
        compressed_end = offset;
        if packet_size == 0 || packet_size == usize::from(u16::MAX) {
            validate_vorbis_padding(payload, offset, data_end)?;
            break;
        }
        packet_count += 1;
        if packet_count > maximum_packets {
            return Err(Error::invalid_data(format!(
                "FSB5 Vorbis packet count exceeds the bound {maximum_packets} derived from its sample count"
            )));
        }
        let packet_end = offset
            .checked_add(u64::try_from(packet_size).expect("u16 packet size fits u64"))
            .ok_or_else(|| Error::invalid_data("FSB5 Vorbis packet range overflowed"))?;
        if packet_end > data_end {
            return Err(Error::invalid_data(format!(
                "FSB5 Vorbis packet at {} declares {packet_size} bytes beyond its source range",
                offset - 2
            )));
        }
        payload.read_exact_at(offset, &mut packet_bytes[..packet_size])?;
        let frames = catch_unwind(AssertUnwindSafe(|| {
            get_decoded_sample_count(identification, setup, &packet_bytes[..packet_size])
        }))
        .map_err(|_| Error::invalid_data("FSB5 Vorbis packet scanner panicked"))?
        .map_err(|error| Error::invalid_data(format!("invalid FSB5 Vorbis packet: {error}")))?;
        if packet_count > 1 {
            decoded_frames = decoded_frames
                .checked_add(u64::try_from(frames).map_err(|_| {
                    Error::invalid_data("FSB5 Vorbis packet sample count exceeds u64")
                })?)
                .ok_or_else(|| Error::invalid_data("FSB5 Vorbis sample count overflowed"))?;
        }
        offset = packet_end;
        compressed_end = offset;
    }
    Ok(VorbisScan {
        compressed_length: compressed_end - data_offset,
        decoded_frames,
    })
}

fn vorbis_packet_buffer() -> Result<Vec<u8>> {
    let mut packet = Vec::new();
    packet
        .try_reserve_exact(MAX_VORBIS_PACKET_BYTES)
        .map_err(|_| {
            Error::invalid_data("cannot allocate the bounded FSB5 Vorbis packet buffer")
        })?;
    packet.resize(MAX_VORBIS_PACKET_BYTES, 0);
    Ok(packet)
}

fn validate_vorbis_padding(payload: &Region, offset: u64, end: u64) -> Result<()> {
    let length = end
        .checked_sub(offset)
        .ok_or_else(|| Error::invalid_data("FSB5 Vorbis padding range is reversed"))?;
    if length > MAX_VORBIS_PADDING_BYTES {
        return Err(Error::invalid_data(format!(
            "FSB5 Vorbis source has more than {MAX_VORBIS_PADDING_BYTES} trailing alignment bytes"
        )));
    }
    let mut byte = [0_u8; 1];
    for position in offset..end {
        payload.read_exact_at(position, &mut byte)?;
        if byte[0] != 0 {
            return Err(Error::invalid_data(
                "FSB5 Vorbis trailing alignment contains nonzero bytes",
            ));
        }
    }
    Ok(())
}

fn write_fsb5_vorbis_wav(
    payload: &Region,
    stream: Fsb5VorbisStream,
    maximum_output_bytes: u64,
    output: &mut impl Write,
) -> Result<u64> {
    let output_size = vorbis_wave_output_size(payload, stream)?;
    if output_size > maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "WAV output is {output_size} bytes, exceeding limit {maximum_output_bytes}"
        )));
    }
    let data_size = u32::try_from(output_size - WAV_HEADER_BYTES)
        .expect("Vorbis WAV output size validation bounded the data chunk");
    write_wav_header(
        output,
        data_size,
        PcmSampleFormat::Signed16.wave_format(),
        stream.channels,
        stream.sample_rate,
        16,
    )?;
    decode_fsb5_vorbis(payload, stream, output)?;
    Ok(output_size)
}

fn decode_fsb5_vorbis(
    payload: &Region,
    stream: Fsb5VorbisStream,
    output: &mut impl Write,
) -> Result<()> {
    let (identification, setup) =
        vorbis_headers(stream.channels, stream.sample_rate, stream.setup_crc)?;
    let mut previous_window = PreviousWindowRight::new();
    let mut packet_bytes = vorbis_packet_buffer()?;
    let mut pcm_bytes = Vec::new();
    let mut offset = stream.data_offset;
    let mut remaining_frames = stream.frame_count;
    while remaining_frames > 0 {
        let packet_size = usize::from(read_u16_le(payload, offset)?);
        offset += 2;
        if packet_size == 0 || packet_size == usize::from(u16::MAX) {
            return Err(Error::invalid_data(
                "FSB5 Vorbis stream ended before its declared sample count",
            ));
        }
        payload.read_exact_at(offset, &mut packet_bytes[..packet_size])?;
        let channels = catch_unwind(AssertUnwindSafe(|| {
            read_audio_packet(
                &identification,
                &setup,
                &packet_bytes[..packet_size],
                &mut previous_window,
            )
        }))
        .map_err(|_| Error::invalid_data("FSB5 Vorbis decoder panicked on a malformed packet"))?
        .map_err(|error| Error::invalid_data(format!("FSB5 Vorbis decode failed: {error}")))?;
        if channels.len() != usize::from(stream.channels) {
            return Err(Error::invalid_data(
                "FSB5 Vorbis decoder produced the wrong channel count",
            ));
        }
        let decoded_frames = channels.first().map_or(0, Vec::len);
        if channels
            .iter()
            .any(|channel| channel.len() != decoded_frames)
        {
            return Err(Error::invalid_data(
                "FSB5 Vorbis decoder produced channel lengths that differ",
            ));
        }
        let write_frames =
            decoded_frames.min(usize::try_from(remaining_frames).unwrap_or(usize::MAX));
        let byte_count = write_frames
            .checked_mul(usize::from(stream.channels))
            .and_then(|value| value.checked_mul(2))
            .ok_or_else(|| Error::invalid_data("FSB5 Vorbis PCM byte count overflowed"))?;
        pcm_bytes.clear();
        if pcm_bytes.capacity() < byte_count {
            pcm_bytes
                .try_reserve_exact(byte_count - pcm_bytes.capacity())
                .map_err(|_| {
                    Error::invalid_data("cannot allocate the FSB5 Vorbis PCM packet buffer")
                })?;
        }
        for frame in 0..write_frames {
            for channel in &channels {
                pcm_bytes.extend_from_slice(&channel[frame].to_le_bytes());
            }
        }
        output.write_all(&pcm_bytes)?;
        remaining_frames -= u64::try_from(write_frames).expect("Vorbis frame count fits u64");
        offset = offset
            .checked_add(u64::try_from(packet_size).expect("u16 packet size fits u64"))
            .ok_or_else(|| Error::invalid_data("FSB5 Vorbis packet offset overflowed"))?;
    }
    Ok(())
}

fn write_i16_le_samples(samples: &[i16], output: &mut impl Write) -> Result<()> {
    let mut bytes = [0_u8; 4096];
    for chunk in samples.chunks(bytes.len() / 2) {
        for (sample, destination) in chunk.iter().zip(bytes.chunks_exact_mut(2)) {
            destination.copy_from_slice(&sample.to_le_bytes());
        }
        output.write_all(&bytes[..chunk.len() * 2])?;
    }
    Ok(())
}

fn write_pcm_wav(
    payload: &Region,
    source: PcmWriteSource,
    maximum_output_bytes: u64,
    output: &mut impl Write,
) -> Result<u64> {
    let layout = pcm_wave_layout(payload, source)?;
    if layout.output_size > maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "WAV output is {} bytes, exceeding limit {maximum_output_bytes}",
            layout.output_size
        )));
    }

    write_wav_header(
        output,
        layout.data_size,
        layout.output_format.wave_format(),
        source.channels,
        source.sample_rate,
        layout.output_width * 8,
    )?;
    if source.convert_float_to_pcm16 {
        write_float_as_pcm16(
            payload,
            source.data_offset,
            source.data_length,
            source.big_endian,
            output,
        )?;
    } else if source.big_endian && source.sample_format.byte_width() > 1 {
        write_byte_swapped(
            payload,
            source.data_offset,
            source.data_length,
            usize::from(source.sample_format.byte_width()),
            output,
        )?;
    } else {
        payload.copy_range(source.data_offset, source.data_length, output)?;
    }
    Ok(layout.output_size)
}

fn pcm_wave_layout(payload: &Region, source: PcmWriteSource) -> Result<PcmWaveLayout> {
    let source_end = source
        .data_offset
        .checked_add(source.data_length)
        .ok_or_else(|| Error::invalid_data("PCM source range overflowed"))?;
    if source_end > payload.len() {
        return Err(Error::invalid_data(format!(
            "PCM source range {}..{source_end} exceeds payload length {}",
            source.data_offset,
            payload.len()
        )));
    }
    if source.channels == 0 || source.sample_rate == 0 {
        return Err(Error::invalid_data(
            "WAV channel count and sample rate must be nonzero",
        ));
    }
    let source_width = source.sample_format.byte_width();
    let source_frame_width = source
        .channels
        .checked_mul(source_width)
        .ok_or_else(|| Error::invalid_data("WAV source frame size overflowed"))?;
    if !source
        .data_length
        .is_multiple_of(u64::from(source_frame_width))
    {
        return Err(Error::invalid_data(format!(
            "PCM payload size {} is not a whole {source_frame_width}-byte sample frame",
            source.data_length
        )));
    }
    let output_format = if source.convert_float_to_pcm16 {
        PcmSampleFormat::Signed16
    } else {
        source.sample_format
    };
    let output_width = output_format.byte_width();
    let frame_count = source.data_length / u64::from(source_frame_width);
    let output_frame_width = source
        .channels
        .checked_mul(output_width)
        .ok_or_else(|| Error::invalid_data("WAV output frame size overflowed"))?;
    let output_data_length = frame_count
        .checked_mul(u64::from(output_frame_width))
        .ok_or_else(|| Error::invalid_data("WAV output data size overflowed"))?;
    let data_size = u32::try_from(output_data_length)
        .map_err(|_| Error::invalid_data("WAV PCM payload exceeds its 32-bit data chunk"))?;
    let output_size = WAV_HEADER_BYTES
        .checked_add(output_data_length)
        .ok_or_else(|| Error::invalid_data("WAV output size overflowed"))?;
    Ok(PcmWaveLayout {
        output_format,
        output_width,
        data_size,
        output_size,
    })
}

fn write_wav_header(
    output: &mut impl Write,
    data_size: u32,
    audio_format: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
) -> Result<()> {
    let block_align = channels
        .checked_mul(bits_per_sample / 8)
        .ok_or_else(|| Error::invalid_data("WAV block alignment overflowed"))?;
    let byte_rate = sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or_else(|| Error::invalid_data("WAV byte rate overflowed"))?;
    let chunk_size = data_size
        .checked_add(36)
        .ok_or_else(|| Error::invalid_data("WAV RIFF chunk size overflowed"))?;
    let mut header = [0_u8; 44];
    header[..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&chunk_size.to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"fmt ");
    header[16..20].copy_from_slice(&16_u32.to_le_bytes());
    header[20..22].copy_from_slice(&audio_format.to_le_bytes());
    header[22..24].copy_from_slice(&channels.to_le_bytes());
    header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    header[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    header[32..34].copy_from_slice(&block_align.to_le_bytes());
    header[34..36].copy_from_slice(&bits_per_sample.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data_size.to_le_bytes());
    output.write_all(&header)?;
    Ok(())
}

fn write_byte_swapped(
    payload: &Region,
    data_offset: u64,
    data_length: u64,
    width: usize,
    output: &mut impl Write,
) -> Result<()> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut consumed = 0_u64;
    while consumed < data_length {
        let remaining = data_length - consumed;
        let maximum = remaining.min(u64::try_from(buffer.len()).expect("buffer size fits u64"));
        let chunk_length = usize::try_from(maximum).expect("chunk is bounded by fixed buffer");
        let chunk_length = chunk_length / width * width;
        if chunk_length == 0 {
            return Err(Error::invalid_data(
                "PCM byte-swap input is not sample-aligned",
            ));
        }
        payload.read_exact_at(data_offset + consumed, &mut buffer[..chunk_length])?;
        for sample in buffer[..chunk_length].chunks_exact_mut(width) {
            sample.reverse();
        }
        output.write_all(&buffer[..chunk_length])?;
        consumed += u64::try_from(chunk_length).expect("chunk size fits u64");
    }
    Ok(())
}

fn write_float_as_pcm16(
    payload: &Region,
    data_offset: u64,
    data_length: u64,
    big_endian: bool,
    output: &mut impl Write,
) -> Result<()> {
    let mut input = [0_u8; 4096];
    let mut converted = [0_u8; 2048];
    let mut consumed = 0_u64;
    while consumed < data_length {
        let chunk_length = usize::try_from(
            (data_length - consumed).min(u64::try_from(input.len()).expect("buffer size fits u64")),
        )
        .expect("chunk is bounded by fixed buffer");
        if !chunk_length.is_multiple_of(4) {
            return Err(Error::invalid_data("PCM-float data is not 32-bit aligned"));
        }
        payload.read_exact_at(data_offset + consumed, &mut input[..chunk_length])?;
        let output_length = chunk_length / 2;
        for (source, destination) in input[..chunk_length]
            .chunks_exact(4)
            .zip(converted[..output_length].chunks_exact_mut(2))
        {
            let bytes: [u8; 4] = source.try_into().expect("four-byte float chunk");
            let value = if big_endian {
                f32::from_be_bytes(bytes)
            } else {
                f32::from_le_bytes(bytes)
            };
            let scaled = value * f32::from(i16::MAX);
            let sample = clamped_float_to_i16(scaled);
            destination.copy_from_slice(&sample.to_le_bytes());
        }
        output.write_all(&converted[..output_length])?;
        consumed += u64::try_from(chunk_length).expect("chunk size fits u64");
    }
    Ok(())
}

fn is_riff_wave(payload: &Region) -> Result<bool> {
    if payload.len() < 12 {
        return Ok(false);
    }
    let mut header = [0_u8; 12];
    payload.read_exact_at(0, &mut header)?;
    Ok(header[..4] == *b"RIFF" && header[8..] == *b"WAVE")
}

#[allow(clippy::cast_possible_truncation)]
fn clamped_float_to_i16(value: f32) -> i16 {
    if value.is_nan() {
        return 0;
    }
    // The explicit clamp proves that Rust's saturating float-to-int cast can
    // only discard the fractional component, matching the managed exporter.
    value.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

fn has_magic(payload: &Region, magic: [u8; 4]) -> Result<bool> {
    if payload.len() < 4 {
        return Ok(false);
    }
    let mut actual = [0_u8; 4];
    payload.read_exact_at(0, &mut actual)?;
    Ok(actual == magic)
}

const fn compact_sample_rate(code: u8) -> Option<u32> {
    match code {
        0 => Some(4_000),
        1 => Some(8_000),
        2 => Some(11_000),
        3 => Some(11_025),
        4 => Some(16_000),
        5 => Some(22_050),
        6 => Some(24_000),
        7 => Some(32_000),
        8 => Some(44_100),
        9 => Some(48_000),
        10 => Some(96_000),
        _ => None,
    }
}

fn read_u8(payload: &Region, offset: u64) -> Result<u8> {
    let mut bytes = [0_u8; 1];
    payload.read_exact_at(offset, &mut bytes)?;
    Ok(bytes[0])
}

fn read_i16_le(payload: &Region, offset: u64) -> Result<i16> {
    let mut bytes = [0_u8; 2];
    payload.read_exact_at(offset, &mut bytes)?;
    Ok(i16::from_le_bytes(bytes))
}

fn read_i16_be(payload: &Region, offset: u64) -> Result<i16> {
    let mut bytes = [0_u8; 2];
    payload.read_exact_at(offset, &mut bytes)?;
    Ok(i16::from_be_bytes(bytes))
}

fn read_u16_le(payload: &Region, offset: u64) -> Result<u16> {
    let mut bytes = [0_u8; 2];
    payload.read_exact_at(offset, &mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32_le(payload: &Region, offset: u64) -> Result<u32> {
    let mut bytes = [0_u8; 4];
    payload.read_exact_at(offset, &mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u32_le_bounded(payload: &Region, offset: u64, end: u64, field: &str) -> Result<u32> {
    let range_end = offset
        .checked_add(4)
        .ok_or_else(|| Error::invalid_data(format!("{field} range overflowed")))?;
    if range_end > end {
        return Err(Error::invalid_data(format!("{field} is truncated")));
    }
    read_u32_le(payload, offset)
}

fn read_u64_le_bounded(payload: &Region, offset: u64, end: u64, field: &str) -> Result<u64> {
    let range_end = offset
        .checked_add(8)
        .ok_or_else(|| Error::invalid_data(format!("{field} range overflowed")))?;
    if range_end > end {
        return Err(Error::invalid_data(format!("{field} is truncated")));
    }
    let mut bytes = [0_u8; 8];
    payload.read_exact_at(offset, &mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::{
        DirectWavKind, Fsb5DspStream, Fsb5FadpcmStream, Fsb5HevagStream, Fsb5ImaStream,
        Fsb5MpegLayer, Fsb5MpegStream, Fsb5OpusStream, Fsb5PcmStream, Fsb5VagStream,
        Fsb5VorbisStream, PcmSampleFormat, detect_direct_wav, parse_fsb5_dsp, parse_fsb5_fadpcm,
        parse_fsb5_hevag, parse_fsb5_ima, parse_fsb5_mpeg, parse_fsb5_opus, parse_fsb5_pcm,
        parse_fsb5_vag, parse_fsb5_vorbis, parse_mpeg_frame_header, write_direct_wav,
    };
    use crate::source::Region;

    type SampleSpec = (u64, u16, u32, Option<(u16, u32)>);

    const FSB5_VORBIS_FIXTURE: &[u8] =
        include_bytes!("../tests/fixtures/audio/fsb5-vorbis-stereo.fsb");

    #[test]
    fn writes_little_and_big_endian_fsb5_pcm16() {
        let little = fsb5(1, 2, 0, &[(2, 2, 44_100, None)], &[1, 2, 3, 4, 5, 6, 7, 8]);
        let little = Region::from_bytes(little);
        let kind = detect_direct_wav(&little, Some(16)).unwrap().unwrap();
        let mut wav = Vec::new();
        assert_eq!(write_direct_wav(&little, kind, 52, &mut wav).unwrap(), 52);
        assert_eq!(&wav[20..24], &[1, 0, 2, 0]);
        assert_eq!(&wav[24..28], &44_100_u32.to_le_bytes());
        assert_eq!(&wav[34..36], &16_u16.to_le_bytes());
        assert_eq!(&wav[44..], &[1, 2, 3, 4, 5, 6, 7, 8]);

        let big = fsb5(1, 2, 1, &[(2, 1, 48_000, None)], &[0x12, 0x34, 0xab, 0xcd]);
        let big = Region::from_bytes(big);
        let kind = detect_direct_wav(&big, Some(16)).unwrap().unwrap();
        let mut wav = Vec::new();
        write_direct_wav(&big, kind, 48, &mut wav).unwrap();
        assert_eq!(&wav[44..], &[0x34, 0x12, 0xcd, 0xab]);
    }

    #[test]
    fn writes_all_decoder_free_fsb5_pcm_widths() {
        let cases = [
            (1, PcmSampleFormat::Unsigned8, vec![0, 127, 255], 8),
            (3, PcmSampleFormat::Signed24, vec![1, 2, 3, 4, 5, 6], 24),
            (
                4,
                PcmSampleFormat::Signed32,
                vec![1, 2, 3, 4, 5, 6, 7, 8],
                32,
            ),
        ];
        for (codec, format, bytes, bits) in cases {
            let frames = u64::try_from(bytes.len()).unwrap() / u64::from(format.byte_width());
            let fsb = Region::from_bytes(fsb5(1, codec, 0, &[(frames, 1, 8_000, None)], &bytes));
            let stream = parse_fsb5_pcm(&fsb, None).unwrap().unwrap();
            assert_eq!(stream.sample_format, format);
            let mut wav = Vec::new();
            write_direct_wav(&fsb, DirectWavKind::Fsb5Pcm(stream), 128, &mut wav).unwrap();
            assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), bits);
            assert_eq!(&wav[44..], bytes);
        }
    }

    #[test]
    fn converts_pcm_float_to_pcm16_or_preserves_ieee_float() {
        let mut data = Vec::new();
        for value in [-1.5_f32, -0.5, 0.5, 1.5, f32::NAN] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let fsb = Region::from_bytes(fsb5(1, 5, 0, &[(5, 1, 44_100, None)], &data));

        let pcm16 = parse_fsb5_pcm(&fsb, Some(16)).unwrap().unwrap();
        assert!(pcm16.convert_float_to_pcm16);
        let mut wav = Vec::new();
        write_direct_wav(&fsb, DirectWavKind::Fsb5Pcm(pcm16), 64, &mut wav).unwrap();
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        let samples: Vec<i16> = wav[44..]
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        assert_eq!(samples, [i16::MIN, -16_383, 16_383, i16::MAX, 0]);

        let float = parse_fsb5_pcm(&fsb, Some(32)).unwrap().unwrap();
        assert!(!float.convert_float_to_pcm16);
        let mut wav = Vec::new();
        write_direct_wav(&fsb, DirectWavKind::Fsb5Pcm(float), 64, &mut wav).unwrap();
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 3);
        assert_eq!(&wav[44..], data);
    }

    #[test]
    fn selects_first_subsound_and_applies_metadata_overrides() {
        let mut data = vec![0_u8; 32];
        data[..4].copy_from_slice(&[1, 2, 3, 4]);
        data.extend_from_slice(&[9, 10]);
        let fsb = Region::from_bytes(fsb5(
            1,
            2,
            0,
            &[(1, 2, 44_100, Some((3, 12_345))), (1, 1, 8_000, None)],
            &data,
        ));
        let stream = parse_fsb5_pcm(&fsb, None).unwrap().unwrap();
        assert_eq!(stream.channels, 3);
        assert_eq!(stream.sample_rate, 12_345);
        assert_eq!(stream.data_length, 6);
        let mut wav = Vec::new();
        write_direct_wav(&fsb, DirectWavKind::Fsb5Pcm(stream), 64, &mut wav).unwrap();
        assert_eq!(&wav[44..], &[1, 2, 3, 4, 0, 0]);
    }

    #[test]
    fn compressed_and_malformed_fsb5_do_not_claim_direct_wav() {
        let compressed = Region::from_bytes(fsb5(1, 15, 0, &[(1, 1, 8_000, None)], &[1]));
        assert_eq!(detect_direct_wav(&compressed, Some(16)).unwrap(), None);

        let mut malformed = fsb5(1, 2, 0, &[(1, 1, 8_000, None)], &[1, 2]);
        malformed.pop();
        let malformed = Region::from_bytes(malformed);
        assert!(parse_fsb5_pcm(&malformed, None).is_err());
        assert_eq!(detect_direct_wav(&malformed, None).unwrap(), None);
    }

    #[test]
    fn enforces_wav_output_limit_before_writing() {
        let fsb = Region::from_bytes(fsb5(1, 2, 0, &[(1, 1, 8_000, None)], &[1, 2]));
        let stream = parse_fsb5_pcm(&fsb, None).unwrap().unwrap();
        let mut output = Vec::new();
        assert!(write_direct_wav(&fsb, DirectWavKind::Fsb5Pcm(stream), 45, &mut output).is_err());
        assert!(output.is_empty());

        let forged = DirectWavKind::Fsb5Pcm(Fsb5PcmStream {
            data_offset: fsb.len(),
            data_length: 2,
            channels: 1,
            sample_rate: 8_000,
            sample_format: PcmSampleFormat::Signed16,
            big_endian: false,
            convert_float_to_pcm16: false,
        });
        assert!(write_direct_wav(&fsb, forged, 128, &mut output).is_err());
        assert!(output.is_empty());
    }

    #[test]
    fn decodes_mono_stereo_and_multichannel_fsb5_ima() {
        for channels in [1_u16, 2, 6] {
            let block = ima_block(channels);
            let fsb = Region::from_bytes(fsb5(1, 7, 0, &[(64, channels, 44_100, None)], &block));
            let stream = parse_fsb5_ima(&fsb).unwrap().unwrap();
            assert_eq!(stream.channels, channels);
            assert_eq!(stream.frame_count, 64);
            assert_eq!(
                stream.compressed_length,
                u64::from(channels).checked_mul(36).unwrap()
            );
            assert!(matches!(
                detect_direct_wav(&fsb, Some(16)).unwrap(),
                Some(DirectWavKind::Fsb5Ima(_))
            ));

            let expected_size = 44 + 64 * u64::from(channels) * 2;
            let mut wav = Vec::new();
            assert_eq!(
                write_direct_wav(
                    &fsb,
                    DirectWavKind::Fsb5Ima(stream),
                    expected_size,
                    &mut wav,
                )
                .unwrap(),
                expected_size
            );
            assert_eq!(
                u16::from_le_bytes(wav[22..24].try_into().unwrap()),
                channels
            );
            assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
            assert_eq!(u64::try_from(wav.len()).unwrap(), expected_size);

            let pcm = wave_data(&wav);
            for channel in 0..usize::from(channels) {
                let first =
                    i16::from_le_bytes(pcm[channel * 2..channel * 2 + 2].try_into().unwrap());
                let second_offset = usize::from(channels) * 2 + channel * 2;
                let second =
                    i16::from_le_bytes(pcm[second_offset..second_offset + 2].try_into().unwrap());
                let history = ima_history(channel);
                assert_eq!(first, history);
                assert_eq!(second, history + ima_step(channel) / 8);
            }
        }
    }

    #[test]
    fn rejects_truncated_forged_and_over_budget_fsb5_ima_before_writing() {
        let block = ima_block(2);
        let fsb_bytes = fsb5(1, 7, 0, &[(64, 2, 44_100, None)], &block);
        let fsb = Region::from_bytes(fsb_bytes.clone());
        let stream = parse_fsb5_ima(&fsb).unwrap().unwrap();
        let mut output = Vec::new();
        let output_size = 44 + 64 * 2 * 2;
        assert!(
            write_direct_wav(
                &fsb,
                DirectWavKind::Fsb5Ima(stream),
                output_size - 1,
                &mut output,
            )
            .is_err()
        );
        assert!(output.is_empty());

        let forged = DirectWavKind::Fsb5Ima(Fsb5ImaStream {
            compressed_length: stream.compressed_length - 1,
            ..stream
        });
        assert!(write_direct_wav(&fsb, forged, output_size, &mut output).is_err());
        assert!(output.is_empty());

        let mut truncated = fsb_bytes;
        truncated.pop();
        assert!(parse_fsb5_ima(&Region::from_bytes(truncated)).is_err());
    }

    #[test]
    #[ignore = "requires the optional vgmstream-cli decoder oracle"]
    fn fsb5_ima_pcm_matches_vgmstream_oracle() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for channels in [1_u16, 2, 6] {
            let fsb_bytes = fsb5(
                1,
                7,
                0,
                &[(64, channels, 44_100, None)],
                &ima_block(channels),
            );
            let region = Region::from_bytes(fsb_bytes.clone());
            let kind = detect_direct_wav(&region, Some(16)).unwrap().unwrap();
            let mut actual = Vec::new();
            write_direct_wav(&region, kind, 1024 * 1024, &mut actual).unwrap();

            let input = std::env::temp_dir().join(format!(
                "assetstudio-fsb5-ima-{}-{unique}-{channels}.fsb",
                std::process::id()
            ));
            let output = input.with_extension("wav");
            std::fs::write(&input, fsb_bytes).unwrap();
            let status = Command::new("vgmstream-cli")
                .arg("-o")
                .arg(&output)
                .arg(&input)
                .status()
                .expect("vgmstream-cli must be installed to run this ignored oracle test");
            assert!(status.success());
            let expected = std::fs::read(&output).unwrap();
            assert_eq!(wave_data(&actual), wave_data(&expected));
            std::fs::remove_file(input).unwrap();
            std::fs::remove_file(output).unwrap();
        }
    }

    #[test]
    fn decodes_interleaved_and_planar_fsb5_dsp() {
        let mut decoded: Vec<Vec<u8>> = Vec::new();
        for (channels, non_interleaved) in [(1_u16, false), (2, false), (6, false), (2, true)] {
            let fsb = Region::from_bytes(fsb5_dsp(channels, non_interleaved));
            let stream = parse_fsb5_dsp(&fsb).unwrap().unwrap();
            assert_eq!(stream.channels, channels);
            assert_eq!(stream.non_interleaved, non_interleaved);
            assert_eq!(stream.frame_count, 14);
            assert_eq!(stream.compressed_length, u64::from(channels) * 8);
            assert!(matches!(
                detect_direct_wav(&fsb, Some(16)).unwrap(),
                Some(DirectWavKind::Fsb5Dsp(_))
            ));

            let expected_size = 44 + 14 * u64::from(channels) * 2;
            let mut wav = Vec::new();
            assert_eq!(
                write_direct_wav(
                    &fsb,
                    DirectWavKind::Fsb5Dsp(stream),
                    expected_size,
                    &mut wav,
                )
                .unwrap(),
                expected_size
            );
            assert_eq!(
                u16::from_le_bytes(wav[22..24].try_into().unwrap()),
                channels
            );
            assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
            let pcm = wave_data(&wav);
            // What this test is for is the wiring: which bytes belong to which
            // channel, and that the planar layout lands in the same place as
            // the interleaved one. It used to pin exact sample values, which
            // only worked because every frame header was zero -- predictor
            // pair 0, scale shift 0 -- and fifteen of each channel's sixteen
            // coefficients were zero too. The arithmetic those values pinned is
            // what the vgmstream comparison checks, on inputs that vary.
            let sample = |frame: usize, channel: usize| {
                let offset = (frame * usize::from(channels) + channel) * 2;
                i16::from_le_bytes(pcm[offset..offset + 2].try_into().unwrap())
            };
            let first: Vec<i16> = (0..usize::from(channels)).map(|c| sample(0, c)).collect();
            // Each channel carries a different header, different nibbles and
            // different coefficients, so no two may decode alike; equal
            // channels would mean one channel's bytes were read twice.
            for channel in 1..usize::from(channels) {
                assert_ne!(
                    first[channel], first[0],
                    "channel {channel} decoded the same as channel 0 at {channels}ch",
                );
            }
            // And the stream advances rather than repeating one sample.
            assert!(
                (0..usize::from(channels)).any(|c| sample(1, c) != sample(0, c)),
                "the second frame repeated the first at {channels}ch",
            );
            decoded.push(pcm.to_vec());
        }
        // The planar case carries the same channel data as the interleaved one
        // laid out differently, so it must decode to the same PCM. This is the
        // property the layout code exists for, and no expected-value table can
        // state it.
        assert_eq!(
            decoded[1], decoded[3],
            "planar stereo decoded differently from interleaved stereo"
        );
    }

    #[test]
    fn rejects_missing_coefficients_bad_predictors_and_forged_dsp_before_writing() {
        let fsb_bytes = fsb5_dsp(2, false);
        let fsb = Region::from_bytes(fsb_bytes.clone());
        let stream = parse_fsb5_dsp(&fsb).unwrap().unwrap();
        let output_size = 44 + 14 * 2 * 2;
        let mut output = Vec::new();
        assert!(
            write_direct_wav(
                &fsb,
                DirectWavKind::Fsb5Dsp(stream),
                output_size - 1,
                &mut output,
            )
            .is_err()
        );
        assert!(output.is_empty());

        let forged = DirectWavKind::Fsb5Dsp(Fsb5DspStream {
            coefficients_length: stream.coefficients_length - 1,
            ..stream
        });
        assert!(write_direct_wav(&fsb, forged, output_size, &mut output).is_err());
        assert!(output.is_empty());

        let data_offset = usize::try_from(stream.data_offset).unwrap();
        let mut bad_predictor = fsb_bytes;
        bad_predictor[data_offset] = 0x80;
        let bad_predictor = Region::from_bytes(bad_predictor);
        let stream = parse_fsb5_dsp(&bad_predictor).unwrap().unwrap();
        assert!(
            write_direct_wav(
                &bad_predictor,
                DirectWavKind::Fsb5Dsp(stream),
                output_size,
                &mut output,
            )
            .is_err()
        );
        assert!(output.is_empty());

        let mut no_coefficients = fsb5_dsp(1, false);
        no_coefficients[0x3c] &= !1;
        assert!(parse_fsb5_dsp(&Region::from_bytes(no_coefficients)).is_err());
    }

    #[test]
    #[ignore = "requires the optional vgmstream-cli decoder oracle"]
    fn fsb5_dsp_pcm_matches_vgmstream_oracle() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for (channels, non_interleaved) in [(1_u16, false), (2, false), (6, false), (2, true)] {
            let fsb_bytes = fsb5_dsp(channels, non_interleaved);
            let region = Region::from_bytes(fsb_bytes.clone());
            let kind = detect_direct_wav(&region, Some(16)).unwrap().unwrap();
            let mut actual = Vec::new();
            write_direct_wav(&region, kind, 1024 * 1024, &mut actual).unwrap();

            let layout = if non_interleaved { "planar" } else { "subint" };
            let input = std::env::temp_dir().join(format!(
                "assetstudio-fsb5-dsp-{}-{unique}-{channels}-{layout}.fsb",
                std::process::id()
            ));
            let output = input.with_extension("wav");
            std::fs::write(&input, fsb_bytes).unwrap();
            let status = Command::new("vgmstream-cli")
                .arg("-o")
                .arg(&output)
                .arg(&input)
                .status()
                .expect("vgmstream-cli must be installed to run this ignored oracle test");
            assert!(status.success());
            let expected = std::fs::read(&output).unwrap();
            assert_eq!(wave_data(&actual), wave_data(&expected));
            std::fs::remove_file(input).unwrap();
            std::fs::remove_file(output).unwrap();
        }
    }

    #[test]
    fn decodes_interleaved_and_planar_fsb5_vag() {
        for (channels, non_interleaved) in [(1_u16, false), (2, false), (6, false), (2, true)] {
            let fsb = Region::from_bytes(fsb5_vag(channels, non_interleaved));
            let stream = parse_fsb5_vag(&fsb).unwrap().unwrap();
            assert_eq!(stream.channels, channels);
            assert_eq!(stream.non_interleaved, non_interleaved);
            assert_eq!(stream.frame_count, 56);
            assert_eq!(stream.compressed_length, u64::from(channels) * 32);
            assert!(matches!(
                detect_direct_wav(&fsb, Some(16)).unwrap(),
                Some(DirectWavKind::Fsb5Vag(_))
            ));

            let expected_size = 44 + 56 * u64::from(channels) * 2;
            let mut wav = Vec::new();
            assert_eq!(
                write_direct_wav(
                    &fsb,
                    DirectWavKind::Fsb5Vag(stream),
                    expected_size,
                    &mut wav,
                )
                .unwrap(),
                expected_size
            );
            let pcm = wave_data(&wav);
            let expected_first = [1_i16, 3, 5, 7, -7, -5];
            let expected_second = [2_i16, 4, 6, -8, -6, -4];
            for channel in 0..usize::from(channels) {
                assert_eq!(
                    wave_sample(pcm, channels, 0, channel),
                    expected_first[channel]
                );
                assert_eq!(
                    wave_sample(pcm, channels, 1, channel),
                    expected_second[channel]
                );
                assert_eq!(
                    wave_sample(pcm, channels, 28, channel),
                    expected_second[channel]
                );
            }
        }
    }

    #[test]
    fn rejects_truncated_forged_and_over_budget_fsb5_vag_before_writing() {
        let fsb_bytes = fsb5_vag(2, false);
        let fsb = Region::from_bytes(fsb_bytes.clone());
        let stream = parse_fsb5_vag(&fsb).unwrap().unwrap();
        let output_size = 44 + 56 * 2 * 2;
        let mut output = Vec::new();
        assert!(
            write_direct_wav(
                &fsb,
                DirectWavKind::Fsb5Vag(stream),
                output_size - 1,
                &mut output,
            )
            .is_err()
        );
        assert!(output.is_empty());

        let forged = DirectWavKind::Fsb5Vag(Fsb5VagStream {
            compressed_length: stream.compressed_length - 1,
            ..stream
        });
        assert!(write_direct_wav(&fsb, forged, output_size, &mut output).is_err());
        assert!(output.is_empty());

        let mut truncated = fsb_bytes;
        truncated.pop();
        assert!(parse_fsb5_vag(&Region::from_bytes(truncated)).is_err());
    }

    #[test]
    #[ignore = "requires the optional vgmstream-cli decoder oracle"]
    fn fsb5_vag_pcm_matches_vgmstream_oracle() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for (channels, non_interleaved) in [(1_u16, false), (2, false), (6, false), (2, true)] {
            let fsb_bytes = fsb5_vag(channels, non_interleaved);
            let region = Region::from_bytes(fsb_bytes.clone());
            let kind = detect_direct_wav(&region, Some(16)).unwrap().unwrap();
            let mut actual = Vec::new();
            write_direct_wav(&region, kind, 1024 * 1024, &mut actual).unwrap();

            let layout = if non_interleaved {
                "planar"
            } else {
                "interleaved"
            };
            let input = std::env::temp_dir().join(format!(
                "assetstudio-fsb5-vag-{}-{unique}-{channels}-{layout}.fsb",
                std::process::id()
            ));
            let output = input.with_extension("wav");
            std::fs::write(&input, fsb_bytes).unwrap();
            let status = Command::new("vgmstream-cli")
                .arg("-o")
                .arg(&output)
                .arg(&input)
                .status()
                .expect("vgmstream-cli must be installed to run this ignored oracle test");
            assert!(status.success());
            let expected = std::fs::read(&output).unwrap();
            assert_eq!(wave_data(&actual), wave_data(&expected));
            std::fs::remove_file(input).unwrap();
            std::fs::remove_file(output).unwrap();
        }
    }

    #[test]
    fn decodes_multichannel_and_extended_predictor_fsb5_hevag() {
        for channels in [1_u16, 2, 6] {
            let fsb = Region::from_bytes(fsb5_hevag(channels));
            let stream = parse_fsb5_hevag(&fsb).unwrap().unwrap();
            assert_eq!(stream.channels, channels);
            assert_eq!(stream.frame_count, 56);
            assert_eq!(stream.compressed_length, u64::from(channels) * 32);
            assert!(matches!(
                detect_direct_wav(&fsb, Some(16)).unwrap(),
                Some(DirectWavKind::Fsb5Hevag(_))
            ));

            let expected_size = 44 + 56 * u64::from(channels) * 2;
            let mut wav = Vec::new();
            assert_eq!(
                write_direct_wav(
                    &fsb,
                    DirectWavKind::Fsb5Hevag(stream),
                    expected_size,
                    &mut wav,
                )
                .unwrap(),
                expected_size
            );
            let pcm = wave_data(&wav);
            let expected_first = [1_i16, 3, 5, 7, -7, -5];
            let expected_second = [2_i16, 4, 6, -8, -6, -4];
            for channel in 0..usize::from(channels) {
                assert_eq!(
                    wave_sample(pcm, channels, 0, channel),
                    expected_first[channel]
                );
                assert_eq!(
                    wave_sample(pcm, channels, 1, channel),
                    expected_second[channel]
                );
            }
        }
    }

    #[test]
    fn rejects_truncated_forged_and_over_budget_fsb5_hevag_before_writing() {
        let fsb_bytes = fsb5_hevag(2);
        let fsb = Region::from_bytes(fsb_bytes.clone());
        let stream = parse_fsb5_hevag(&fsb).unwrap().unwrap();
        let output_size = 44 + 56 * 2 * 2;
        let mut output = Vec::new();
        assert!(
            write_direct_wav(
                &fsb,
                DirectWavKind::Fsb5Hevag(stream),
                output_size - 1,
                &mut output,
            )
            .is_err()
        );
        assert!(output.is_empty());

        let forged = DirectWavKind::Fsb5Hevag(Fsb5HevagStream {
            compressed_length: stream.compressed_length - 1,
            ..stream
        });
        assert!(write_direct_wav(&fsb, forged, output_size, &mut output).is_err());
        assert!(output.is_empty());

        let mut truncated = fsb_bytes;
        truncated.pop();
        assert!(parse_fsb5_hevag(&Region::from_bytes(truncated)).is_err());
    }

    #[test]
    #[ignore = "requires the optional vgmstream-cli decoder oracle"]
    fn fsb5_hevag_pcm_matches_vgmstream_oracle() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for channels in [1_u16, 2, 6] {
            let fsb_bytes = fsb5_hevag(channels);
            let region = Region::from_bytes(fsb_bytes.clone());
            let kind = detect_direct_wav(&region, Some(16)).unwrap().unwrap();
            let mut actual = Vec::new();
            write_direct_wav(&region, kind, 1024 * 1024, &mut actual).unwrap();

            let input = std::env::temp_dir().join(format!(
                "assetstudio-fsb5-hevag-{}-{unique}-{channels}.fsb",
                std::process::id()
            ));
            let output = input.with_extension("wav");
            std::fs::write(&input, fsb_bytes).unwrap();
            let status = Command::new("vgmstream-cli")
                .arg("-o")
                .arg(&output)
                .arg(&input)
                .status()
                .expect("vgmstream-cli must be installed to run this ignored oracle test");
            assert!(status.success());
            let expected = std::fs::read(&output).unwrap();
            assert_eq!(wave_data(&actual), wave_data(&expected));
            std::fs::remove_file(input).unwrap();
            std::fs::remove_file(output).unwrap();
        }
    }

    #[test]
    fn decodes_multichannel_and_multiframe_fsb5_fadpcm() {
        for channels in [1_u16, 2, 6] {
            let fsb = Region::from_bytes(fsb5_fadpcm(channels));
            let stream = parse_fsb5_fadpcm(&fsb).unwrap().unwrap();
            assert_eq!(stream.channels, channels);
            assert_eq!(stream.frame_count, 512);
            assert_eq!(stream.compressed_length, u64::from(channels) * 0x118);
            assert!(matches!(
                detect_direct_wav(&fsb, Some(16)).unwrap(),
                Some(DirectWavKind::Fsb5Fadpcm(_))
            ));

            let expected_size = 44 + 512 * u64::from(channels) * 2;
            let mut wav = Vec::new();
            assert_eq!(
                write_direct_wav(
                    &fsb,
                    DirectWavKind::Fsb5Fadpcm(stream),
                    expected_size,
                    &mut wav,
                )
                .unwrap(),
                expected_size
            );
            let pcm = wave_data(&wav);
            let expected_first = [1_i16, 3, 5, 7, -7, -5];
            let expected_second = [2_i16, 4, 6, -8, -6, -4];
            for channel in 0..usize::from(channels) {
                assert_eq!(
                    wave_sample(pcm, channels, 0, channel),
                    expected_first[channel]
                );
                assert_eq!(
                    wave_sample(pcm, channels, 1, channel),
                    expected_second[channel]
                );
                assert_eq!(
                    wave_sample(pcm, channels, 256, channel),
                    expected_second[channel]
                );
            }
        }
    }

    #[test]
    fn rejects_truncated_forged_and_over_budget_fsb5_fadpcm_before_writing() {
        let fsb_bytes = fsb5_fadpcm(2);
        let fsb = Region::from_bytes(fsb_bytes.clone());
        let stream = parse_fsb5_fadpcm(&fsb).unwrap().unwrap();
        let output_size = 44 + 512 * 2 * 2;
        let mut output = Vec::new();
        assert!(
            write_direct_wav(
                &fsb,
                DirectWavKind::Fsb5Fadpcm(stream),
                output_size - 1,
                &mut output,
            )
            .is_err()
        );
        assert!(output.is_empty());

        let forged = DirectWavKind::Fsb5Fadpcm(Fsb5FadpcmStream {
            compressed_length: stream.compressed_length - 1,
            ..stream
        });
        assert!(write_direct_wav(&fsb, forged, output_size, &mut output).is_err());
        assert!(output.is_empty());

        let mut truncated = fsb_bytes;
        truncated.pop();
        assert!(parse_fsb5_fadpcm(&Region::from_bytes(truncated)).is_err());
    }

    #[test]
    #[ignore = "requires the optional vgmstream-cli decoder oracle"]
    fn fsb5_fadpcm_pcm_matches_vgmstream_oracle() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for channels in [1_u16, 2, 6] {
            let fsb_bytes = fsb5_fadpcm(channels);
            let region = Region::from_bytes(fsb_bytes.clone());
            let kind = detect_direct_wav(&region, Some(16)).unwrap().unwrap();
            let mut actual = Vec::new();
            write_direct_wav(&region, kind, 1024 * 1024, &mut actual).unwrap();

            let input = std::env::temp_dir().join(format!(
                "assetstudio-fsb5-fadpcm-{}-{unique}-{channels}.fsb",
                std::process::id()
            ));
            let output = input.with_extension("wav");
            std::fs::write(&input, fsb_bytes).unwrap();
            let status = Command::new("vgmstream-cli")
                .arg("-o")
                .arg(&output)
                .arg(&input)
                .status()
                .expect("vgmstream-cli must be installed to run this ignored oracle test");
            assert!(status.success());
            let expected = std::fs::read(&output).unwrap();
            assert_eq!(wave_data(&actual), wave_data(&expected));
            std::fs::remove_file(input).unwrap();
            std::fs::remove_file(output).unwrap();
        }
    }

    #[test]
    fn decodes_mono_and_stereo_fsb5_mpeg_layer3() {
        for channels in [1_u16, 2] {
            let fsb = Region::from_bytes(fsb5_mpeg_silence(channels, 2));
            let stream = parse_fsb5_mpeg(&fsb).unwrap().unwrap();
            assert_eq!(stream.channels, channels);
            assert_eq!(stream.sample_rate, 44_100);
            assert_eq!(stream.frame_count, 2304);
            assert_eq!(stream.compressed_length, 208);
            assert_eq!(stream.layer, Fsb5MpegLayer::Layer3);
            assert!(matches!(
                detect_direct_wav(&fsb, Some(16)).unwrap(),
                Some(DirectWavKind::Fsb5Mpeg(_))
            ));

            let expected_size = 44 + 2304 * u64::from(channels) * 2;
            let mut wav = Vec::new();
            assert_eq!(
                write_direct_wav(
                    &fsb,
                    DirectWavKind::Fsb5Mpeg(stream),
                    expected_size,
                    &mut wav,
                )
                .unwrap(),
                expected_size
            );
            assert!(wave_data(&wav).iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn decodes_fsb5_mpeg_layer2() {
        let fsb = Region::from_bytes(fsb5_mpeg_layer2_silence());
        let stream = parse_fsb5_mpeg(&fsb).unwrap().unwrap();
        assert_eq!(stream.layer, Fsb5MpegLayer::Layer2);
        assert_eq!(stream.frame_count, 1152);
        let mut wav = Vec::new();
        write_direct_wav(
            &fsb,
            DirectWavKind::Fsb5Mpeg(stream),
            44 + 1152 * 2,
            &mut wav,
        )
        .unwrap();
        assert!(wave_data(&wav).iter().all(|byte| *byte == 0));
    }

    #[test]
    fn rejects_malformed_forged_multistream_and_over_budget_fsb5_mpeg() {
        let fsb_bytes = fsb5_mpeg_silence(2, 2);
        let fsb = Region::from_bytes(fsb_bytes.clone());
        let stream = parse_fsb5_mpeg(&fsb).unwrap().unwrap();
        let output_size = 44 + 2304 * 2 * 2;
        let mut output = Vec::new();
        assert!(
            write_direct_wav(
                &fsb,
                DirectWavKind::Fsb5Mpeg(stream),
                output_size - 1,
                &mut output,
            )
            .is_err()
        );
        assert!(output.is_empty());

        let forged = DirectWavKind::Fsb5Mpeg(Fsb5MpegStream {
            compressed_length: stream.compressed_length - 4,
            ..stream
        });
        assert!(write_direct_wav(&fsb, forged, output_size, &mut output).is_err());
        assert!(output.is_empty());

        let multistream = Region::from_bytes(fsb5_mpeg_silence(6, 2));
        assert!(parse_fsb5_mpeg(&multistream).is_err());

        let mut bad_sync = fsb_bytes.clone();
        bad_sync[0x3c + 8] = 0;
        assert!(parse_fsb5_mpeg(&Region::from_bytes(bad_sync)).is_err());
        let mut truncated = fsb_bytes;
        truncated.pop();
        assert!(parse_fsb5_mpeg(&Region::from_bytes(truncated)).is_err());
    }

    #[test]
    #[ignore = "requires the optional vgmstream-cli decoder oracle"]
    fn fsb5_mpeg_pcm_matches_vgmstream_oracle() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // The silent pair verifies framing across both channel counts; the tone
        // is what makes this a decode comparison at all, since two readers
        // agree on zeroes whatever their decoders do with the bits.
        let cases = [
            ("silence-mono", fsb5_mpeg_silence(1, 2)),
            ("silence-stereo", fsb5_mpeg_silence(2, 2)),
            ("tone-mono", fsb5_mpeg_tone()),
        ];
        for (channels, fsb_bytes) in cases {
            let region = Region::from_bytes(fsb_bytes.clone());
            let kind = detect_direct_wav(&region, Some(16)).unwrap().unwrap();
            let mut actual = Vec::new();
            write_direct_wav(&region, kind, 1024 * 1024, &mut actual).unwrap();

            let input = std::env::temp_dir().join(format!(
                "assetstudio-fsb5-mpeg-{}-{unique}-{channels}.fsb",
                std::process::id()
            ));
            let output = input.with_extension("wav");
            std::fs::write(&input, fsb_bytes).unwrap();
            let status = Command::new("vgmstream-cli")
                .arg("-o")
                .arg(&output)
                .arg(&input)
                .status()
                .expect("vgmstream-cli must be installed to run this ignored oracle test");
            assert!(status.success());
            let expected = std::fs::read(&output).unwrap();
            let rust = wave_data(&actual);
            let oracle = wave_data(&expected);
            assert_eq!(rust.len(), oracle.len(), "{channels} sample count");
            // Layer III output is not specified bit-exactly: the standard fixes
            // the algorithm, not the rounding, so two independent decoders
            // differ in the last place. Measured against this fixture the worst
            // disagreement is one, on about seven percent of samples. Silence
            // still has to match exactly, where any difference at all would be
            // a real defect rather than rounding.
            let tolerance = i32::from(!channels.starts_with("silence"));
            for (index, (rust, oracle)) in
                rust.chunks_exact(2).zip(oracle.chunks_exact(2)).enumerate()
            {
                let rust = i32::from(i16::from_le_bytes(rust.try_into().unwrap()));
                let oracle = i32::from(i16::from_le_bytes(oracle.try_into().unwrap()));
                assert!(
                    (rust - oracle).abs() <= tolerance,
                    "{channels} sample {index}: {rust} vs {oracle}"
                );
            }
            std::fs::remove_file(input).unwrap();
            std::fs::remove_file(output).unwrap();
        }
    }

    #[test]
    fn decodes_mono_and_stereo_fsb5_opus() {
        for channels in [1_u16, 2] {
            let fsb = Region::from_bytes(fsb5_opus(channels, 2));
            let stream = parse_fsb5_opus(&fsb).unwrap().unwrap();
            assert_eq!(stream.channels, channels);
            assert_eq!(stream.sample_rate, 48_000);
            assert_eq!(stream.frame_count, 1608);
            assert_eq!(stream.encoder_delay, 312);
            assert_eq!(stream.compressed_length, stream.data_length);
            assert!(matches!(
                detect_direct_wav(&fsb, Some(16)).unwrap(),
                Some(DirectWavKind::Fsb5Opus(_))
            ));

            let expected_size = 44 + stream.frame_count * u64::from(channels) * 2;
            let mut wav = Vec::new();
            assert_eq!(
                write_direct_wav(
                    &fsb,
                    DirectWavKind::Fsb5Opus(stream),
                    expected_size,
                    &mut wav,
                )
                .unwrap(),
                expected_size
            );
            assert_eq!(
                u16::from_le_bytes(wav[22..24].try_into().unwrap()),
                channels
            );
            assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 48_000);
            assert!(wave_data(&wav).iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn rejects_malformed_multistream_and_over_budget_fsb5_opus() {
        let fsb = Region::from_bytes(fsb5_opus(2, 2));
        let stream = parse_fsb5_opus(&fsb).unwrap().unwrap();
        let output_size = 44 + stream.frame_count * 4;
        let mut output = Vec::new();
        assert!(
            write_direct_wav(
                &fsb,
                DirectWavKind::Fsb5Opus(stream),
                output_size - 1,
                &mut output,
            )
            .is_err()
        );
        assert!(output.is_empty());

        let forged = DirectWavKind::Fsb5Opus(Fsb5OpusStream {
            compressed_length: stream.compressed_length - 1,
            ..stream
        });
        assert!(write_direct_wav(&fsb, forged, output_size, &mut output).is_err());
        assert!(output.is_empty());

        assert!(parse_fsb5_opus(&Region::from_bytes(fsb5_opus(6, 2))).is_err());
        let wrong_rate = fsb5(1, 17, 0, &[(648, 1, 44_100, None)], &[2, 0, 3, 0]);
        assert!(parse_fsb5_opus(&Region::from_bytes(wrong_rate)).is_err());
        let malformed = fsb5(1, 17, 0, &[(648, 1, 48_000, None)], &[2, 0, 3, 0]);
        assert!(parse_fsb5_opus(&Region::from_bytes(malformed)).is_err());
        let truncated = fsb5(1, 17, 0, &[(648, 1, 48_000, None)], &[10, 0, 0]);
        assert!(parse_fsb5_opus(&Region::from_bytes(truncated)).is_err());
    }

    #[test]
    #[ignore = "requires the optional vgmstream-cli decoder oracle"]
    fn fsb5_opus_pcm_matches_vgmstream_oracle() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for channels in [1_u16, 2] {
            let fsb_bytes = fsb5_opus(channels, 3);
            let region = Region::from_bytes(fsb_bytes.clone());
            let kind = detect_direct_wav(&region, Some(16)).unwrap().unwrap();
            let mut actual = Vec::new();
            write_direct_wav(&region, kind, 1024 * 1024, &mut actual).unwrap();

            let input = std::env::temp_dir().join(format!(
                "assetstudio-fsb5-opus-{}-{unique}-{channels}.fsb",
                std::process::id()
            ));
            let output = input.with_extension("wav");
            std::fs::write(&input, fsb_bytes).unwrap();
            let status = Command::new("vgmstream-cli")
                .arg("-o")
                .arg(&output)
                .arg(&input)
                .status()
                .expect("vgmstream-cli must be installed to run this ignored oracle test");
            assert!(status.success());
            let expected = std::fs::read(&output).unwrap();
            assert_eq!(wave_data(&actual).len(), wave_data(&expected).len());
            for (rust, oracle) in wave_data(&actual)
                .chunks_exact(2)
                .zip(wave_data(&expected).chunks_exact(2))
            {
                let rust = i16::from_le_bytes(rust.try_into().unwrap());
                let oracle = i16::from_le_bytes(oracle.try_into().unwrap());
                assert!(i32::from(rust).abs_diff(i32::from(oracle)) <= 2);
            }
            std::fs::remove_file(input).unwrap();
            std::fs::remove_file(output).unwrap();
        }
    }

    #[test]
    fn decodes_bundled_fsb5_vorbis_to_bounded_pcm16() {
        let fsb = Region::from_bytes(FSB5_VORBIS_FIXTURE);
        let stream = parse_fsb5_vorbis(&fsb).unwrap().unwrap();
        assert_eq!(stream.data_offset, 80);
        assert_eq!(stream.data_length, 2726);
        assert_eq!(stream.compressed_length, 2726);
        assert_eq!(stream.frame_count, 4800);
        assert_eq!(stream.channels, 2);
        assert_eq!(stream.sample_rate, 48_000);
        assert_eq!(stream.setup_crc, 0x87c1_21d5);
        assert!(matches!(
            detect_direct_wav(&fsb, Some(16)).unwrap(),
            Some(DirectWavKind::Fsb5Vorbis(_))
        ));

        let output_size = 44 + 4800 * 2 * 2;
        let mut output = Vec::new();
        assert_eq!(
            write_direct_wav(
                &fsb,
                DirectWavKind::Fsb5Vorbis(stream),
                output_size,
                &mut output,
            )
            .unwrap(),
            output_size
        );
        assert_eq!(output.len(), usize::try_from(output_size).unwrap());
        assert_eq!(u16::from_le_bytes(output[22..24].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(output[24..28].try_into().unwrap()),
            48_000
        );
        assert!(wave_data(&output).iter().any(|byte| *byte != 0));
        assert_ne!(&wave_data(&output)[..64], &wave_data(&output)[64..128]);
    }

    #[test]
    fn rejects_unknown_malformed_forged_and_over_budget_fsb5_vorbis() {
        let fsb = Region::from_bytes(FSB5_VORBIS_FIXTURE);
        let stream = parse_fsb5_vorbis(&fsb).unwrap().unwrap();
        let output_size = 44 + stream.frame_count * u64::from(stream.channels) * 2;
        let mut output = Vec::new();
        assert!(
            write_direct_wav(
                &fsb,
                DirectWavKind::Fsb5Vorbis(stream),
                output_size - 1,
                &mut output,
            )
            .is_err()
        );
        assert!(output.is_empty());

        let forged = DirectWavKind::Fsb5Vorbis(Fsb5VorbisStream {
            compressed_length: stream.compressed_length - 1,
            ..stream
        });
        assert!(write_direct_wav(&fsb, forged, output_size, &mut output).is_err());
        assert!(output.is_empty());

        let mut unknown_setup = FSB5_VORBIS_FIXTURE.to_vec();
        unknown_setup[72..76].copy_from_slice(&0_u32.to_le_bytes());
        assert!(parse_fsb5_vorbis(&Region::from_bytes(unknown_setup)).is_err());

        let mut malformed_packet = FSB5_VORBIS_FIXTURE.to_vec();
        malformed_packet[80..82].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(parse_fsb5_vorbis(&Region::from_bytes(malformed_packet)).is_err());
    }

    #[test]
    #[ignore = "requires the optional vgmstream-cli decoder oracle"]
    fn fsb5_vorbis_pcm_matches_vgmstream_oracle() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let fsb = Region::from_bytes(FSB5_VORBIS_FIXTURE);
        let kind = detect_direct_wav(&fsb, Some(16)).unwrap().unwrap();
        let mut actual = Vec::new();
        write_direct_wav(&fsb, kind, 1024 * 1024, &mut actual).unwrap();

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let input = std::env::temp_dir().join(format!(
            "assetstudio-fsb5-vorbis-{}-{unique}.fsb",
            std::process::id()
        ));
        let output = input.with_extension("wav");
        std::fs::write(&input, FSB5_VORBIS_FIXTURE).unwrap();
        let status = Command::new("vgmstream-cli")
            .arg("-o")
            .arg(&output)
            .arg(&input)
            .status()
            .expect("vgmstream-cli must be installed to run this ignored oracle test");
        assert!(status.success());
        let expected = std::fs::read(&output).unwrap();
        assert_eq!(wave_data(&actual).len(), wave_data(&expected).len());
        let maximum_delta = wave_data(&actual)
            .chunks_exact(2)
            .zip(wave_data(&expected).chunks_exact(2))
            .map(|(rust, oracle)| {
                let rust = i16::from_le_bytes(rust.try_into().unwrap());
                let oracle = i16::from_le_bytes(oracle.try_into().unwrap());
                i32::from(rust).abs_diff(i32::from(oracle))
            })
            .max()
            .unwrap_or(0);
        assert!(maximum_delta <= 1);
        std::fs::remove_file(input).unwrap();
        std::fs::remove_file(output).unwrap();
    }

    fn fsb5(version: u32, codec: u32, flags: u32, samples: &[SampleSpec], data: &[u8]) -> Vec<u8> {
        let base_size = if version == 0 { 0x40 } else { 0x3c };
        let mut headers = Vec::new();
        for (index, (frames, channels, rate, metadata)) in samples.iter().copied().enumerate() {
            let data_offset = if index == 0 { 0 } else { 32 };
            let channel_code = match channels {
                2 => 1,
                6 => 2,
                8 => 3,
                _ => 0,
            };
            let rate_code = compact_rate_code(rate).unwrap_or(0);
            let has_metadata = u64::from(metadata.is_some() || !matches!(channels, 1 | 2 | 6 | 8));
            let mode = (frames << 34)
                | ((u64::try_from(data_offset).unwrap() / 32) << 7)
                | (channel_code << 5)
                | (u64::from(rate_code) << 1)
                | has_metadata;
            headers.extend_from_slice(&mode.to_le_bytes());
            if let Some((override_channels, override_rate)) = metadata {
                let channels_header = (1_u32 << 25) | (1 << 1) | 1;
                headers.extend_from_slice(&channels_header.to_le_bytes());
                headers.push(u8::try_from(override_channels).unwrap());
                let rate_header = (2_u32 << 25) | (4 << 1);
                headers.extend_from_slice(&rate_header.to_le_bytes());
                headers.extend_from_slice(&override_rate.to_le_bytes());
            }
        }

        let mut output = vec![0_u8; base_size];
        output[..4].copy_from_slice(b"FSB5");
        output[4..8].copy_from_slice(&version.to_le_bytes());
        output[8..12].copy_from_slice(&u32::try_from(samples.len()).unwrap().to_le_bytes());
        output[12..16].copy_from_slice(&u32::try_from(headers.len()).unwrap().to_le_bytes());
        output[16..20].copy_from_slice(&0_u32.to_le_bytes());
        output[20..24].copy_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        output[24..28].copy_from_slice(&codec.to_le_bytes());
        if version == 1 {
            output[32..36].copy_from_slice(&flags.to_le_bytes());
        }
        output.extend_from_slice(&headers);
        output.extend_from_slice(data);
        output
    }

    fn ima_block(channels: u16) -> Vec<u8> {
        let channels = usize::from(channels);
        // Every nibble value, rather than 0x10 repeated. At 0x10 the only
        // nibbles that ever appear are 0 and 1: the sign bit is never set, so
        // a decoder that mishandles negative deltas passes, and the step-index
        // table is only ever walked at its first entries.
        let mut block: Vec<u8> = (0..channels * 36)
            .map(|index| u8::try_from((index * 37 + 11) % 256).unwrap())
            .collect();
        // The first decoded nibble of each channel stays zero, so the tests
        // that check where the decoder starts reading -- the block header's
        // history and step index, and the per-channel byte layout -- can still
        // name the sample it must produce. Everything after it varies.
        for channel in 0..channels {
            let offset = usize::try_from(
                super::ima_nibble_byte_offset(0, channels, channel, 1).expect("fixture offset"),
            )
            .expect("fixture offset fits usize");
            block[offset] &= 0xf0;
        }
        if channels <= 2 {
            for channel in 0..channels {
                let offset = channel * 4;
                block[offset..offset + 2].copy_from_slice(&ima_history(channel).to_le_bytes());
                block[offset + 2] = ima_index(channel);
                block[offset + 3] = 0;
            }
        } else {
            for channel in 0..channels {
                let history_offset = channel * 2;
                block[history_offset..history_offset + 2]
                    .copy_from_slice(&ima_history(channel).to_le_bytes());
                let index_offset = channels * 2 + channel * 2;
                block[index_offset] = ima_index(channel);
                block[index_offset + 1] = 0;
            }
        }
        block
    }

    fn fsb5_dsp(channels: u16, non_interleaved: bool) -> Vec<u8> {
        let channels_usize = usize::from(channels);
        // All sixteen coefficients per channel, not just the first pair. With
        // the rest left at zero, every predictor index but 0 multiplies by
        // nothing, so choosing the wrong pair is invisible -- and the frame
        // headers below never chose anything else either.
        let mut coefficients = vec![0_u8; channels_usize * 0x2e];
        for channel in 0..channels_usize {
            let offset = channel * 0x2e;
            for index in 0..16 {
                let value = i16::try_from(
                    1024 + i32::try_from(index).unwrap() * 137
                        - i32::try_from(channel).unwrap() * 53,
                )
                .unwrap();
                let at = offset + index * 2;
                coefficients[at..at + 2].copy_from_slice(&value.to_be_bytes());
            }
        }
        let frames: Vec<[u8; 8]> = (0..channels_usize).map(dsp_frame).collect();
        let mut data = Vec::with_capacity(channels_usize * 8);
        if non_interleaved {
            for frame in &frames {
                data.extend_from_slice(frame);
            }
        } else {
            for offset in [0, 2, 4, 6] {
                for frame in &frames {
                    data.extend_from_slice(&frame[offset..offset + 2]);
                }
            }
        }
        let channel_code = match channels {
            1 => 0_u64,
            2 => 1,
            6 => 2,
            8 => 3,
            _ => panic!("DSP test channels must use compact FSB5 metadata"),
        };
        let mode = (14_u64 << 34) | (channel_code << 5) | (8 << 1) | 1;
        let chunk_header = (7_u32 << 25) | (u32::try_from(coefficients.len()).unwrap() << 1);
        let mut headers = Vec::new();
        headers.extend_from_slice(&mode.to_le_bytes());
        headers.extend_from_slice(&chunk_header.to_le_bytes());
        headers.extend_from_slice(&coefficients);

        let mut fsb = vec![0_u8; 0x3c];
        fsb[..4].copy_from_slice(b"FSB5");
        fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
        fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
        fsb[12..16].copy_from_slice(&u32::try_from(headers.len()).unwrap().to_le_bytes());
        fsb[20..24].copy_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        fsb[24..28].copy_from_slice(&6_u32.to_le_bytes());
        fsb[32..36].copy_from_slice(&u32::from(non_interleaved).wrapping_mul(2).to_le_bytes());
        fsb.extend_from_slice(&headers);
        fsb.extend_from_slice(&data);
        fsb
    }

    /// One DSP frame, whose header selects a predictor pair and a scale.
    ///
    /// The header used to be zero in every frame, which pins the predictor to
    /// pair 0 and the scale shift to 0: two of the three things a DSP decoder
    /// has to get right, held at the one value that hides a mistake. It now
    /// varies per channel, and the sample nibbles already did.
    fn dsp_frame(channel: usize) -> [u8; 8] {
        const NIBBLES: [u8; 6] = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc];
        const HEADERS: [u8; 6] = [0x13, 0x25, 0x071, 0x42, 0x56, 0x34];
        let mut frame = [NIBBLES[channel]; 8];
        frame[0] = HEADERS[channel];
        frame
    }

    fn fsb5_vag(channels: u16, non_interleaved: bool) -> Vec<u8> {
        let channels_usize = usize::from(channels);
        let frames: Vec<[[u8; 16]; 2]> = (0..channels_usize).map(vag_frames).collect();
        let mut data = Vec::with_capacity(channels_usize * 32);
        if non_interleaved {
            for channel_frames in &frames {
                for frame in channel_frames {
                    data.extend_from_slice(frame);
                }
            }
        } else {
            for frame_index in 0..2 {
                for channel_frames in &frames {
                    data.extend_from_slice(&channel_frames[frame_index]);
                }
            }
        }
        let channel_code = match channels {
            1 => 0_u64,
            2 => 1,
            6 => 2,
            8 => 3,
            _ => panic!("VAG test channels must use compact FSB5 metadata"),
        };
        let mode = (56_u64 << 34) | (channel_code << 5) | (8 << 1);
        let mut fsb = vec![0_u8; 0x3c];
        fsb[..4].copy_from_slice(b"FSB5");
        fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
        fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
        fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
        fsb[20..24].copy_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        fsb[24..28].copy_from_slice(&8_u32.to_le_bytes());
        let flags = if non_interleaved { 2_u32 } else { 0 };
        fsb[32..36].copy_from_slice(&flags.to_le_bytes());
        fsb.extend_from_slice(&mode.to_le_bytes());
        fsb.extend_from_slice(&data);
        fsb
    }

    fn vag_frames(channel: usize) -> [[u8; 16]; 2] {
        const FIRST: [u8; 6] = [0x21, 0x43, 0x65, 0x87, 0xa9, 0xcb];
        const SECOND: [u8; 6] = [0x32, 0x54, 0x76, 0x98, 0xba, 0xdc];
        [vag_frame(FIRST[channel]), vag_frame(SECOND[channel])]
    }

    fn vag_frame(nibbles: u8) -> [u8; 16] {
        let mut frame = [nibbles; 16];
        frame[0] = 0x0c;
        frame[1] = 0;
        frame
    }

    fn fsb5_hevag(channels: u16) -> Vec<u8> {
        let channels_usize = usize::from(channels);
        let first = [0x21_u8, 0x43, 0x65, 0x87, 0xa9, 0xcb];
        let second = [0x32_u8, 0x54, 0x76, 0x98, 0xba, 0xdc];
        let mut data = Vec::with_capacity(channels_usize * 32);
        for &channel_nibbles in &first[..channels_usize] {
            data.extend_from_slice(&hevag_frame(channel_nibbles, 0));
        }
        for &channel_nibbles in &second[..channels_usize] {
            data.extend_from_slice(&hevag_frame(channel_nibbles, 127));
        }
        let channel_code = match channels {
            1 => 0_u64,
            2 => 1,
            6 => 2,
            8 => 3,
            _ => panic!("HEVAG test channels must use compact FSB5 metadata"),
        };
        let mode = (56_u64 << 34) | (channel_code << 5) | (8 << 1);
        let mut fsb = vec![0_u8; 0x3c];
        fsb[..4].copy_from_slice(b"FSB5");
        fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
        fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
        fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
        fsb[20..24].copy_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        fsb[24..28].copy_from_slice(&9_u32.to_le_bytes());
        fsb.extend_from_slice(&mode.to_le_bytes());
        fsb.extend_from_slice(&data);
        fsb
    }

    fn hevag_frame(nibbles: u8, predictor: u8) -> [u8; 16] {
        let mut frame = [nibbles; 16];
        frame[0] = ((predictor & 0x0f) << 4) | 0x0c;
        frame[1] = predictor & 0xf0;
        frame
    }

    fn fsb5_fadpcm(channels: u16) -> Vec<u8> {
        let channels_usize = usize::from(channels);
        let first = [0x21_u8, 0x43, 0x65, 0x87, 0xa9, 0xcb];
        let second = [0x32_u8, 0x54, 0x76, 0x98, 0xba, 0xdc];
        let mut data = Vec::with_capacity(channels_usize * 0x118);
        for nibbles in [first, second] {
            for &channel_nibbles in &nibbles[..channels_usize] {
                data.extend_from_slice(&fadpcm_frame(channel_nibbles));
            }
        }
        let channel_code = match channels {
            1 => 0_u64,
            2 => 1,
            6 => 2,
            8 => 3,
            _ => panic!("FADPCM test channels must use compact FSB5 metadata"),
        };
        let mode = (512_u64 << 34) | (channel_code << 5) | (8 << 1);
        let mut fsb = vec![0_u8; 0x3c];
        fsb[..4].copy_from_slice(b"FSB5");
        fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
        fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
        fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
        fsb[20..24].copy_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        fsb[24..28].copy_from_slice(&16_u32.to_le_bytes());
        fsb.extend_from_slice(&mode.to_le_bytes());
        fsb.extend_from_slice(&data);
        fsb
    }

    /// The MPEG differential's non-silent payload: real Layer III frames.
    ///
    /// The silent fixture below verifies framing and nothing else -- both
    /// readers agree on zeroes whatever their decoders do with the bits. This
    /// one carries an actual tone, so a sample-level difference has somewhere
    /// to show up.
    const MPEG_TONE: &[u8] = include_bytes!("../tests/fixtures/audio/mpeg-layer3-tone.mp3");
    /// Thirteen MPEG-1 Layer III frames at 1152 samples each.
    const MPEG_TONE_FRAMES: u64 = 13;

    fn fsb5_mpeg_tone() -> Vec<u8> {
        // FSB5 pads every MPEG frame to a four-byte boundary, so a plain
        // concatenation of the encoder's output is not a valid payload; the
        // scan loses sync on the first frame whose length is not a multiple
        // of four.
        let mut data = Vec::with_capacity(MPEG_TONE.len() + 64);
        let mut offset = 0_usize;
        let mut frames = 0_u64;
        while offset + 4 <= MPEG_TONE.len() {
            let header = u32::from_be_bytes(
                MPEG_TONE[offset..offset + 4]
                    .try_into()
                    .expect("four header bytes"),
            );
            let parsed = parse_mpeg_frame_header(header).expect("a Layer III frame header");
            let length = usize::try_from(parsed.byte_length).expect("a frame length");
            data.extend_from_slice(&MPEG_TONE[offset..offset + length]);
            data.resize(data.len().next_multiple_of(4), 0);
            offset += length;
            frames += 1;
        }
        assert_eq!(frames, MPEG_TONE_FRAMES, "the fixture frame count changed");
        fsb5(
            1,
            11,
            0,
            &[(MPEG_TONE_FRAMES * 1152, 1, 44_100, None)],
            &data,
        )
    }

    fn fsb5_mpeg_silence(channels: u16, frame_count: usize) -> Vec<u8> {
        let channel_mode = if channels == 1 { 0xc0 } else { 0x00 };
        let mut data = Vec::with_capacity(frame_count * 104);
        for _ in 0..frame_count {
            data.extend_from_slice(&[0xff, 0xfb, 0x10, channel_mode]);
            data.resize(data.len() + 100, 0);
        }
        fsb5(
            1,
            11,
            0,
            &[(
                u64::try_from(frame_count).unwrap() * 1152,
                channels,
                44_100,
                None,
            )],
            &data,
        )
    }

    fn fsb5_mpeg_layer2_silence() -> Vec<u8> {
        let mut data = vec![0_u8; 104];
        data[..4].copy_from_slice(&[0xff, 0xfd, 0x10, 0xc0]);
        fsb5(1, 11, 0, &[(1152, 1, 44_100, None)], &data)
    }

    /// The Opus differential's non-silent payloads.
    ///
    /// Already in FSB5's own framing -- each packet preceded by its
    /// little-endian length -- because that is exactly what the container
    /// stores. Both are regenerated by `tools/generate_audio_fixtures.py`.
    ///
    /// Opus carries two internal codecs and this crate decodes them to
    /// different standards, so there is one fixture of each: the CELT stream
    /// matches libopus, the SILK/hybrid stream does not. Testing only one would
    /// either miss the defect or write it off as codec tolerance.
    const OPUS_TONE: &[u8] = include_bytes!("../tests/fixtures/audio/opus-tone-packets.bin");
    const OPUS_TONE_CELT: &[u8] =
        include_bytes!("../tests/fixtures/audio/opus-tone-celt-packets.bin");
    /// Seven packets at 960 samples each, less the encoder delay FSB5 hides.
    const OPUS_TONE_FRAMES: u64 = 7 * 960 - 312;

    /// How far either way the alignment search looks. The observed shifts are
    /// well inside this; the margin exists so a regression that moves further
    /// still reports a number rather than falling off the search.
    const SEARCH: isize = 8;
    const SEARCH_MARGIN: usize = 8;

    /// Decodes an FSB5 Opus fixture here and in `vgmstream-cli`, and returns
    /// the smallest worst-case sample difference over a small alignment search
    /// together with the shift that achieved it.
    fn opus_divergence_from_vgmstream(packets: &[u8], label: &str) -> (i32, isize) {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let fsb_bytes = fsb5_opus_tone_from(packets);
        let region = Region::from_bytes(fsb_bytes.clone());
        let kind = detect_direct_wav(&region, Some(16)).unwrap().unwrap();
        let mut actual = Vec::new();
        write_direct_wav(&region, kind, 1024 * 1024, &mut actual).unwrap();

        let input = std::env::temp_dir().join(format!(
            "assetstudio-fsb5-opus-{label}-{}-{unique}.fsb",
            std::process::id()
        ));
        let output = input.with_extension("wav");
        std::fs::write(&input, fsb_bytes).unwrap();
        let status = Command::new("vgmstream-cli")
            .arg("-o")
            .arg(&output)
            .arg(&input)
            .status()
            .expect("vgmstream-cli must be installed to run this ignored oracle test");
        assert!(status.success());
        let expected = std::fs::read(&output).unwrap();
        std::fs::remove_file(input).unwrap();
        std::fs::remove_file(output).unwrap();

        let decode = |wave: &[u8]| -> Vec<i32> {
            wave_data(wave)
                .chunks_exact(2)
                .map(|pair| i32::from(i16::from_le_bytes(pair.try_into().unwrap())))
                .collect()
        };
        let rust = decode(&actual);
        let oracle = decode(&expected);
        assert_eq!(rust.len(), oracle.len(), "{label} sample count");
        // The oracle is not silent, or none of the above would mean anything:
        // an all-zero fixture is exactly how the earlier version of this
        // comparison passed while the decoder was wrong.
        assert!(
            oracle.iter().any(|value| value.abs() > 1000),
            "{label} oracle decoded to near-silence"
        );

        let mut best = (i32::MAX, 0);
        for shift in -SEARCH..=SEARCH {
            let window = &oracle[SEARCH_MARGIN..oracle.len() - SEARCH_MARGIN];
            let mut worst = 0_i32;
            for (offset, expected) in window.iter().enumerate() {
                let index = isize::try_from(SEARCH_MARGIN + offset).unwrap() + shift;
                let shifted = usize::try_from(index).unwrap();
                worst = worst.max((rust[shifted] - expected).abs());
            }
            best = best.min((worst, shift));
        }
        best
    }

    /// CELT-only Opus decodes to libopus's own output.
    ///
    /// Opus conformance is a similarity metric rather than bit equality, so a
    /// unit of slack is warranted; it is also what was measured, and the
    /// alignment is required to be exact.
    #[test]
    #[ignore = "requires the optional vgmstream-cli decoder oracle"]
    fn fsb5_opus_celt_tone_matches_vgmstream() {
        let (worst, shift) = opus_divergence_from_vgmstream(OPUS_TONE_CELT, "celt");
        assert_eq!(shift, 0, "CELT output is misaligned by {shift} samples");
        assert!(
            worst <= 1,
            "CELT divergence is {worst}, past the measured 1"
        );
    }

    /// Records how far SILK/hybrid Opus sits from libopus. This is a known
    /// defect, not a tolerance.
    ///
    /// Written up with measurements in `docs/upstream-defects.md`.
    ///
    /// The divergence was isolated to `ruopus` 0.1.2 and reproduces with no
    /// code from this crate involved: feeding the same packets straight to the
    /// decoder and applying the stream's own pre-skip, its output arrives early
    /// and differs in amplitude, while `ffmpeg` and `vgmstream` -- both
    /// libopus -- agree with each other to within one unit.
    ///
    /// The error tracks SILK's internal sample rate, which is what a resampler
    /// delay that is compensated slightly differently would do:
    ///
    /// | packet mode        | shift | worst |
    /// |--------------------|-------|-------|
    /// | CELT-only          |     0 |     1 |
    /// | SILK/hybrid        |    -2 |   103 |
    /// | SILK wideband      |    -2 |   135 |
    /// | SILK narrowband    |    -4 |   115 |
    ///
    /// against a peak near 4200 in every case. This fixture, a 24 kbps hybrid
    /// stream, measures 276 -- worse than any of the probes above, so the bound
    /// here is its own measurement rather than a number carried over.
    ///
    /// The bound below pins the measured behaviour so a regression past it
    /// fails. Fixing it means an upstream change or a different decoder;
    /// `fsb5_opus_celt_tone_matches_vgmstream` guards the half that is right.
    #[test]
    #[ignore = "requires the optional vgmstream-cli decoder oracle"]
    fn fsb5_opus_silk_tone_divergence_from_libopus_is_bounded() {
        const WORST_DELTA: i32 = 276;
        const ALIGNMENT: isize = -2;

        let (worst, shift) = opus_divergence_from_vgmstream(OPUS_TONE, "silk");
        assert_eq!(shift, ALIGNMENT, "SILK alignment moved to {shift}");
        assert!(
            worst <= WORST_DELTA,
            "SILK divergence grew to {worst}, past the recorded {WORST_DELTA}"
        );
    }

    fn fsb5_opus_tone_from(packets: &[u8]) -> Vec<u8> {
        let mut data = packets.to_vec();
        // The zero length that ends the packet run.
        data.extend_from_slice(&0_u16.to_le_bytes());
        fsb5(1, 17, 0, &[(OPUS_TONE_FRAMES, 1, 48_000, None)], &data)
    }

    fn fsb5_opus(channels: u16, packet_count: usize) -> Vec<u8> {
        const MONO: [[u8; 64]; 2] = [
            [
                0xf8, 0x6f, 0xed, 0x8a, 0x58, 0xc6, 0x40, 0x44, 0x64, 0xd8, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0xad, 0x43, 0xa8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
            [
                0xf8, 0x61, 0xfe, 0x77, 0x80, 0x8d, 0xd2, 0x60, 0xa9, 0xfe, 0x94, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
        ];
        const STEREO: [[u8; 64]; 2] = [
            [
                0xfc, 0x6f, 0xee, 0x2a, 0x9d, 0x99, 0xdf, 0x52, 0x14, 0x00, 0x00, 0x00, 0xbc, 0x7f,
                0xdc, 0xae, 0x9f, 0x04, 0xc6, 0x16, 0x4d, 0xe0, 0xc3, 0xe1, 0x7b, 0xc4, 0x28, 0x43,
                0xcf, 0x55, 0xb3, 0x9d, 0x68, 0xa3, 0x38, 0x00, 0x5a, 0x73, 0xe0, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
            [
                0xfc, 0x61, 0xfe, 0x78, 0x00, 0x77, 0x03, 0x22, 0x2b, 0xe1, 0x17, 0xcb, 0x00, 0x06,
                0xe8, 0x94, 0x43, 0x08, 0xe5, 0x1b, 0xc6, 0x22, 0x14, 0xcb, 0x0c, 0xaa, 0xbe, 0x71,
                0x35, 0xc9, 0xf9, 0xbb, 0xee, 0x9a, 0x00, 0x9f, 0x38, 0x02, 0x75, 0xc9, 0x70, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
        ];
        let packets = if channels == 1 { MONO } else { STEREO };
        let mut data = Vec::new();
        for index in 0..packet_count {
            let packet = &packets[index % packets.len()];
            data.extend_from_slice(&u16::try_from(packet.len()).unwrap().to_le_bytes());
            data.extend_from_slice(packet);
        }
        data.extend_from_slice(&0_u16.to_le_bytes());
        let decoded_frames = u64::try_from(packet_count).unwrap() * 960;
        let exposed_frames = decoded_frames - 312;
        fsb5(1, 17, 0, &[(exposed_frames, channels, 48_000, None)], &data)
    }

    fn fadpcm_frame(nibbles: u8) -> [u8; 0x8c] {
        let mut frame = [nibbles; 0x8c];
        frame[..0x0c].fill(0);
        frame
    }

    fn wave_sample(pcm: &[u8], channels: u16, frame: usize, channel: usize) -> i16 {
        let offset = (frame * usize::from(channels) + channel) * 2;
        i16::from_le_bytes(pcm[offset..offset + 2].try_into().unwrap())
    }

    fn ima_history(channel: usize) -> i16 {
        1000 + i16::try_from(channel).unwrap() * 100
    }

    fn ima_index(channel: usize) -> u8 {
        10 + u8::try_from(channel).unwrap()
    }

    fn ima_step(channel: usize) -> i16 {
        const STEPS: [i16; 6] = [19, 21, 23, 25, 28, 31];
        STEPS[channel]
    }

    fn wave_data(wav: &[u8]) -> &[u8] {
        assert!(wav.len() >= 12);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let mut cursor = 12;
        while cursor + 8 <= wav.len() {
            let size = usize::try_from(u32::from_le_bytes(
                wav[cursor + 4..cursor + 8].try_into().unwrap(),
            ))
            .unwrap();
            let data_start = cursor + 8;
            let data_end = data_start.checked_add(size).unwrap();
            assert!(data_end <= wav.len());
            if &wav[cursor..cursor + 4] == b"data" {
                return &wav[data_start..data_end];
            }
            cursor = data_end + (size & 1);
        }
        panic!("WAV contains no data chunk");
    }

    const fn compact_rate_code(rate: u32) -> Option<u8> {
        match rate {
            4_000 => Some(0),
            8_000 => Some(1),
            11_000 => Some(2),
            11_025 => Some(3),
            16_000 => Some(4),
            22_050 => Some(5),
            24_000 => Some(6),
            32_000 => Some(7),
            44_100 => Some(8),
            48_000 => Some(9),
            96_000 => Some(10),
            _ => None,
        }
    }

    #[test]
    fn direct_kind_layout_is_stable_for_callers() {
        let kind = DirectWavKind::Fsb5Pcm(Fsb5PcmStream {
            data_offset: 60,
            data_length: 4,
            channels: 1,
            sample_rate: 8_000,
            sample_format: PcmSampleFormat::Signed16,
            big_endian: false,
            convert_float_to_pcm16: false,
        });
        assert!(matches!(kind, DirectWavKind::Fsb5Pcm(_)));

        let kind = DirectWavKind::Fsb5Ima(Fsb5ImaStream {
            data_offset: 60,
            compressed_length: 36,
            frame_count: 64,
            channels: 1,
            sample_rate: 8_000,
        });
        assert!(matches!(kind, DirectWavKind::Fsb5Ima(_)));

        let kind = DirectWavKind::Fsb5Dsp(Fsb5DspStream {
            data_offset: 128,
            data_length: 8,
            compressed_length: 8,
            coefficients_offset: 72,
            coefficients_length: 0x2e,
            frame_count: 14,
            channels: 1,
            sample_rate: 8_000,
            non_interleaved: false,
        });
        assert!(matches!(kind, DirectWavKind::Fsb5Dsp(_)));

        let kind = DirectWavKind::Fsb5Vag(Fsb5VagStream {
            data_offset: 68,
            data_length: 16,
            compressed_length: 16,
            frame_count: 28,
            channels: 1,
            sample_rate: 8_000,
            non_interleaved: false,
        });
        assert!(matches!(kind, DirectWavKind::Fsb5Vag(_)));

        let kind = DirectWavKind::Fsb5Hevag(Fsb5HevagStream {
            data_offset: 68,
            data_length: 16,
            compressed_length: 16,
            frame_count: 28,
            channels: 1,
            sample_rate: 8_000,
        });
        assert!(matches!(kind, DirectWavKind::Fsb5Hevag(_)));

        let kind = DirectWavKind::Fsb5Fadpcm(Fsb5FadpcmStream {
            data_offset: 68,
            data_length: 0x8c,
            compressed_length: 0x8c,
            frame_count: 256,
            channels: 1,
            sample_rate: 8_000,
        });
        assert!(matches!(kind, DirectWavKind::Fsb5Fadpcm(_)));

        let kind = DirectWavKind::Fsb5Mpeg(Fsb5MpegStream {
            data_offset: 68,
            data_length: 104,
            compressed_length: 104,
            frame_count: 1152,
            channels: 1,
            sample_rate: 44_100,
            layer: Fsb5MpegLayer::Layer3,
        });
        assert!(matches!(kind, DirectWavKind::Fsb5Mpeg(_)));
    }
}
