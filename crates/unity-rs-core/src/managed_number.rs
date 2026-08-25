//! .NET's default number text, for the documents that have to reproduce it.
//!
//! Several formats this crate writes are text the managed implementation also
//! writes, and it renders every number through `ToString()` under the general
//! format: the shortest digits that round-trip, laid out as a plain decimal
//! while the decimal exponent satisfies `-5 < exponent < precision`, and in
//! scientific notation with a two-digit-minimum exponent otherwise. Rust's
//! `Display` never switches to an exponent, so the two agree only inside that
//! band -- which is wide enough that ordinary test values never leave it, and
//! narrow enough that real data does. A normal component of `4.3e-8` is
//! ordinary floating-point noise, and .NET writes it `4.3E-08`.
//!
//! This module exists because the OBJ writer and the `TypeTree` dump each
//! answered this separately, one of them wrongly, and both got the tie case
//! wrong. It is deliberately not [`crate::live2d_number`], which implements the
//! two *other* formats the managed Cubism extractor uses -- `"0.###"` and
//! Newtonsoft's default float -- neither of which is this one.
//!
//! Culture: this targets `InvariantCulture` deliberately. It is what the
//! managed exporter runs under (`InvariantGlobalization` is set in the shipped
//! projects) and what a runner without a locale falls back to. Under a culture
//! such as de-DE the managed text would use a decimal comma and a localized
//! infinity symbol; that is not reproduced, and these documents are not
//! culture-sensitive.

use std::fmt::{self, Write as _};

/// Round-trip digit counts .NET uses as the general-format precision.
const SINGLE_PRECISION: i32 = 9;
const DOUBLE_PRECISION: i32 = 17;

/// Long enough for any `f64`: `-1.7976931348623157E+308` is 24 characters, and
/// the widest fixed form, a negative value at exponent -5 with seventeen
/// digits, is 25.
const CAPACITY: usize = 40;

/// One number rendered as .NET renders it, held without allocating.
pub(crate) struct ManagedNumber {
    bytes: [u8; CAPACITY],
    length: usize,
}

impl ManagedNumber {
    /// Renders a finite `f32`. Non-finite values are the caller's to handle,
    /// because the callers disagree about them: the OBJ writer substitutes
    /// `NaN` with `0`, and the dump writes the managed spelling.
    pub(crate) fn single(value: f32) -> Option<Self> {
        let mut shortest = Self::empty();
        write!(shortest, "{value:e}").ok()?;
        let significant = significant_digits(shortest.as_str())?;
        // Re-rendering at exactly the shortest form's digit count is what makes
        // ties come out the way .NET's do. Both round to nearest, but Rust's
        // shortest form breaks an exact tie away from zero while its
        // fixed-precision form breaks it to even, which is .NET's rule:
        // 1298351.25 is 1298351.3 one way and 1298351.2 the other.
        let mut rendered = Self::empty();
        write!(rendered, "{:.*e}", significant - 1, value).ok()?;
        Self::from_exponential(rendered.as_str(), SINGLE_PRECISION)
    }

    /// Renders a finite `f64`.
    pub(crate) fn double(value: f64) -> Option<Self> {
        let mut shortest = Self::empty();
        write!(shortest, "{value:e}").ok()?;
        let significant = significant_digits(shortest.as_str())?;
        let mut rendered = Self::empty();
        write!(rendered, "{:.*e}", significant - 1, value).ok()?;
        Self::from_exponential(rendered.as_str(), DOUBLE_PRECISION)
    }

    pub(crate) fn as_str(&self) -> &str {
        // Filled only through `write_str` and `push`, and `push` is only handed
        // ASCII.
        std::str::from_utf8(&self.bytes[..self.length]).unwrap_or_default()
    }

    const fn empty() -> Self {
        Self {
            bytes: [0; CAPACITY],
            length: 0,
        }
    }

    fn push(&mut self, byte: u8) -> Option<()> {
        *self.bytes.get_mut(self.length)? = byte;
        self.length += 1;
        Some(())
    }

    fn push_str(&mut self, text: &str) -> Option<()> {
        let end = self.length.checked_add(text.len())?;
        self.bytes
            .get_mut(self.length..end)?
            .copy_from_slice(text.as_bytes());
        self.length = end;
        Some(())
    }

    /// Lays out `d.dddde±X`, as Rust's `LowerExp` writes it, the way .NET's
    /// general format would.
    fn from_exponential(exponential: &str, precision: i32) -> Option<Self> {
        let (mantissa, exponent) = exponential.split_once('e')?;
        let exponent: i32 = exponent.parse().ok()?;
        let (sign, mantissa) = match mantissa.strip_prefix('-') {
            Some(rest) => ("-", rest),
            None => ("", mantissa),
        };

        let mut digits = Self::empty();
        for byte in mantissa.bytes().filter(u8::is_ascii_digit) {
            digits.push(byte)?;
        }
        let digits = digits.as_str();
        if digits.is_empty() {
            return None;
        }
        let mut out = Self::empty();
        out.push_str(sign)?;
        if digits.bytes().all(|digit| digit == b'0') {
            // Rust renders zero as `0e0`; .NET keeps the sign of negative zero.
            out.push_str("0")?;
            return Some(out);
        }
        // Rounding up to the next power of ten leaves trailing zeros behind,
        // and they carry nothing once the exponent is written separately.
        let digits = digits.trim_end_matches('0');
        let digits = if digits.is_empty() { "0" } else { digits };

        if exponent > -5 && exponent < precision {
            out.push_fixed(digits, exponent)?;
        } else {
            out.push_str(digits.get(..1)?)?;
            if let Some(rest) = digits.get(1..)
                && !rest.is_empty()
            {
                out.push(b'.')?;
                out.push_str(rest)?;
            }
            out.push(b'E')?;
            out.push(if exponent < 0 { b'-' } else { b'+' })?;
            let magnitude = exponent.unsigned_abs();
            if magnitude < 10 {
                out.push(b'0')?;
            }
            let mut digits_out = Self::empty();
            write!(digits_out, "{magnitude}").ok()?;
            out.push_str(digits_out.as_str())?;
        }
        Some(out)
    }

    fn push_fixed(&mut self, digits: &str, exponent: i32) -> Option<()> {
        if exponent < 0 {
            // -4 through -1: a leading zero, then the gap before the digits.
            self.push_str("0.")?;
            for _ in 0..(-exponent - 1) {
                self.push(b'0')?;
            }
            return self.push_str(digits);
        }
        let integer_digits = usize::try_from(exponent).ok()? + 1;
        if digits.len() <= integer_digits {
            self.push_str(digits)?;
            for _ in 0..(integer_digits - digits.len()) {
                self.push(b'0')?;
            }
            Some(())
        } else {
            self.push_str(digits.get(..integer_digits)?)?;
            self.push(b'.')?;
            self.push_str(digits.get(integer_digits..)?)
        }
    }
}

impl fmt::Write for ManagedNumber {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.push_str(text).ok_or(fmt::Error)
    }
}

/// Counts the significant digits in `d.dddde±X`.
fn significant_digits(exponential: &str) -> Option<usize> {
    let count = exponential
        .split_once('e')?
        .0
        .bytes()
        .filter(u8::is_ascii_digit)
        .count();
    (count > 0).then_some(count)
}

#[cfg(test)]
mod tests {
    use super::ManagedNumber;

    /// Every value formatted by the managed reader on .NET 10. See the file
    /// header for how it was produced.
    const MANAGED_FLOAT_FORMAT: &str = include_str!("../tests/fixtures/managed/float_format.txt");

    #[test]
    fn text_matches_the_managed_formatter_for_every_recorded_value() {
        let mut checked = 0_usize;
        let mut ties = 0_usize;
        for line in MANAGED_FLOAT_FORMAT.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let mut columns = line.split('\t');
            let kind = columns.next().expect("value kind");
            let bits = columns.next().expect("bit pattern");
            let expected = columns.next().expect("expected text");
            assert!(columns.next().is_none(), "unexpected column in {line:?}");

            let actual = match kind {
                "f" => {
                    let value = f32::from_bits(bits.parse().unwrap());
                    // A tie is exactly where the shortest round-trip form and
                    // the correctly rounded form at the same digit count
                    // disagree, so counting them needs no second formatter.
                    let shortest = format!("{value:e}");
                    let significant = super::significant_digits(&shortest).unwrap();
                    if shortest != format!("{:.*e}", significant - 1, value) {
                        ties += 1;
                    }
                    ManagedNumber::single(value)
                }
                "d" => ManagedNumber::double(f64::from_bits(bits.parse().unwrap())),
                other => panic!("unknown value kind {other:?}"),
            };
            assert_eq!(
                actual.expect("finite value renders").as_str(),
                expected,
                "{kind} bits {bits}"
            );
            checked += 1;
        }
        assert!(checked > 500, "only {checked} values were compared");
        // Without this the file could lose its tie entries and the comparison
        // would go on passing while the case they cover went unchecked -- which
        // is how the tie rounding stayed wrong through 800 recorded values.
        assert!(
            ties > 0,
            "the fixture holds no value that ties at the last digit"
        );
    }

    #[test]
    fn renders_singles_the_way_dotnet_does() {
        // Probed from .NET 10:
        //   string.Format(CultureInfo.InvariantCulture, "{0}", value)
        // Values are bit patterns so nothing is rounded on the way in.
        const CASES: &[(u32, &str)] = &[
            (0, "0"),
            (2_147_483_648, "-0"),
            (1_056_964_608, "0.5"),
            (3_214_934_016, "-1.25"),
            (1_120_403_456, "100"),
            (1_036_831_949, "0.1"),
            (1_051_372_203, "0.33333334"),
            (953_267_991, "0.0001"),       // the smallest fixed magnitude
            (953_267_990, "9.999999E-05"), // just below it
            (925_353_388, "1E-05"),
            (1_315_859_238, "999999900"), // the largest fixed magnitude
            (1_315_859_240, "1E+09"),     // just above it
            (1_318_263_473, "1.234E+09"),
            (1_235_123_578, "1298351.2"), // a tie, rounded to even
            (1_184_677_024, "20062.312"), // another tie
            (1, "1E-45"),                 // the smallest subnormal
            (8_388_607, "1.1754942E-38"), // the largest subnormal
            (8_388_608, "1.1754944E-38"), // the smallest normal
            (4_286_578_687, "-3.4028235E+38"),
            (2_139_095_039, "3.4028235E+38"),
            (1_266_679_808, "16777216"),
            (1_290_500_515, "123456790"),
        ];
        for (bits, expected) in CASES {
            let value = f32::from_bits(*bits);
            assert_eq!(
                ManagedNumber::single(value).unwrap().as_str(),
                *expected,
                "bits {bits}"
            );
        }
    }

    #[test]
    fn renders_doubles_the_way_dotnet_does() {
        // Doubles switch to scientific at a different exponent than floats do,
        // which is the whole reason the precision is a parameter.
        const CASES: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "-0"),
            (0.1, "0.1"),
            (1.0 / 3.0, "0.3333333333333333"),
            (0.0001, "0.0001"),
            (1e-5, "1E-05"),
            (1e13, "10000000000000"),
            (1e16, "10000000000000000"), // still fixed, unlike an f32
            (1e17, "1E+17"),             // the first scientific one
            (1.5e15, "1500000000000000"),
            (1e300, "1E+300"), // a three-digit exponent
            (5e-324, "5E-324"),
            (f64::MAX, "1.7976931348623157E+308"),
            (f64::MIN, "-1.7976931348623157E+308"),
        ];
        for (value, expected) in CASES {
            assert_eq!(
                ManagedNumber::double(*value).unwrap().as_str(),
                *expected,
                "value {value}"
            );
        }
    }

    #[test]
    fn a_float_and_the_same_value_widened_can_render_differently() {
        // The reason both entry points exist: widening changes the shortest
        // round-trip digits, so a `float` field rendered as a double would
        // carry the expansion instead of the value Unity stored.
        assert_eq!(ManagedNumber::single(0.1_f32).unwrap().as_str(), "0.1");
        assert_eq!(
            ManagedNumber::double(f64::from(0.1_f32)).unwrap().as_str(),
            "0.10000000149011612"
        );
    }
}
