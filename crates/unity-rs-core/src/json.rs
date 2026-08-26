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
            // Serialize each at its source width, so a `float` field keeps its
            // own shortest round-trip form instead of the double expansion of
            // the widened value.
            TypeValue::Float32(value) if value.is_finite() => {
                serde_json::to_writer(&mut self.output, value).map_err(json_error)?;
            }
            TypeValue::Float(value) if value.is_finite() => {
                serde_json::to_writer(&mut self.output, value).map_err(json_error)?;
            }
            TypeValue::Float32(_) | TypeValue::Float(_) => {
                let (nan, negative) = match value {
                    TypeValue::Float32(value) => (value.is_nan(), value.is_sign_negative()),
                    TypeValue::Float(value) => (value.is_nan(), value.is_sign_negative()),
                    _ => unreachable!("guarded by the surrounding pattern"),
                };
                let label = if nan {
                    "NaN"
                } else if negative {
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
    fn floats_serialize_at_their_source_width() {
        // A serialized `float` widened to f64 has the double expansion as its
        // shortest round-trip form, so keeping the source width is what makes
        // 0.1f read back as 0.1 rather than 0.10000000149011612.
        let value = TypeValue::Object(vec![
            TypeField {
                name: "single".to_owned(),
                value: TypeValue::Float32(0.1),
            },
            TypeField {
                name: "widened".to_owned(),
                value: TypeValue::Float(f64::from(0.1_f32)),
            },
            TypeField {
                name: "double".to_owned(),
                value: TypeValue::Float(0.1),
            },
            TypeField {
                name: "single_nan".to_owned(),
                value: TypeValue::Float32(f32::NAN),
            },
            TypeField {
                name: "single_negative_infinity".to_owned(),
                value: TypeValue::Float32(f32::NEG_INFINITY),
            },
        ]);
        let mut output = Vec::new();
        write_type_value_json(&value, &mut output, false).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"single\":0.1,\
             \"widened\":0.10000000149011612,\
             \"double\":0.1,\
             \"single_nan\":\"NaN\",\
             \"single_negative_infinity\":\"-Infinity\"}"
        );
    }

    /// A node Unity names `string` but shapes as a class reaches JSON as an
    /// ordinary nested object, not a quoted value. This is the surface the asset
    /// exporter consumes, so pin it here rather than only at the `TypeValue`
    /// layer: `ExposedReference<T>` is the shape that made it matter.
    #[test]
    fn writes_a_string_named_class_as_a_nested_object() {
        let value = TypeValue::Object(vec![
            TypeField {
                name: "exposedName".to_owned(),
                value: TypeValue::Object(vec![TypeField {
                    name: "id".to_owned(),
                    value: TypeValue::String("259224778".to_owned()),
                }]),
            },
            TypeField {
                name: "defaultValue".to_owned(),
                value: TypeValue::Object(vec![TypeField {
                    name: "m_PathID".to_owned(),
                    value: TypeValue::Signed(0),
                }]),
            },
        ]);
        let mut output = Vec::new();
        write_type_value_json(&value, &mut output, false).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"exposedName\":{\"id\":\"259224778\"},\
             \"defaultValue\":{\"m_PathID\":0}}"
        );
    }

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

    /// A deterministic xorshift, so a failure names a seed that reproduces it.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut state = self.0;
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            self.0 = state;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, bound: u64) -> usize {
            usize::try_from(self.next() % bound).expect("bounded by a small constant")
        }
    }

    /// What the generator actually produced, so the test can refuse to pass
    /// while claiming coverage it never reached.
    #[derive(Default)]
    struct Seen {
        variants: std::collections::BTreeSet<&'static str>,
        empty_containers: usize,
        non_finite: usize,
        replacement_characters: usize,
        nested_containers: usize,
    }

    const STRINGS: &[&str] = &[
        "",
        "plain",
        "a\nb\tc",
        "quote\"and\\backslash",
        "\u{0}\u{1}\u{1f}",
        "汉字",
        "\u{1f600}",
        "   ",
    ];

    const NAMES: &[&str] = &["m_Name", "value", "a\"b", "\u{0}", "键", ""];

    const FLOATS32: &[f32] = &[
        0.0,
        -0.0,
        0.1,
        1.0,
        -12.5,
        1e-40,
        1e38,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ];

    const FLOATS: &[f64] = &[
        0.0,
        -0.0,
        0.1,
        1.0,
        -12.5,
        1e-40,
        1e38,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];

    const CHARACTERS: &[u16] = &[0x0000, 0x0041, 0x00e9, 0x4e2d, 0xd800, 0xdfff, 0xfffd];

    fn sample_leaf(rng: &mut Rng, seen: &mut Seen) -> TypeValue {
        match rng.below(8) {
            0 => TypeValue::Signed(rng.next().cast_signed()),
            1 => TypeValue::Unsigned(rng.next()),
            2 => {
                let code = CHARACTERS[rng.below(CHARACTERS.len() as u64)];
                if char::from_u32(u32::from(code)).is_none() {
                    seen.replacement_characters += 1;
                }
                TypeValue::Character(code)
            }
            3 => {
                let value = FLOATS32[rng.below(FLOATS32.len() as u64)];
                if !value.is_finite() {
                    seen.non_finite += 1;
                }
                TypeValue::Float32(value)
            }
            4 => {
                let value = FLOATS[rng.below(FLOATS.len() as u64)];
                if !value.is_finite() {
                    seen.non_finite += 1;
                }
                TypeValue::Float(value)
            }
            5 => TypeValue::Boolean(rng.below(2) == 1),
            6 => TypeValue::String(STRINGS[rng.below(STRINGS.len() as u64)].to_owned()),
            _ => TypeValue::TypelessData {
                offset: rng.next(),
                size: rng.next(),
            },
        }
    }

    fn sample(rng: &mut Rng, depth: usize, seen: &mut Seen) -> TypeValue {
        // Containers only above the depth limit, so recursion terminates.
        let value = match if depth == 0 { 0 } else { rng.below(11) } {
            0..=7 => sample_leaf(rng, seen),
            8 => {
                let length = rng.below(4);
                if length == 0 {
                    seen.empty_containers += 1;
                }
                TypeValue::Array(
                    (0..length)
                        .map(|_| sample(rng, depth - 1, seen))
                        .collect::<Vec<_>>(),
                )
            }
            9 => {
                let length = rng.below(4);
                if length == 0 {
                    seen.empty_containers += 1;
                }
                TypeValue::Object(
                    (0..length)
                        .map(|index| TypeField {
                            // Prefixed so no object carries a duplicate key: a
                            // parser keeps only the last of those, which would
                            // make the comparison meaningless rather than
                            // strict.
                            name: format!("{index}{}", NAMES[rng.below(NAMES.len() as u64)]),
                            value: sample(rng, depth - 1, seen),
                        })
                        .collect::<Vec<_>>(),
                )
            }
            _ => {
                let length = rng.below(4);
                if length == 0 {
                    seen.empty_containers += 1;
                }
                TypeValue::Map(
                    (0..length)
                        .map(|_| TypeMapEntry {
                            key: sample(rng, depth - 1, seen),
                            value: sample(rng, depth - 1, seen),
                        })
                        .collect::<Vec<_>>(),
                )
            }
        };
        seen.variants.insert(match &value {
            TypeValue::Signed(_) => "Signed",
            TypeValue::Unsigned(_) => "Unsigned",
            TypeValue::Character(_) => "Character",
            TypeValue::Float32(_) => "Float32",
            TypeValue::Float(_) => "Float",
            TypeValue::Boolean(_) => "Boolean",
            TypeValue::String(_) => "String",
            TypeValue::TypelessData { .. } => "TypelessData",
            TypeValue::Array(values) => {
                if values.iter().any(is_container) {
                    seen.nested_containers += 1;
                }
                "Array"
            }
            TypeValue::Object(fields) => {
                if fields.iter().any(|field| is_container(&field.value)) {
                    seen.nested_containers += 1;
                }
                "Object"
            }
            TypeValue::Map(entries) => {
                if entries
                    .iter()
                    .any(|entry| is_container(&entry.key) || is_container(&entry.value))
                {
                    seen.nested_containers += 1;
                }
                "Map"
            }
        });
        value
    }

    fn is_container(value: &TypeValue) -> bool {
        matches!(
            value,
            TypeValue::Array(_) | TypeValue::Object(_) | TypeValue::Map(_)
        )
    }

    /// The mapping this module documents, expressed against `serde_json`'s
    /// model rather than against the writer's bytes.
    fn expected(value: &TypeValue) -> serde_json::Value {
        use serde_json::Value;

        fn number(value: f64) -> Value {
            Value::Number(serde_json::Number::from_f64(value).expect("finite"))
        }

        fn label(nan: bool, negative: bool) -> Value {
            Value::String(
                if nan {
                    "NaN"
                } else if negative {
                    "-Infinity"
                } else {
                    "Infinity"
                }
                .to_owned(),
            )
        }

        match value {
            TypeValue::Signed(value) => Value::from(*value),
            TypeValue::Unsigned(value) => Value::from(*value),
            TypeValue::Character(value) => Value::String(
                char::from_u32(u32::from(*value))
                    .unwrap_or(char::REPLACEMENT_CHARACTER)
                    .to_string(),
            ),
            // A `float` is written at its source width, so the number in the
            // document is the shortest form that round-trips the f32 -- read
            // back as a double that is 0.1, not f64::from(0.1f32). Going
            // through the shortest decimal is the whole point, so the
            // expectation has to go through it too.
            TypeValue::Float32(value) if value.is_finite() => {
                number(format!("{value}").parse::<f64>().expect("finite"))
            }
            TypeValue::Float(value) if value.is_finite() => number(*value),
            TypeValue::Float32(value) => label(value.is_nan(), value.is_sign_negative()),
            TypeValue::Float(value) => label(value.is_nan(), value.is_sign_negative()),
            TypeValue::Boolean(value) => Value::Bool(*value),
            TypeValue::String(value) => Value::String(value.clone()),
            TypeValue::TypelessData { offset, size } => {
                serde_json::json!({"Offset": offset, "Size": size})
            }
            TypeValue::Array(values) => Value::Array(values.iter().map(expected).collect()),
            TypeValue::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|field| (field.name.clone(), expected(&field.value)))
                    .collect(),
            ),
            TypeValue::Map(entries) => Value::Array(
                entries
                    .iter()
                    .map(|entry| {
                        serde_json::json!({
                            "key": expected(&entry.key),
                            "value": expected(&entry.value),
                        })
                    })
                    .collect(),
            ),
        }
    }

    #[test]
    fn arbitrary_trees_emit_json_a_real_parser_reads_back_unchanged() {
        // The framing here -- braces, commas, separators, indentation -- is
        // written by hand, and the other tests in this module compare it with
        // strings written by hand as well. Those agree by construction. This
        // one hands the bytes to serde_json's parser, which shares nothing
        // with the writer, and compares what comes back against the mapping
        // the module documents. Object key order is not compared, because a
        // parsed object is unordered; the hand-written tests above cover the
        // order.
        let mut rng = Rng(0x5eed_1234_9abc_def1);
        let mut seen = Seen::default();
        for iteration in 0..600 {
            let value = sample(&mut rng, 4, &mut seen);

            let mut compact = Vec::new();
            write_type_value_json(&value, &mut compact, false).unwrap();
            let mut pretty = Vec::new();
            write_type_value_json(&value, &mut pretty, true).unwrap();

            let want = expected(&value);
            let got_compact: serde_json::Value =
                serde_json::from_slice(&compact).unwrap_or_else(|error| {
                    panic!(
                        "iteration {iteration} produced unparseable compact JSON: {error}\n{}",
                        String::from_utf8_lossy(&compact)
                    )
                });
            let got_pretty: serde_json::Value =
                serde_json::from_slice(&pretty).unwrap_or_else(|error| {
                    panic!(
                        "iteration {iteration} produced unparseable pretty JSON: {error}\n{}",
                        String::from_utf8_lossy(&pretty)
                    )
                });

            assert_eq!(got_compact, want, "iteration {iteration} compact");
            assert_eq!(got_pretty, want, "iteration {iteration} pretty");
            assert!(
                !compact.contains(&b'\n'),
                "iteration {iteration} compact output carries a raw newline"
            );
            if is_container(&value) {
                assert!(
                    pretty.ends_with(b"\n"),
                    "iteration {iteration} pretty output does not end with a newline"
                );
            }
        }

        // Everything above is vacuous if the generator never reached these.
        assert_eq!(
            seen.variants.len(),
            11,
            "the generator missed a TypeValue variant: {:?}",
            seen.variants
        );
        assert!(
            seen.empty_containers > 0,
            "no empty container was generated"
        );
        assert!(seen.nested_containers > 0, "no container held another");
        assert!(seen.non_finite > 0, "no NaN or infinity was generated");
        assert!(
            seen.replacement_characters > 0,
            "no unpaired surrogate was generated"
        );
    }
}
