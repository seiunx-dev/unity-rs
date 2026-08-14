//! Number formatting for the Cubism JSON documents.
//!
//! The managed extractor writes these files with two different float formats,
//! sometimes in the same document, and neither is what Rust produces by
//! default. Matching them is what makes the documents comparable byte for byte
//! rather than merely equivalent, which is the difference between an oracle
//! that can check any model and one that can only check values short enough to
//! print the same way by accident.
//!
//! That comparison is real now rather than aspirational: the differential
//! compares each Cubism document's bytes against the one the managed extractor
//! wrote, not only against what it parses to. Getting there took fixing the
//! layout as well as the numbers -- see [`crate::live2d_package`] -- because a
//! value comparison had been hiding the fact that the two disagreed about
//! every object and array in every document.
//!
//! Neither format here is .NET's default `ToString()`, which is
//! [`crate::managed_number`]; these are the two the Cubism extractor asks for
//! specifically.
//!
//! Both formatters take `f32` because Unity serializes these fields as floats
//! and both formats depend on the value's shortest *decimal* form, which
//! changes when the value is widened: `0.8f` widened to `f64` prints as
//! `0.800000011920929`, and `0.0025f` rounds to `0.002` rather than `0.003`
//! because its true binary value is `0.00249999994`.

use core::cmp::Ordering;
use core::fmt::Write as _;

/// Formats a number the way .NET's `"0.###"` does.
///
/// The managed extractor hands this format to Newtonsoft for every float in
/// physics3.json and for the segment lists in motion3.json. Three rules follow
/// from it:
///
/// * the value is first reduced to seven significant digits, .NET's legacy
///   single-precision display width;
/// * then rounded to at most three decimals, halves away from zero rather than
///   to even;
/// * then trailing zeros and a bare decimal point are dropped, so an integral
///   value prints without one.
///
/// A negative value keeps its sign even when it rounds to zero, which is why
/// `-0.0004` prints as `-0` and not `0`.
pub(crate) fn three_decimals(value: f32) -> String {
    const SIGNIFICANT: usize = 7;
    const DECIMALS: i32 = 3;

    let (negative, mut digits, mut point) = decompose(value);
    if digits.is_empty() {
        return if negative {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }

    round_significant(&mut digits, &mut point, SIGNIFICANT);
    let keep = point + DECIMALS;
    match keep.cmp(&0) {
        Ordering::Less => digits.clear(),
        // Exactly at the rounding position: the first digit decides whether
        // anything survives at all.
        Ordering::Equal => {
            if digits[0] >= 5 {
                digits = vec![1];
                point += 1;
            } else {
                digits.clear();
            }
        }
        Ordering::Greater => {
            let keep = usize::try_from(keep).expect("a nonnegative count fits");
            round_significant(&mut digits, &mut point, keep);
        }
    }

    let mut output = String::new();
    if negative {
        output.push('-');
    }
    render_fixed(&mut output, &digits, point, false);
    output
}

/// Formats a number the way Newtonsoft serializes a `float` by default.
///
/// This is what motion3.json uses for everything outside the segment lists. It
/// is the value's shortest round-trip decimal form, with two departures from
/// Rust's own output: an integral value keeps a trailing `.0`, and the notation
/// switches to .NET's exponent form outside a fixed range -- at or above `1e9`
/// and below `1e-4`, both measured on the leading digit's power of ten.
pub(crate) fn managed_float(value: f32) -> String {
    // The bounds .NET switches notation at, as powers of ten of the leading
    // digit: 1e8 prints in full and 1e9 does not, 1e-4 prints in full and 1e-5
    // does not.
    const FIXED_MAXIMUM: i32 = 9;
    const FIXED_MINIMUM: i32 = -4;

    let (negative, digits, point) = decompose(value);
    let mut output = String::new();
    if negative {
        output.push('-');
    }
    if digits.is_empty() {
        output.push_str("0.0");
        return output;
    }

    // `point` counts digits before the decimal separator; the exponent of the
    // leading digit is one less.
    let exponent = point - 1;
    if !(FIXED_MINIMUM..FIXED_MAXIMUM).contains(&exponent) {
        output.push(char::from(b'0' + digits[0]));
        if digits.len() > 1 {
            output.push('.');
            for digit in &digits[1..] {
                output.push(char::from(b'0' + digit));
            }
        }
        // .NET pads the exponent to two digits and always writes its sign.
        let _ = write!(
            output,
            "E{}{:02}",
            if exponent < 0 { '-' } else { '+' },
            exponent.abs()
        );
        return output;
    }

    render_fixed(&mut output, &digits, point, true);
    output
}

/// Splits a float into a sign and `0.d1d2... * 10^point` with `d1` nonzero.
///
/// Normalizing this way makes rounding by significant digit and by decimal
/// place the same operation at different offsets. The digits come from the
/// shortest form that round-trips the `f32`, which is what both .NET formats
/// round from.
fn decompose(value: f32) -> (bool, Vec<u8>, i32) {
    debug_assert!(value.is_finite(), "callers reject non-finite values first");
    let text = format!("{value}");
    let (negative, magnitude) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.as_str()),
    };
    let (whole, fraction) = match magnitude.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (magnitude, ""),
    };

    let mut digits: Vec<u8> = whole
        .bytes()
        .chain(fraction.bytes())
        .map(|byte| byte - b'0')
        .collect();
    let mut point = i32::try_from(whole.len()).expect("a formatted float is short");
    let leading = digits.iter().take_while(|digit| **digit == 0).count();
    digits.drain(..leading);
    point -= i32::try_from(leading).expect("a formatted float is short");
    while digits.last() == Some(&0) {
        digits.pop();
    }
    if digits.is_empty() {
        point = 0;
    }
    (negative, digits, point)
}

/// Rounds `digits` to `keep` significant digits, halves away from zero.
fn round_significant(digits: &mut Vec<u8>, point: &mut i32, keep: usize) {
    if keep >= digits.len() {
        return;
    }
    let round_up = digits[keep] >= 5;
    digits.truncate(keep);
    if round_up {
        let mut index = keep;
        loop {
            if index == 0 {
                digits.insert(0, 1);
                *point += 1;
                break;
            }
            index -= 1;
            if digits[index] == 9 {
                digits[index] = 0;
            } else {
                digits[index] += 1;
                break;
            }
        }
    }
    while digits.last() == Some(&0) {
        digits.pop();
    }
}

/// Renders `0.digits * 10^point` in plain decimal.
///
/// With `trailing_point` an integral value keeps a `.0`, which is how
/// Newtonsoft marks a value as floating point; without it the value prints as
/// a bare integer, which is what `"0.###"` does.
fn render_fixed(output: &mut String, digits: &[u8], point: i32, trailing_point: bool) {
    if digits.is_empty() {
        output.push('0');
        if trailing_point {
            output.push_str(".0");
        }
        return;
    }
    if point <= 0 {
        output.push_str("0.");
        for _ in 0..-point {
            output.push('0');
        }
        for digit in digits {
            output.push(char::from(b'0' + digit));
        }
        return;
    }

    let whole = usize::try_from(point).expect("a positive point fits");
    for index in 0..whole {
        output.push(char::from(b'0' + digits.get(index).copied().unwrap_or(0)));
    }
    if digits.len() > whole {
        output.push('.');
        for digit in &digits[whole..] {
            output.push(char::from(b'0' + digit));
        }
    } else if trailing_point {
        output.push_str(".0");
    }
}

#[cfg(test)]
mod tests {
    use super::{managed_float, three_decimals};

    /// Every expectation here is what .NET 10 printed for the same value:
    /// `float.ToString("0.###", InvariantCulture)`, the formatter the managed
    /// extractor hands to Newtonsoft. They are that program's output, not a
    /// reading of its documentation.
    #[test]
    fn three_decimals_matches_dotnet() {
        const CASES: &[(f32, &str)] = &[
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (100.0, "100"),
            (0.8, "0.8"),
            (0.95, "0.95"),
            (1.5, "1.5"),
            (0.25, "0.25"),
            // Rounding happens at three decimals, halves away from zero.
            (0.1234, "0.123"),
            (0.1235, "0.124"),
            (0.12349, "0.123"),
            (2.0005, "2.001"),
            (2.0015, "2.002"),
            (1.0005, "1.001"),
            (-2.0005, "-2.001"),
            // On the shortest decimal form, not the binary value: 0.0025f is
            // really 0.00249999994, which would round the other way.
            (0.0015, "0.002"),
            (0.0025, "0.003"),
            (0.0005, "0.001"),
            // Below half a thousandth nothing survives, but the sign does.
            (0.00049, "0"),
            (1e-5, "0"),
            (1e-10, "0"),
            (-0.0004, "-0"),
            // Seven significant digits first, which only shows above a million.
            (1234.5678, "1234.568"),
            (99999.99, "99999.99"),
            (1_000_000.06, "1000000"),
            (1_234_567.8, "1234568"),
            (12_345_678.0, "12345680"),
            (16_777_216.0, "16777220"),
            (123_456_792.0, "123456800"),
            (1e10, "10000000000"),
            (2.5, "2.5"),
            (3.5, "3.5"),
            (0.5, "0.5"),
        ];
        assert_formats(CASES, three_decimals, "0.###");
    }

    /// The same, for `JsonConvert.SerializeObject(float)` on Newtonsoft 13.
    #[test]
    fn managed_float_matches_newtonsoft() {
        const CASES: &[(f32, &str)] = &[
            // An integral value keeps a trailing .0 that Rust does not write.
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (30.0, "30.0"),
            (100.0, "100.0"),
            (1e6, "1000000.0"),
            (1e7, "10000000.0"),
            (1e8, "100000000.0"),
            (123_456_792.0, "123456790.0"),
            // Otherwise the shortest form that round-trips the float.
            (0.8, "0.8"),
            (0.95, "0.95"),
            (1.5, "1.5"),
            (0.25, "0.25"),
            (0.1234, "0.1234"),
            (1234.5678, "1234.5677"),
            (0.0025, "0.0025"),
            (-0.0004, "-0.0004"),
            (0.333_333_3, "0.3333333"),
            (1_234_567.8, "1234567.8"),
            (0.000_123_45, "0.00012345"),
            // The notation switches at 1e9 and below 1e-4.
            (1e9, "1E+09"),
            (1e10, "1E+10"),
            (1e14, "1E+14"),
            (9.999_999e14, "9.999999E+14"),
            (0.001, "0.001"),
            (0.0001, "0.0001"),
            (1e-5, "1E-05"),
            (1e-6, "1E-06"),
            (f32::MAX, "3.4028235E+38"),
            (f32::MIN, "-3.4028235E+38"),
            (f32::MIN_POSITIVE, "1.1754944E-38"),
            (f32::from_bits(1), "1E-45"),
        ];
        assert_formats(CASES, managed_float, "Newtonsoft float");
    }

    fn assert_formats(cases: &[(f32, &str)], format: fn(f32) -> String, name: &str) {
        let wrong: Vec<String> = cases
            .iter()
            .filter_map(|(value, expected)| {
                let actual = format(*value);
                (actual != *expected)
                    .then(|| format!("{value:?}: expected {expected}, produced {actual}"))
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "{} of {} {name} case(s) differ:\n{}",
            wrong.len(),
            cases.len(),
            wrong.join("\n")
        );
    }
}
