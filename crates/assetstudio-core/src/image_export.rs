//! Bounded, dependency-light image encoders for decoded Unity RGBA8 pixels.
//!
//! `Texture2D` decoders retain Unity's native row order, while Sprite cropping
//! already produces display-order rows. Callers make that distinction explicit
//! through [`ImageRowOrder`] so file formats are always written top-down.

use std::borrow::Cow;
use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use image_webp::{ColorType as WebPColorType, EncodingError as WebPEncodingError, WebPEncoder};
use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder, EncodingError};

use crate::texture::{RgbaImage, write_rgba_ir, write_rgba_ir_display_order};
use crate::{Error, Result};

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const PNG_IDAT_CHUNK_BYTES: usize = 64 * 1024;
const PNG_MAX_DIMENSION: u32 = 0x7fff_ffff;
const WEBP_MAX_DIMENSION: u32 = 16_384;
const BGRA_WRITE_CHUNK_BYTES: usize = 4 * 1024;
const BMP_V4_HEADER_LENGTH: usize = 14 + 108;
const BMP_V4_HEADER_BYTES: u64 = 14 + 108;
const BMP_V4_PIXEL_OFFSET: u32 = 14 + 108;
const TGA_HEADER_LENGTH: usize = 18;
const TGA_HEADER_BYTES: u64 = 18;
const RGBA_IR_HEADER_BYTES: u64 = 36;

/// ImageSharp-compatible default quality used by JPEG export.
pub const DEFAULT_JPEG_QUALITY: u8 = 75;

/// File format used for a decoded `Texture2D` or Sprite image.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// Lossy baseline JPEG. RGB is retained and alpha is discarded.
    Jpeg,
    /// Lossless RGBA8 PNG using filter type zero and zlib compression.
    #[default]
    Png,
    /// 32-bit top-down BMP with explicit RGBA bit masks.
    Bmp,
    /// Uncompressed 32-bit top-left-origin TGA.
    Tga,
    /// Pure-Rust lossless VP8L WebP with RGBA preservation.
    Webp,
    /// The existing `HARUKI_RGBAIR_V1` interchange contract.
    RawRgba,
}

impl ImageFormat {
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => ".jpg",
            Self::Png => ".png",
            Self::Bmp => ".bmp",
            Self::Tga => ".tga",
            Self::Webp => ".webp",
            Self::RawRgba => ".rgba",
        }
    }

    #[must_use]
    pub const fn payload_kind(self) -> &'static str {
        match self {
            Self::Jpeg => "image_jpeg",
            Self::Png => "image_png",
            Self::Bmp => "image_bmp",
            Self::Tga => "image_tga",
            Self::Webp => "image_webp",
            Self::RawRgba => "image_raw_rgba",
        }
    }
}

/// Describes the row order of an in-memory [`RgbaImage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageRowOrder {
    /// Rows came directly from a `Texture2D` decoder and must be flipped.
    UnityDecoded,
    /// Rows already run from the displayed top edge to the bottom edge.
    Display,
}

/// Writes one RGBA8 image while enforcing the complete encoded-output budget.
///
/// PNG scanlines are compressed directly into bounded IDAT chunks, so encoding
/// does not materialize a second full-image buffer. For JPEG and lossless WebP,
/// the same limit also bounds the contiguous top-down RGBA encoder input.
pub fn write_rgba_image<W: Write>(
    image: &RgbaImage,
    format: ImageFormat,
    row_order: ImageRowOrder,
    maximum_output_bytes: u64,
    output: &mut W,
) -> Result<u64> {
    write_rgba_image_with_quality(
        image,
        format,
        row_order,
        DEFAULT_JPEG_QUALITY,
        maximum_output_bytes,
        output,
    )
}

/// Writes one RGBA8 image with an explicit JPEG quality from 1 through 100.
///
/// The quality is ignored for non-JPEG formats. JPEG follows the managed
/// exporter's semantics by retaining RGB values and discarding alpha rather
/// than compositing transparent pixels against a background.
pub fn write_rgba_image_with_quality<W: Write>(
    image: &RgbaImage,
    format: ImageFormat,
    row_order: ImageRowOrder,
    jpeg_quality: u8,
    maximum_output_bytes: u64,
    output: &mut W,
) -> Result<u64> {
    let dimensions = validate_image(image)?;
    let mut output = BoundedWriter::new(output, maximum_output_bytes);
    match format {
        ImageFormat::Jpeg => write_jpeg(
            image,
            dimensions,
            row_order,
            jpeg_quality,
            maximum_output_bytes,
            &mut output,
        )?,
        ImageFormat::Png => write_png(image, dimensions, row_order, &mut output)?,
        ImageFormat::Bmp => write_bmp(image, dimensions, row_order, &mut output)?,
        ImageFormat::Tga => write_tga(image, dimensions, row_order, &mut output)?,
        ImageFormat::Webp => write_webp(
            image,
            dimensions,
            row_order,
            maximum_output_bytes,
            &mut output,
        )?,
        ImageFormat::RawRgba => {
            let expected = RGBA_IR_HEADER_BYTES
                .checked_add(dimensions.pixel_bytes)
                .ok_or_else(|| Error::invalid_data("raw-RGBA output length overflowed"))?;
            ensure_output_budget(expected, maximum_output_bytes, "raw-RGBA")?;
            let written = match row_order {
                ImageRowOrder::UnityDecoded => write_rgba_ir(image, &mut output)?,
                ImageRowOrder::Display => write_rgba_ir_display_order(image, &mut output)?,
            };
            if written != expected {
                return Err(Error::invalid_data(format!(
                    "raw-RGBA writer produced {written} bytes, expected {expected}"
                )));
            }
        }
    }
    Ok(output.written())
}

fn write_jpeg<W: Write>(
    image: &RgbaImage,
    dimensions: ImageDimensions,
    row_order: ImageRowOrder,
    quality: u8,
    maximum_working_bytes: u64,
    output: &mut BoundedWriter<'_, W>,
) -> Result<()> {
    if !(1..=100).contains(&quality) {
        return Err(Error::invalid_data(format!(
            "JPEG quality {quality} is outside the supported range 1 through 100"
        )));
    }
    let width = u16::try_from(image.width)
        .map_err(|_| Error::invalid_data("JPEG width exceeds 65535 pixels"))?;
    let height = u16::try_from(image.height)
        .map_err(|_| Error::invalid_data("JPEG height exceeds 65535 pixels"))?;
    if dimensions.pixel_bytes > maximum_working_bytes {
        return Err(Error::invalid_data(format!(
            "JPEG encoder requires {} input bytes, exceeding working limit {}",
            dimensions.pixel_bytes, maximum_working_bytes
        )));
    }
    let pixels = display_order_rgba(image, dimensions, row_order)?;
    let result = catch_unwind(AssertUnwindSafe(|| {
        JpegEncoder::new(output, quality).encode(
            pixels.as_ref(),
            width,
            height,
            JpegColorType::Rgba,
        )
    }));
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(EncodingError::IoError(error))) => Err(error.into()),
        Ok(Err(error)) => Err(Error::invalid_data(format!("cannot encode JPEG: {error}"))),
        Err(_) => Err(Error::invalid_data("JPEG encoder panicked")),
    }
}

#[derive(Debug, Clone, Copy)]
struct ImageDimensions {
    pixel_bytes: u64,
    stride_usize: usize,
    height_usize: usize,
}

fn validate_image(image: &RgbaImage) -> Result<ImageDimensions> {
    if image.width == 0 || image.height == 0 {
        return Err(Error::invalid_data("image dimensions must be positive"));
    }
    let stride = u64::from(image.width)
        .checked_mul(4)
        .ok_or_else(|| Error::invalid_data("image row stride overflowed"))?;
    let pixel_bytes = stride
        .checked_mul(u64::from(image.height))
        .ok_or_else(|| Error::invalid_data("image pixel byte length overflowed"))?;
    let actual = u64::try_from(image.pixels.len())
        .map_err(|_| Error::invalid_data("image pixel buffer length does not fit in u64"))?;
    if actual != pixel_bytes {
        return Err(Error::invalid_data(format!(
            "image pixel buffer is {actual} bytes, expected {pixel_bytes}"
        )));
    }
    Ok(ImageDimensions {
        pixel_bytes,
        stride_usize: usize::try_from(stride)
            .map_err(|_| Error::invalid_data("image row stride is too large for this platform"))?,
        height_usize: usize::try_from(image.height)
            .map_err(|_| Error::invalid_data("image height is too large for this platform"))?,
    })
}

fn write_png<W: Write>(
    image: &RgbaImage,
    dimensions: ImageDimensions,
    row_order: ImageRowOrder,
    output: &mut BoundedWriter<'_, W>,
) -> Result<()> {
    if image.width > PNG_MAX_DIMENSION || image.height > PNG_MAX_DIMENSION {
        return Err(Error::invalid_data(format!(
            "PNG dimensions {}x{} exceed the format maximum {PNG_MAX_DIMENSION}",
            image.width, image.height
        )));
    }
    output.write_all(PNG_SIGNATURE)?;

    let mut ihdr = [0_u8; 13];
    ihdr[..4].copy_from_slice(&image.width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&image.height.to_be_bytes());
    ihdr[8] = 8;
    ihdr[9] = 6;
    write_png_chunk(output, *b"IHDR", &ihdr)?;

    let idat = IdatWriter::new(output)?;
    let mut encoder = ZlibEncoder::new(idat, Compression::default());
    for output_row in 0..dimensions.height_usize {
        encoder.write_all(&[0])?;
        encoder.write_all(display_row(image, dimensions, row_order, output_row)?)?;
    }
    let idat = encoder.finish()?;
    idat.finish()?;

    write_png_chunk(output, *b"IEND", &[])?;
    Ok(())
}

fn write_bmp<W: Write>(
    image: &RgbaImage,
    dimensions: ImageDimensions,
    row_order: ImageRowOrder,
    output: &mut BoundedWriter<'_, W>,
) -> Result<()> {
    let width = i32::try_from(image.width)
        .map_err(|_| Error::invalid_data("BMP width exceeds signed 32-bit range"))?;
    let height = i32::try_from(image.height)
        .map_err(|_| Error::invalid_data("BMP height exceeds signed 32-bit range"))?;
    let file_size = BMP_V4_HEADER_BYTES
        .checked_add(dimensions.pixel_bytes)
        .ok_or_else(|| Error::invalid_data("BMP file size overflowed"))?;
    ensure_output_budget(file_size, output.maximum(), "BMP")?;
    let file_size_u32 = u32::try_from(file_size)
        .map_err(|_| Error::invalid_data("BMP file size exceeds its 32-bit header"))?;
    let pixel_bytes_u32 = u32::try_from(dimensions.pixel_bytes)
        .map_err(|_| Error::invalid_data("BMP pixel data exceeds its 32-bit header"))?;

    let mut header = [0_u8; BMP_V4_HEADER_LENGTH];
    header[..2].copy_from_slice(b"BM");
    header[2..6].copy_from_slice(&file_size_u32.to_le_bytes());
    header[10..14].copy_from_slice(&BMP_V4_PIXEL_OFFSET.to_le_bytes());
    header[14..18].copy_from_slice(&108_u32.to_le_bytes());
    header[18..22].copy_from_slice(&width.to_le_bytes());
    header[22..26].copy_from_slice(&(-height).to_le_bytes());
    header[26..28].copy_from_slice(&1_u16.to_le_bytes());
    header[28..30].copy_from_slice(&32_u16.to_le_bytes());
    header[30..34].copy_from_slice(&3_u32.to_le_bytes());
    header[34..38].copy_from_slice(&pixel_bytes_u32.to_le_bytes());
    header[54..58].copy_from_slice(&0x00ff_0000_u32.to_le_bytes());
    header[58..62].copy_from_slice(&0x0000_ff00_u32.to_le_bytes());
    header[62..66].copy_from_slice(&0x0000_00ff_u32.to_le_bytes());
    header[66..70].copy_from_slice(&0xff00_0000_u32.to_le_bytes());
    header[70..74].copy_from_slice(&0x7352_4742_u32.to_le_bytes());
    output.write_all(&header)?;
    write_bgra_rows(image, dimensions, row_order, output)
}

fn write_tga<W: Write>(
    image: &RgbaImage,
    dimensions: ImageDimensions,
    row_order: ImageRowOrder,
    output: &mut BoundedWriter<'_, W>,
) -> Result<()> {
    let width = u16::try_from(image.width)
        .map_err(|_| Error::invalid_data("TGA width exceeds 65535 pixels"))?;
    let height = u16::try_from(image.height)
        .map_err(|_| Error::invalid_data("TGA height exceeds 65535 pixels"))?;
    let file_size = TGA_HEADER_BYTES
        .checked_add(dimensions.pixel_bytes)
        .ok_or_else(|| Error::invalid_data("TGA file size overflowed"))?;
    ensure_output_budget(file_size, output.maximum(), "TGA")?;

    let mut header = [0_u8; TGA_HEADER_LENGTH];
    header[2] = 2;
    header[12..14].copy_from_slice(&width.to_le_bytes());
    header[14..16].copy_from_slice(&height.to_le_bytes());
    header[16] = 32;
    header[17] = 0x28;
    output.write_all(&header)?;
    write_bgra_rows(image, dimensions, row_order, output)
}

fn write_webp<W: Write>(
    image: &RgbaImage,
    dimensions: ImageDimensions,
    row_order: ImageRowOrder,
    maximum_working_bytes: u64,
    output: &mut BoundedWriter<'_, W>,
) -> Result<()> {
    if image.width > WEBP_MAX_DIMENSION || image.height > WEBP_MAX_DIMENSION {
        return Err(Error::invalid_data(format!(
            "WebP dimensions {}x{} exceed the lossless format maximum {WEBP_MAX_DIMENSION}",
            image.width, image.height
        )));
    }
    if dimensions.pixel_bytes > maximum_working_bytes {
        return Err(Error::invalid_data(format!(
            "WebP encoder requires {} input bytes, exceeding working limit {}",
            dimensions.pixel_bytes, maximum_working_bytes
        )));
    }
    let pixels = display_order_rgba(image, dimensions, row_order)?;
    let result = catch_unwind(AssertUnwindSafe(|| {
        WebPEncoder::new(output).encode(
            pixels.as_ref(),
            image.width,
            image.height,
            WebPColorType::Rgba8,
        )
    }));
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(WebPEncodingError::IoError(error))) => Err(error.into()),
        Ok(Err(error)) => Err(Error::invalid_data(format!(
            "cannot encode lossless WebP: {error}"
        ))),
        Err(_) => Err(Error::invalid_data("lossless WebP encoder panicked")),
    }
}

fn display_order_rgba(
    image: &RgbaImage,
    dimensions: ImageDimensions,
    row_order: ImageRowOrder,
) -> Result<Cow<'_, [u8]>> {
    if row_order == ImageRowOrder::Display {
        return Ok(Cow::Borrowed(&image.pixels));
    }
    let length = usize::try_from(dimensions.pixel_bytes).map_err(|_| {
        Error::invalid_data("image encoder working buffer does not fit this platform")
    })?;
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate image encoder working buffer: {error}"
        ))
    })?;
    for output_row in 0..dimensions.height_usize {
        pixels.extend_from_slice(display_row(image, dimensions, row_order, output_row)?);
    }
    Ok(Cow::Owned(pixels))
}

fn write_bgra_rows<W: Write>(
    image: &RgbaImage,
    dimensions: ImageDimensions,
    row_order: ImageRowOrder,
    output: &mut BoundedWriter<'_, W>,
) -> Result<()> {
    let mut bgra = [0_u8; BGRA_WRITE_CHUNK_BYTES];
    for output_row in 0..dimensions.height_usize {
        let rgba = display_row(image, dimensions, row_order, output_row)?;
        for source_chunk in rgba.chunks(BGRA_WRITE_CHUNK_BYTES) {
            let destination_chunk = &mut bgra[..source_chunk.len()];
            for (source, destination) in source_chunk
                .chunks_exact(4)
                .zip(destination_chunk.chunks_exact_mut(4))
            {
                destination.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
            }
            output.write_all(destination_chunk)?;
        }
    }
    Ok(())
}

fn display_row(
    image: &RgbaImage,
    dimensions: ImageDimensions,
    row_order: ImageRowOrder,
    output_row: usize,
) -> Result<&[u8]> {
    if output_row >= dimensions.height_usize {
        return Err(Error::invalid_data("image output row is out of range"));
    }
    let source_row = match row_order {
        ImageRowOrder::UnityDecoded => dimensions.height_usize - 1 - output_row,
        ImageRowOrder::Display => output_row,
    };
    let start = source_row
        .checked_mul(dimensions.stride_usize)
        .ok_or_else(|| Error::invalid_data("image row offset overflowed"))?;
    let end = start
        .checked_add(dimensions.stride_usize)
        .ok_or_else(|| Error::invalid_data("image row end overflowed"))?;
    image
        .pixels
        .get(start..end)
        .ok_or_else(|| Error::invalid_data("image row exceeds the pixel buffer"))
}

fn write_png_chunk<W: Write>(
    output: &mut BoundedWriter<'_, W>,
    chunk_type: [u8; 4],
    data: &[u8],
) -> Result<()> {
    let length = u32::try_from(data.len())
        .map_err(|_| Error::invalid_data("PNG chunk exceeds its 32-bit length"))?;
    output.write_all(&length.to_be_bytes())?;
    output.write_all(&chunk_type)?;
    output.write_all(data)?;
    output.write_all(&png_crc32(chunk_type, data).to_be_bytes())?;
    Ok(())
}

fn png_crc32(chunk_type: [u8; 4], data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in chunk_type.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn ensure_output_budget(length: u64, maximum: u64, format: &str) -> Result<()> {
    if length > maximum {
        return Err(Error::invalid_data(format!(
            "{format} image output is {length} bytes, exceeding limit {maximum}"
        )));
    }
    Ok(())
}

struct IdatWriter<'borrow, 'output, W> {
    output: &'borrow mut BoundedWriter<'output, W>,
    buffer: Vec<u8>,
}

impl<'borrow, 'output, W: Write> IdatWriter<'borrow, 'output, W> {
    fn new(output: &'borrow mut BoundedWriter<'output, W>) -> io::Result<Self> {
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(PNG_IDAT_CHUNK_BYTES)
            .map_err(|error| {
                io::Error::other(format!("cannot allocate PNG IDAT chunk: {error}"))
            })?;
        Ok(Self { output, buffer })
    }

    fn flush_chunk(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let length = u32::try_from(self.buffer.len())
            .map_err(|_| io::Error::other("PNG IDAT chunk length does not fit in u32"))?;
        self.output.write_all(&length.to_be_bytes())?;
        self.output.write_all(b"IDAT")?;
        self.output.write_all(&self.buffer)?;
        self.output
            .write_all(&png_crc32(*b"IDAT", &self.buffer).to_be_bytes())?;
        self.buffer.clear();
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        self.flush_chunk()
    }
}

impl<W: Write> Write for IdatWriter<'_, '_, W> {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        let original_length = bytes.len();
        while !bytes.is_empty() {
            let available = PNG_IDAT_CHUNK_BYTES - self.buffer.len();
            let copied = available.min(bytes.len());
            self.buffer.extend_from_slice(&bytes[..copied]);
            bytes = &bytes[copied..];
            if self.buffer.len() == PNG_IDAT_CHUNK_BYTES {
                self.flush_chunk()?;
            }
        }
        Ok(original_length)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct BoundedWriter<'a, W> {
    output: &'a mut W,
    maximum: u64,
    written: u64,
}

impl<'a, W> BoundedWriter<'a, W> {
    const fn new(output: &'a mut W, maximum: u64) -> Self {
        Self {
            output,
            maximum,
            written: 0,
        }
    }

    const fn maximum(&self) -> u64 {
        self.maximum
    }

    const fn written(&self) -> u64 {
        self.written
    }
}

impl<W: Write> Write for BoundedWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(bytes.len())
            .map_err(|_| io::Error::other("image write length does not fit in u64"))?;
        let next = self
            .written
            .checked_add(length)
            .ok_or_else(|| io::Error::other("image output length overflowed"))?;
        if next > self.maximum {
            return Err(io::Error::other(format!(
                "image output exceeds limit {} bytes",
                self.maximum
            )));
        }
        let written = self.output.write(bytes)?;
        self.written =
            self.written
                .checked_add(u64::try_from(written).map_err(|_| {
                    io::Error::other("written image byte count does not fit in u64")
                })?)
                .ok_or_else(|| io::Error::other("written image byte count overflowed"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use flate2::read::ZlibDecoder;
    use image_webp::WebPDecoder;
    use zune_jpeg::JpegDecoder;

    use super::{
        ImageFormat, ImageRowOrder, PNG_SIGNATURE, png_crc32, write_rgba_image,
        write_rgba_image_with_quality,
    };
    use crate::texture::RgbaImage;

    fn two_by_two() -> RgbaImage {
        RgbaImage {
            width: 2,
            height: 2,
            pixels: vec![
                255, 0, 0, 1, 0, 255, 0, 2, // decoded bottom row
                0, 0, 255, 3, 255, 255, 255, 4, // decoded top row
            ],
        }
    }

    fn jpeg_color_bands() -> RgbaImage {
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(16 * 16 * 4).unwrap();
        for decoded_row in 0..16 {
            let pixel = if decoded_row < 8 {
                [255, 0, 0, 255]
            } else {
                // JPEG must preserve the hidden blue RGB rather than composite
                // this transparent half against black or white.
                [0, 0, 255, 0]
            };
            for _ in 0..16 {
                pixels.extend_from_slice(&pixel);
            }
        }
        RgbaImage {
            width: 16,
            height: 16,
            pixels,
        }
    }

    #[test]
    fn png_has_valid_chunks_crc_zlib_and_top_down_rgba_scanlines() {
        let image = two_by_two();
        let mut png = Vec::new();
        let written = write_rgba_image(
            &image,
            ImageFormat::Png,
            ImageRowOrder::UnityDecoded,
            1_000_000,
            &mut png,
        )
        .unwrap();
        assert_eq!(written, u64::try_from(png.len()).unwrap());
        assert_eq!(&png[..8], PNG_SIGNATURE);

        let mut cursor = 8;
        let mut idat = Vec::new();
        let mut saw_ihdr = false;
        let mut saw_iend = false;
        while cursor < png.len() {
            let length = usize::try_from(u32::from_be_bytes(
                png[cursor..cursor + 4].try_into().unwrap(),
            ))
            .unwrap();
            cursor += 4;
            let kind: &[u8; 4] = png[cursor..cursor + 4].try_into().unwrap();
            cursor += 4;
            let data = &png[cursor..cursor + length];
            cursor += length;
            let crc = u32::from_be_bytes(png[cursor..cursor + 4].try_into().unwrap());
            cursor += 4;
            assert_eq!(crc, png_crc32(*kind, data));
            match kind {
                b"IHDR" => {
                    saw_ihdr = true;
                    assert_eq!(&data[..8], &[0, 0, 0, 2, 0, 0, 0, 2]);
                    assert_eq!(&data[8..], &[8, 6, 0, 0, 0]);
                }
                b"IDAT" => idat.extend_from_slice(data),
                b"IEND" => saw_iend = true,
                _ => panic!("unexpected PNG chunk"),
            }
        }
        assert!(saw_ihdr && saw_iend);

        let mut scanlines = Vec::new();
        ZlibDecoder::new(idat.as_slice())
            .read_to_end(&mut scanlines)
            .unwrap();
        assert_eq!(
            scanlines,
            [
                0, 0, 0, 255, 3, 255, 255, 255, 4, // top row
                0, 255, 0, 0, 1, 0, 255, 0, 2, // bottom row
            ]
        );
    }

    #[test]
    fn png_streams_large_compressed_data_across_bounded_idat_chunks() {
        let mut state = 0x1234_5678_u32;
        let mut pixels = Vec::with_capacity(256 * 128 * 4);
        for _ in 0..256 * 128 * 4 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            pixels.push(state.to_le_bytes()[0]);
        }
        let image = RgbaImage {
            width: 256,
            height: 128,
            pixels,
        };
        let mut png = Vec::new();
        write_rgba_image(
            &image,
            ImageFormat::Png,
            ImageRowOrder::Display,
            1_000_000,
            &mut png,
        )
        .unwrap();

        let mut cursor = PNG_SIGNATURE.len();
        let mut idat_count = 0;
        while cursor < png.len() {
            let length = usize::try_from(u32::from_be_bytes(
                png[cursor..cursor + 4].try_into().unwrap(),
            ))
            .unwrap();
            let kind = &png[cursor + 4..cursor + 8];
            if kind == b"IDAT" {
                idat_count += 1;
                assert!(length <= super::PNG_IDAT_CHUNK_BYTES);
            }
            cursor += 12 + length;
        }
        assert!(idat_count >= 2);
    }

    #[test]
    fn bmp_v4_header_and_pixels_preserve_alpha_in_top_down_order() {
        let image = two_by_two();
        let mut bmp = Vec::new();
        write_rgba_image(
            &image,
            ImageFormat::Bmp,
            ImageRowOrder::UnityDecoded,
            1_000_000,
            &mut bmp,
        )
        .unwrap();

        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()), 138);
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 122);
        assert_eq!(u32::from_le_bytes(bmp[14..18].try_into().unwrap()), 108);
        assert_eq!(i32::from_le_bytes(bmp[22..26].try_into().unwrap()), -2);
        assert_eq!(
            u32::from_le_bytes(bmp[66..70].try_into().unwrap()),
            0xff00_0000
        );
        assert_eq!(
            &bmp[122..],
            &[
                255, 0, 0, 3, 255, 255, 255, 4, // display top, BGRA
                0, 0, 255, 1, 0, 255, 0, 2, // display bottom, BGRA
            ]
        );
    }

    #[test]
    fn tga_marks_top_left_origin_and_does_not_flip_display_order_again() {
        let mut image = two_by_two();
        image.pixels.rotate_left(8);
        let mut tga = Vec::new();
        write_rgba_image(
            &image,
            ImageFormat::Tga,
            ImageRowOrder::Display,
            1_000_000,
            &mut tga,
        )
        .unwrap();

        assert_eq!(tga[2], 2);
        assert_eq!(&tga[12..16], &[2, 0, 2, 0]);
        assert_eq!(&tga[16..18], &[32, 0x28]);
        assert_eq!(
            &tga[18..],
            &[
                255, 0, 0, 3, 255, 255, 255, 4, // display top, BGRA
                0, 0, 255, 1, 0, 255, 0, 2, // display bottom, BGRA
            ]
        );
    }

    #[test]
    fn raw_rgba_keeps_the_existing_ir_and_row_order_contracts() {
        let image = two_by_two();
        let mut decoded = Vec::new();
        write_rgba_image(
            &image,
            ImageFormat::RawRgba,
            ImageRowOrder::UnityDecoded,
            1_000_000,
            &mut decoded,
        )
        .unwrap();
        assert_eq!(&decoded[..16], b"HARUKI_RGBAIR_V1");
        assert_eq!(&decoded[36..44], &image.pixels[8..16]);

        let mut display = Vec::new();
        write_rgba_image(
            &image,
            ImageFormat::RawRgba,
            ImageRowOrder::Display,
            1_000_000,
            &mut display,
        )
        .unwrap();
        assert_eq!(&display[36..], image.pixels);
    }

    #[test]
    fn webp_is_lossless_top_down_rgba_and_enforces_format_dimensions() {
        let image = two_by_two();
        let mut webp = Vec::new();
        let written = write_rgba_image(
            &image,
            ImageFormat::Webp,
            ImageRowOrder::UnityDecoded,
            1_000_000,
            &mut webp,
        )
        .unwrap();
        assert_eq!(written, u64::try_from(webp.len()).unwrap());
        assert_eq!(&webp[..4], b"RIFF");
        assert_eq!(&webp[8..12], b"WEBP");
        assert_eq!(&webp[12..16], b"VP8L");

        let mut decoder = WebPDecoder::new(Cursor::new(&webp)).unwrap();
        assert_eq!(decoder.dimensions(), (2, 2));
        assert!(decoder.has_alpha());
        assert!(!decoder.is_lossy());
        let mut restored_pixels = vec![0; decoder.output_buffer_size().unwrap()];
        decoder.read_image(&mut restored_pixels).unwrap();
        assert_eq!(
            restored_pixels,
            [
                0, 0, 255, 3, 255, 255, 255, 4, // display top
                255, 0, 0, 1, 0, 255, 0, 2, // display bottom
            ]
        );

        let oversized = RgbaImage {
            width: super::WEBP_MAX_DIMENSION + 1,
            height: 1,
            pixels: vec![0; usize::try_from((super::WEBP_MAX_DIMENSION + 1) * 4).unwrap()],
        };
        let error = write_rgba_image(
            &oversized,
            ImageFormat::Webp,
            ImageRowOrder::Display,
            1_000_000,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("format maximum"));
    }

    #[test]
    fn jpeg_is_top_down_discards_alpha_and_validates_quality_and_dimensions() {
        let mut jpeg = Vec::new();
        let written = write_rgba_image_with_quality(
            &jpeg_color_bands(),
            ImageFormat::Jpeg,
            ImageRowOrder::UnityDecoded,
            100,
            1_000_000,
            &mut jpeg,
        )
        .unwrap();
        assert_eq!(written, u64::try_from(jpeg.len()).unwrap());
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xff, 0xd9]);

        let mut decoder = JpegDecoder::new(Cursor::new(&jpeg));
        let restored = decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        assert_eq!((info.width, info.height, info.components), (16, 16, 3));
        let top_blue = &restored[(2 * 16 + 2) * 3..][..3];
        assert!(top_blue[2] > 220 && top_blue[0] < 30 && top_blue[1] < 30);
        let bottom_red = &restored[(13 * 16 + 2) * 3..][..3];
        assert!(bottom_red[0] > 220 && bottom_red[1] < 30 && bottom_red[2] < 30);

        for quality in [0, 101] {
            assert!(
                write_rgba_image_with_quality(
                    &jpeg_color_bands(),
                    ImageFormat::Jpeg,
                    ImageRowOrder::Display,
                    quality,
                    1_000_000,
                    &mut Vec::new(),
                )
                .is_err()
            );
        }

        let oversized = RgbaImage {
            width: u32::from(u16::MAX) + 1,
            height: 1,
            pixels: vec![0; (usize::from(u16::MAX) + 1) * 4],
        };
        let error = write_rgba_image(
            &oversized,
            ImageFormat::Jpeg,
            ImageRowOrder::Display,
            1_000_000,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("65535"));
    }

    #[test]
    fn rejects_bad_pixel_lengths_and_every_format_respects_output_budget() {
        let malformed = RgbaImage {
            width: 2,
            height: 2,
            pixels: vec![0; 15],
        };
        assert!(
            write_rgba_image(
                &malformed,
                ImageFormat::Png,
                ImageRowOrder::Display,
                u64::MAX,
                &mut Vec::new(),
            )
            .is_err()
        );

        for format in [
            ImageFormat::Jpeg,
            ImageFormat::Png,
            ImageFormat::Bmp,
            ImageFormat::Tga,
            ImageFormat::Webp,
            ImageFormat::RawRgba,
        ] {
            assert!(
                write_rgba_image(
                    &two_by_two(),
                    format,
                    ImageRowOrder::Display,
                    1,
                    &mut Vec::new(),
                )
                .is_err(),
                "{format:?} ignored its output budget"
            );
        }
    }
}
