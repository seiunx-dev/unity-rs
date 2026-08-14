use std::io::Write;

use crate::type_tree::{TypeField, TypeMapEntry, TypeValue};
use crate::{Error, Result};

pub fn write_type_value_json<W: Write>(
    value: &TypeValue,
    output: &mut W,
    pretty: bool,
) -> Result<()> {
    let mut writer = JsonWriter {
        output,
        pretty,
        depth: 0,
    };
    writer.write_value(value)?;
    if pretty {
        writer.output.write_all(b"\n")?;
    }
    Ok(())
}

struct JsonWriter<'a, W> {
    output: &'a mut W,
    pretty: bool,
    depth: usize,
}

impl<W: Write> JsonWriter<'_, W> {
    fn write_value(&mut self, value: &TypeValue) -> Result<()> {
        match value {
            TypeValue::Signed(value) => write!(self.output, "{value}")?,
            TypeValue::Unsigned(value) => write!(self.output, "{value}")?,
            TypeValue::Character(value) => {
                let character =
                    char::from_u32(u32::from(*value)).unwrap_or(char::REPLACEMENT_CHARACTER);
                self.write_string(&character.to_string())?;
            }
            TypeValue::Float(value) if value.is_finite() => {
                serde_json::to_writer(&mut self.output, value).map_err(json_error)?;
            }
            TypeValue::Float(value) => {
                let label = if value.is_nan() {
                    "NaN"
                } else if value.is_sign_negative() {
                    "-Infinity"
                } else {
                    "Infinity"
                };
                self.write_string(label)?;
            }
            TypeValue::Boolean(value) => {
                self.output
                    .write_all(if *value { b"true" } else { b"false" })?;
            }
            TypeValue::String(value) => self.write_string(value)?,
            TypeValue::TypelessData { offset, size } => {
                self.output.write_all(b"{")?;
                self.depth += 1;
                self.newline_and_indent()?;
                self.write_string("Offset")?;
                self.write_separator()?;
                write!(self.output, "{offset}")?;
                self.output.write_all(b",")?;
                self.newline_and_indent()?;
                self.write_string("Size")?;
                self.write_separator()?;
                write!(self.output, "{size}")?;
                self.depth -= 1;
                self.newline_and_indent()?;
                self.output.write_all(b"}")?;
            }
            TypeValue::Array(values) => self.write_array(values)?,
            TypeValue::Object(fields) => self.write_object(fields)?,
            TypeValue::Map(entries) => self.write_map(entries)?,
        }
        Ok(())
    }

    fn write_array(&mut self, values: &[TypeValue]) -> Result<()> {
        self.output.write_all(b"[")?;
        if !values.is_empty() {
            self.depth += 1;
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    self.output.write_all(b",")?;
                }
                self.newline_and_indent()?;
                self.write_value(value)?;
            }
            self.depth -= 1;
            self.newline_and_indent()?;
        }
        self.output.write_all(b"]")?;
        Ok(())
    }

    fn write_object(&mut self, fields: &[TypeField]) -> Result<()> {
        self.output.write_all(b"{")?;
        if !fields.is_empty() {
            self.depth += 1;
            for (index, field) in fields.iter().enumerate() {
                if index != 0 {
                    self.output.write_all(b",")?;
                }
                self.newline_and_indent()?;
                self.write_string(&field.name)?;
                self.write_separator()?;
                self.write_value(&field.value)?;
            }
            self.depth -= 1;
            self.newline_and_indent()?;
        }
        self.output.write_all(b"}")?;
        Ok(())
    }

    fn write_map(&mut self, entries: &[TypeMapEntry]) -> Result<()> {
        self.output.write_all(b"[")?;
        if !entries.is_empty() {
            self.depth += 1;
            for (index, entry) in entries.iter().enumerate() {
                if index != 0 {
                    self.output.write_all(b",")?;
                }
                self.newline_and_indent()?;
                self.output.write_all(b"{")?;
                self.depth += 1;
                self.newline_and_indent()?;
                self.write_string("key")?;
                self.write_separator()?;
                self.write_value(&entry.key)?;
                self.output.write_all(b",")?;
                self.newline_and_indent()?;
                self.write_string("value")?;
                self.write_separator()?;
                self.write_value(&entry.value)?;
                self.depth -= 1;
                self.newline_and_indent()?;
                self.output.write_all(b"}")?;
            }
            self.depth -= 1;
            self.newline_and_indent()?;
        }
        self.output.write_all(b"]")?;
        Ok(())
    }

    fn write_string(&mut self, value: &str) -> Result<()> {
        serde_json::to_writer(&mut self.output, value).map_err(json_error)
    }

    fn write_separator(&mut self) -> Result<()> {
        self.output
            .write_all(if self.pretty { b": " } else { b":" })?;
        Ok(())
    }

    fn newline_and_indent(&mut self) -> Result<()> {
        if !self.pretty {
            return Ok(());
        }
        self.output.write_all(b"\n")?;
        for _ in 0..self.depth {
            self.output.write_all(b"  ")?;
        }
        Ok(())
    }
}

fn json_error(error: serde_json::Error) -> Error {
    if error.is_io() {
        Error::Io(error.into())
    } else {
        Error::invalid_data(format!("failed to encode type tree JSON: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::write_type_value_json;
    use crate::type_tree::{TypeField, TypeMapEntry, TypeValue};

    #[test]
    fn writes_ordered_objects_maps_and_special_floats() {
        let value = TypeValue::Object(vec![
            TypeField {
                name: "name".to_owned(),
                value: TypeValue::String("a\nb".to_owned()),
            },
            TypeField {
                name: "number".to_owned(),
                value: TypeValue::Float(f64::INFINITY),
            },
            TypeField {
                name: "map".to_owned(),
                value: TypeValue::Map(vec![TypeMapEntry {
                    key: TypeValue::Signed(1),
                    value: TypeValue::Boolean(true),
                }]),
            },
        ]);
        let mut output = Vec::new();
        write_type_value_json(&value, &mut output, false).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            r#"{"name":"a\nb","number":"Infinity","map":[{"key":1,"value":true}]}"#
        );
    }

    #[test]
    fn writes_typeless_metadata_without_copying_payload() {
        let mut output = Vec::new();
        write_type_value_json(
            &TypeValue::TypelessData {
                offset: 12,
                size: 34,
            },
            &mut output,
            true,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
            serde_json::json!({"Offset": 12, "Size": 34})
        );
    }
}
