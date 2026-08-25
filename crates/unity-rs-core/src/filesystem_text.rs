//! Fallible, allocation-aware conversion of platform filesystem strings.
//!
//! `OsStr::to_string_lossy` may allocate the complete replacement-expanded
//! string before a caller can enforce its byte budget. The helpers here expose
//! the same platform replacement semantics as a streaming character walk so a
//! caller can count first and allocate exactly once.

use std::ffi::OsStr;

use crate::{Error, Result};

#[cfg(not(windows))]
fn for_each_utf8_with_replacement(
    mut input: &[u8],
    mut visitor: impl FnMut(char) -> Result<()>,
) -> Result<()> {
    while !input.is_empty() {
        match std::str::from_utf8(input) {
            Ok(valid) => {
                for character in valid.chars() {
                    visitor(character)?;
                }
                return Ok(());
            }
            Err(error) => {
                let valid_length = error.valid_up_to();
                if valid_length != 0 {
                    let valid = std::str::from_utf8(&input[..valid_length]).map_err(|_| {
                        Error::invalid_data("valid UTF-8 prefix could not be decoded")
                    })?;
                    for character in valid.chars() {
                        visitor(character)?;
                    }
                }
                visitor(char::REPLACEMENT_CHARACTER)?;
                let invalid_length = error
                    .error_len()
                    .unwrap_or_else(|| input.len() - valid_length);
                input = &input[valid_length + invalid_length..];
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn for_each_os_str_char_lossy(
    value: &OsStr,
    mut visitor: impl FnMut(char) -> Result<()>,
) -> Result<()> {
    use std::char::decode_utf16;
    use std::os::windows::ffi::OsStrExt;

    for character in decode_utf16(value.encode_wide()) {
        visitor(character.unwrap_or(char::REPLACEMENT_CHARACTER))?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn for_each_os_str_char_lossy(
    value: &OsStr,
    visitor: impl FnMut(char) -> Result<()>,
) -> Result<()> {
    for_each_utf8_with_replacement(value.as_encoded_bytes(), visitor)
}

pub(crate) fn lossy_os_str_utf8_length(value: &OsStr) -> Result<usize> {
    let mut length = 0_usize;
    for_each_os_str_char_lossy(value, |character| {
        length = length
            .checked_add(character.len_utf8())
            .ok_or_else(|| Error::invalid_data("lossy filesystem string length overflowed"))?;
        Ok(())
    })?;
    Ok(length)
}

pub(crate) fn copy_os_str_with_replacement(
    value: &OsStr,
    utf8_length: usize,
    field: &'static str,
) -> Result<String> {
    let mut output = String::new();
    output
        .try_reserve_exact(utf8_length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    for_each_os_str_char_lossy(value, |character| {
        output.push(character);
        Ok(())
    })?;
    if output.len() != utf8_length {
        return Err(Error::invalid_data(format!(
            "{field} changed between validation and allocation"
        )));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{copy_os_str_with_replacement, lossy_os_str_utf8_length};

    #[cfg(unix)]
    #[test]
    fn unix_streaming_conversion_matches_standard_lossy_replacement() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let cases = [
            b"plain ASCII".as_slice(),
            "界".as_bytes(),
            &[0xff],
            &[0xe2, 0x82],
            &[0xe2, 0x28, 0xa1],
            &[0xed, 0xa0, 0x80],
            &[0xc0, 0xaf],
            &[0xf4, 0x8f, 0xbf, 0xbf],
            &[0xf4, 0x90, 0x80, 0x80],
            &[b'a', 0xff, b'b', 0xe2, 0x82],
        ];
        for bytes in cases {
            let value = OsString::from_vec(bytes.to_vec());
            let expected = value.to_string_lossy();
            let length = lossy_os_str_utf8_length(&value).unwrap();
            assert_eq!(length, expected.len(), "{bytes:02x?}");
            assert_eq!(
                copy_os_str_with_replacement(&value, length, "test filesystem string").unwrap(),
                expected,
                "{bytes:02x?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_streaming_conversion_matches_standard_surrogate_replacement() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        let cases: &[&[u16]] = &[
            &[0x0041],
            &[0xd800],
            &[0xdc00],
            &[0xd800, 0xdc00],
            &[0xd83d, 0xde00],
            &[0xd800, 0x0058, 0xdc00],
        ];
        for units in cases {
            let value = OsString::from_wide(units);
            let expected = value.to_string_lossy();
            let length = lossy_os_str_utf8_length(&value).unwrap();
            assert_eq!(length, expected.len(), "{units:04x?}");
            assert_eq!(
                copy_os_str_with_replacement(&value, length, "test filesystem string").unwrap(),
                expected,
                "{units:04x?}"
            );
        }
    }
}
