//! Unity's `PackedFloatVector` and `PackedIntVector` bit packing.
//!
//! Both compressed animation curves and compressed meshes store their values as
//! a dense little-endian bit stream at a per-vector width, so the reader and the
//! two container types live here rather than being duplicated per consumer.
//!
//! Every read is checked against the buffer, so a hostile width or item count
//! reports an error instead of reading past the data.

use crate::{Error, Result};

/// Reads little-endian bit fields from a packed vector's payload.
pub(crate) struct PackedBitReader<'a> {
    bytes: &'a [u8],
    bit_position: usize,
}

impl<'a> PackedBitReader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_position: 0,
        }
    }

    /// Positions the cursor at `item` values of `bit_size` bits.
    pub(crate) fn seek_to_item(&mut self, item: usize, bit_size: u8) -> Result<()> {
        self.bit_position = item
            .checked_mul(usize::from(bit_size))
            .ok_or_else(|| Error::invalid_data("packed vector start offset overflowed"))?;
        Ok(())
    }

    pub(crate) fn read(&mut self, bit_count: u8) -> Result<u32> {
        if bit_count > 32 {
            return Err(Error::invalid_data(format!(
                "packed vector bit width {bit_count} exceeds 32"
            )));
        }
        let end = self
            .bit_position
            .checked_add(usize::from(bit_count))
            .ok_or_else(|| Error::invalid_data("packed vector bit range overflowed"))?;
        let available = self
            .bytes
            .len()
            .checked_mul(8)
            .ok_or_else(|| Error::invalid_data("packed vector byte size overflowed"))?;
        if end > available {
            return Err(Error::invalid_data("packed vector ends inside a value"));
        }
        let mut output = 0_u32;
        for output_bit in 0..usize::from(bit_count) {
            let source_bit = self.bit_position + output_bit;
            let bit = (self.bytes[source_bit / 8] >> (source_bit % 8)) & 1;
            output |= u32::from(bit) << output_bit;
        }
        self.bit_position = end;
        Ok(output)
    }
}

/// A quantized float array: `start + value / ((2^bit_size - 1) / range)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PackedFloatVector {
    pub item_count: u32,
    pub range: f32,
    pub start: f32,
    pub data: Vec<u8>,
    pub bit_size: u8,
}

impl PackedFloatVector {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.item_count == 0
    }

    /// Unpacks `chunk_count` chunks of `items_per_chunk` values, beginning at
    /// value index `first_item`.
    ///
    /// The arithmetic stays in `f32` throughout, matching the managed reader.
    /// Widening it would change the shortest round-trip text the OBJ writer
    /// produces and so diverge from the differential oracle.
    pub fn unpack(
        &self,
        items_per_chunk: usize,
        first_item: usize,
        chunk_count: Option<usize>,
    ) -> Result<Vec<f32>> {
        if items_per_chunk == 0 {
            return Err(Error::invalid_data(
                "packed float chunk size cannot be zero",
            ));
        }
        if self.bit_size == 0 || self.bit_size > 32 {
            return Err(Error::invalid_data(format!(
                "packed float bit width {} is out of range",
                self.bit_size
            )));
        }
        let item_count = usize::try_from(self.item_count)
            .map_err(|_| Error::invalid_data("packed float item count does not fit usize"))?;
        let chunks = chunk_count.unwrap_or(item_count / items_per_chunk);
        let total = chunks
            .checked_mul(items_per_chunk)
            .ok_or_else(|| Error::invalid_data("packed float value count overflowed"))?;

        // `2^bit_size - 1` is the largest packed value; the reference divides by
        // it, so a width of 32 must not overflow the shift.
        //
        // The order of operations is the reference's, not a tidier equivalent.
        // It takes the reciprocal of the range in `f32`, multiplies that by the
        // largest packed value, and then divides and adds -- three roundings.
        // Computing one scale in `f64` and fusing the multiply-add is the same
        // arithmetic and different bits: `mul_add` rounds once. The two agree
        // whenever the scale comes out exactly 1, which is what a range of 255
        // at eight bits does, and that is precisely the shape the differential
        // fixture used to have, so the divergence sat here unseen.
        #[allow(clippy::cast_precision_loss)]
        let maximum = (u32::MAX >> (32 - u32::from(self.bit_size))) as f32;
        let denominator = (1.0_f32 / self.range) * maximum;

        let mut reader = PackedBitReader::new(&self.data);
        reader.seek_to_item(first_item, self.bit_size)?;
        let mut output = Vec::new();
        output.try_reserve_exact(total).map_err(|error| {
            Error::invalid_data(format!("cannot allocate packed floats: {error}"))
        })?;
        for _ in 0..total {
            #[allow(clippy::cast_precision_loss)]
            let value = reader.read(self.bit_size)? as f32;
            output.push(value / denominator + self.start);
        }
        Ok(output)
    }
}

/// A quantized integer array at a fixed bit width.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PackedIntVector {
    pub item_count: u32,
    pub data: Vec<u8>,
    pub bit_size: u8,
}

impl PackedIntVector {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.item_count == 0
    }

    pub fn unpack(&self) -> Result<Vec<u32>> {
        if self.item_count == 0 {
            return Ok(Vec::new());
        }
        if self.bit_size == 0 || self.bit_size > 32 {
            return Err(Error::invalid_data(format!(
                "packed integer bit width {} is out of range",
                self.bit_size
            )));
        }
        let item_count = usize::try_from(self.item_count)
            .map_err(|_| Error::invalid_data("packed integer count does not fit usize"))?;
        let mut reader = PackedBitReader::new(&self.data);
        let mut output = Vec::new();
        output.try_reserve_exact(item_count).map_err(|error| {
            Error::invalid_data(format!("cannot allocate packed integers: {error}"))
        })?;
        for _ in 0..item_count {
            output.push(reader.read(self.bit_size)?);
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::{PackedFloatVector, PackedIntVector};

    /// Packs `values` at `bit_size` bits each, little-endian.
    fn pack(values: &[u32], bit_size: u8) -> Vec<u8> {
        let mut bits = Vec::new();
        for value in values {
            for bit in 0..bit_size {
                bits.push((value >> bit) & 1 == 1);
            }
        }
        let mut bytes = vec![0_u8; bits.len().div_ceil(8)];
        for (position, set) in bits.iter().enumerate() {
            if *set {
                bytes[position / 8] |= 1 << (position % 8);
            }
        }
        bytes
    }

    #[test]
    fn unpacks_floats_across_the_quantized_range() {
        // Four bits: 0 maps to start and 15 to start + range.
        let vector = PackedFloatVector {
            item_count: 4,
            range: 2.0,
            start: -1.0,
            data: pack(&[0, 5, 10, 15], 4),
            bit_size: 4,
        };
        let values = vector.unpack(1, 0, None).unwrap();
        assert_eq!(values.len(), 4);
        assert!((values[0] + 1.0).abs() < 1e-6);
        assert!((values[3] - 1.0).abs() < 1e-6);
        assert!(values[1] < values[2]);
    }

    #[test]
    fn unpacking_honours_the_start_item_and_chunk_count() {
        let vector = PackedFloatVector {
            item_count: 6,
            range: 15.0,
            start: 0.0,
            data: pack(&[0, 1, 2, 3, 4, 5], 4),
            bit_size: 4,
        };
        // Skip the first chunk of two, then take two chunks of two.
        let values = vector.unpack(2, 2, Some(2)).unwrap();
        #[allow(clippy::cast_possible_truncation)]
        let rounded: Vec<i32> = values.iter().map(|value| value.round() as i32).collect();
        assert_eq!(rounded, [2, 3, 4, 5]);
    }

    #[test]
    fn rejects_widths_and_lengths_the_payload_cannot_satisfy() {
        let short = PackedFloatVector {
            item_count: 8,
            range: 1.0,
            start: 0.0,
            data: vec![0_u8; 1],
            bit_size: 8,
        };
        assert!(short.unpack(1, 0, None).is_err());

        let zero_width = PackedFloatVector {
            bit_size: 0,
            ..short.clone()
        };
        assert!(zero_width.unpack(1, 0, None).is_err());
        assert!(short.unpack(0, 0, None).is_err());

        let integers = PackedIntVector {
            item_count: 8,
            data: vec![0_u8; 1],
            bit_size: 8,
        };
        assert!(integers.unpack().is_err());
    }

    #[test]
    fn unpacks_integers_at_a_non_byte_width() {
        let vector = PackedIntVector {
            item_count: 5,
            data: pack(&[1, 2, 3, 0, 2], 3),
            bit_size: 3,
        };
        assert_eq!(vector.unpack().unwrap(), [1, 2, 3, 0, 2]);
    }
}
