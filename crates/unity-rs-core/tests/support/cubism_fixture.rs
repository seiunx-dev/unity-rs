//! Builds a `CubismPhysicsController` fixture with an embedded `TypeTree`.
//!
//! Cubism data is not a Unity built-in class: it is a `MonoBehaviour` whose
//! shape comes from the `Live2D` SDK's own C# types. A reader therefore has
//! nothing to fall back on and must take the layout from the tree the file
//! carries, which is exactly what makes this worth comparing -- the managed
//! implementation and this crate walk the same tree independently and project
//! it into physics3.json through completely separate code.
//!
//! The field names and their order come from the Cubism types the managed
//! repository declares in `CubismUnityClasses/CubismPhysics.cs`, so the fixture
//! is not this crate's idea of the shape.
//!
//! The tree is written in the format 19+ blob encoding, where a node is 32
//! bytes and carries a reference type hash the older 24-byte layout has no room
//! for.

use serde_json::{Value, json};

/// Unity's meta flag for "align the stream to four bytes after this field".
const ALIGN: i32 = 0x4000;

/// Accumulates type tree nodes in serialized (depth-first) order.
pub(crate) struct TreeBuilder {
    nodes: Vec<Value>,
    index: i32,
}

impl TreeBuilder {
    pub(crate) fn new() -> Self {
        Self {
            nodes: Vec::new(),
            index: 0,
        }
    }

    fn push(&mut self, type_name: &str, field: &str, byte_size: i32, level: i32, flags: i32) {
        self.push_node(type_name, field, byte_size, level, 0, flags);
    }

    fn push_node(
        &mut self,
        type_name: &str,
        field: &str,
        byte_size: i32,
        level: i32,
        is_array: i32,
        flags: i32,
    ) {
        self.nodes.push(json!({
            "type": type_name,
            "name": field,
            "byte_size": byte_size,
            "index": self.index,
            "is_array": is_array,
            "version": 1,
            "meta_flags": flags,
            "level": level,
        }));
        self.index += 1;
    }

    fn float(&mut self, field: &str, level: i32) {
        self.push("float", field, 4, level, 0);
    }

    fn int(&mut self, field: &str, level: i32) {
        self.push("int", field, 4, level, 0);
    }

    /// A `bool` is one byte and Unity aligns after it.
    fn bool(&mut self, field: &str, level: i32) {
        self.push("bool", field, 1, level, ALIGN);
    }

    /// `m_Enabled` is the one byte-sized field Unity types as `UInt8` rather
    /// than `bool`. The managed reader deserializes the whole behaviour
    /// dictionary through the Cubism types, where `MonoBehaviour.m_Enabled` is
    /// numeric, so a `bool` here makes the conversion throw rather than differ.
    fn byte(&mut self, field: &str, level: i32) {
        self.push("UInt8", field, 1, level, ALIGN);
    }

    /// Unity writes a string as a character array with a leading count, and
    /// aligns after the characters rather than after each one.
    fn string(&mut self, field: &str, level: i32) {
        self.push("string", field, -1, level, ALIGN);
        self.push_node("Array", "Array", -1, level + 1, 1, ALIGN);
        self.int("size", level + 2);
        self.push("char", "data", 1, level + 2, 0);
    }

    fn vector2(&mut self, field: &str, level: i32) {
        self.push("Vector2f", field, 8, level, 0);
        self.float("x", level + 1);
        self.float("y", level + 1);
    }

    /// Opens a `T[] field`, which Unity models as a `vector` wrapping an
    /// `Array` whose `data` node is the element. The caller writes the
    /// element's own children at `level + 3`.
    fn open_vector(&mut self, element: &str, field: &str, level: i32) {
        self.push("vector", field, -1, level, 0);
        self.push_node("Array", "Array", -1, level + 1, 1, ALIGN);
        self.int("size", level + 2);
        self.push(element, "data", -1, level + 2, 0);
    }

    /// A `float[] field`, whose element needs no children of its own.
    fn float_vector(&mut self, field: &str, level: i32) {
        self.push("vector", field, -1, level, 0);
        self.push_node("Array", "Array", -1, level + 1, 1, ALIGN);
        self.int("size", level + 2);
        self.float("data", level + 2);
    }

    /// A `string[] field`.
    fn string_vector(&mut self, field: &str, level: i32) {
        self.push("vector", field, -1, level, 0);
        self.push_node("Array", "Array", -1, level + 1, 1, ALIGN);
        self.int("size", level + 2);
        self.string("data", level + 2);
    }

    pub(crate) fn finish(self) -> Vec<Value> {
        self.nodes
    }
}

/// The tree for a `CubismPhysicsController` behaviour.
pub(crate) fn cubism_physics_tree() -> Vec<Value> {
    let mut tree = TreeBuilder::new();
    tree.push("MonoBehaviour", "Base", -1, 0, 0);
    pptr(&mut tree, "m_GameObject", "GameObject", 1);
    tree.byte("m_Enabled", 1);
    pptr(&mut tree, "m_Script", "MonoScript", 1);
    tree.string("m_Name", 1);

    tree.push("CubismPhysicsRig", "_rig", -1, 1, 0);
    tree.open_vector("CubismPhysicsSubRig", "SubRigs", 2);
    // Element children sit one level below the `data` node.
    let element = 5;
    sub_rig(&mut tree, element);
    tree.vector2("Gravity", 2);
    tree.vector2("Wind", 2);
    tree.float("Fps", 2);
    tree.finish()
}

fn pptr(tree: &mut TreeBuilder, field: &str, target: &str, level: i32) {
    tree.push(&format!("PPtr<{target}>"), field, 12, level, 0);
    tree.int("m_FileID", level + 1);
    tree.push("SInt64", "m_PathID", 8, level + 1, 0);
}

fn sub_rig(tree: &mut TreeBuilder, level: i32) {
    tree.open_vector("CubismPhysicsInput", "Input", level);
    let input = level + 3;
    tree.string("SourceId", input);
    tree.vector2("ScaleOfTranslation", input);
    tree.float("AngleScale", input);
    tree.float("Weight", input);
    tree.int("SourceComponent", input);
    tree.bool("IsInverted", input);

    tree.open_vector("CubismPhysicsOutput", "Output", level);
    let output = level + 3;
    tree.string("DestinationId", output);
    tree.int("ParticleIndex", output);
    tree.vector2("TranslationScale", output);
    tree.float("AngleScale", output);
    tree.float("Weight", output);
    tree.int("SourceComponent", output);
    tree.bool("IsInverted", output);

    tree.open_vector("CubismPhysicsParticle", "Particles", level);
    let particle = level + 3;
    tree.vector2("InitialPosition", particle);
    tree.float("Mobility", particle);
    tree.float("Delay", particle);
    tree.float("Acceleration", particle);
    tree.float("Radius", particle);

    tree.push("CubismPhysicsNormalization", "Normalization", -1, level, 0);
    for component in ["Position", "Angle"] {
        tree.push(
            "CubismPhysicsNormalizationTuplet",
            component,
            12,
            level + 1,
            0,
        );
        tree.float("Maximum", level + 2);
        tree.float("Minimum", level + 2);
        tree.float("Default", level + 2);
    }
}

/// Writes the bytes the tree above describes.
///
/// Deliberately hand-written against the tree rather than driven from it: a
/// writer that walked the same node list would agree with any mistake in it,
/// where two independent passes disagree loudly and the managed reader is the
/// one that decides which is right.
pub(crate) fn cubism_physics_object(name: &str) -> Vec<u8> {
    let mut data = Vec::new();
    push_pptr(&mut data);
    data.push(1); // m_Enabled
    align(&mut data);
    push_pptr(&mut data);
    push_string(&mut data, name);

    // Two sub-rigs, so the projection's per-rig numbering is exercised.
    push_i32(&mut data, 2);
    push_sub_rig(&mut data, 0);
    push_sub_rig(&mut data, 1);
    push_vector2(&mut data, 0.0, -1.0); // Gravity
    push_vector2(&mut data, 0.5, 0.25); // Wind
    push_f32(&mut data, 30.0); // Fps
    data
}

fn push_sub_rig(data: &mut Vec<u8>, rig: u8) {
    let offset = f32::from(rig);

    push_i32(data, 2); // Input
    for input in 0..2_i32 {
        push_string(
            data,
            &format!("ParamAngle{}", if input == 0 { "X" } else { "Y" }),
        );
        push_vector2(data, 1.0 + offset, 2.0 + offset);
        push_f32(data, 0.5 + offset);
        // Values that only agree if the .NET "0.###" format is reproduced:
        // more than three decimals, and a half that rounds away from zero.
        push_f32(data, 1.234_567_8);
        // The three source components are X, Y and Angle; using a different
        // one per input keeps the enum mapping under test.
        push_i32(data, input);
        data.push(u8::from(input == 1));
        align(data);
    }

    push_i32(data, 1); // Output
    push_string(data, "ParamHairFront");
    push_i32(data, 1);
    push_vector2(data, 3.0 + offset, 4.0 + offset);
    push_f32(data, 0.002_5);
    push_f32(data, 100.0);
    push_i32(data, 2); // Angle
    data.push(1);
    align(data);

    push_i32(data, 2); // Particles
    for particle in 0..2_u8 {
        let step = f32::from(particle);
        push_vector2(data, step, 5.0 + step + offset);
        push_f32(data, 0.000_49);
        push_f32(data, 0.8 + step);
        push_f32(data, -0.000_4);
        // Eight significant digits, which is one more than a float carries.
        // "0.###" rounds to the float's seven first and only then to three
        // decimals, so this prints 1234.568; rounding straight to three
        // decimals, or to six digits first, gives something else. Nothing else
        // in these fixtures is long enough to tell those apart.
        push_f32(data, 1_234.567_8 + step);
    }

    // Normalization: Position then Angle, each Maximum/Minimum/Default.
    push_f32(data, 10.0);
    push_f32(data, -10.0);
    push_f32(data, 0.0);
    push_f32(data, 90.0);
    push_f32(data, -90.0);
    push_f32(data, 5.0 + offset);
}

fn push_pptr(data: &mut Vec<u8>) {
    push_i32(data, 0);
    data.extend_from_slice(&0_i64.to_le_bytes());
}

fn push_string(data: &mut Vec<u8>, value: &str) {
    push_i32(data, i32::try_from(value.len()).unwrap());
    data.extend_from_slice(value.as_bytes());
    align(data);
}

fn push_vector2(data: &mut Vec<u8>, x: f32, y: f32) {
    push_f32(data, x);
    push_f32(data, y);
}

fn push_f32(data: &mut Vec<u8>, value: f32) {
    data.extend_from_slice(&value.to_le_bytes());
}

fn push_i32(data: &mut Vec<u8>, value: i32) {
    data.extend_from_slice(&value.to_le_bytes());
}

fn align(data: &mut Vec<u8>) {
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
}

/// The tree for a `CubismFadeMotionData` behaviour.
///
/// The field names and order come from the managed
/// `CubismUnityClasses/CubismFadeMotionData.cs`, and the curve shape from
/// Unity's own `AnimationCurve`/`Keyframe`: this is the layout the SDK's
/// serializer writes, not a shape invented here.
pub(crate) fn cubism_fade_motion_tree() -> Vec<Value> {
    let mut tree = TreeBuilder::new();
    tree.push("MonoBehaviour", "Base", -1, 0, 0);
    pptr(&mut tree, "m_GameObject", "GameObject", 1);
    tree.byte("m_Enabled", 1);
    pptr(&mut tree, "m_Script", "MonoScript", 1);
    tree.string("m_Name", 1);

    tree.string("MotionName", 1);
    tree.float("FadeInTime", 1);
    tree.float("FadeOutTime", 1);
    tree.string_vector("ParameterIds", 1);

    tree.open_vector("AnimationCurve", "ParameterCurves", 1);
    let curve = 4;
    tree.open_vector("Keyframe", "m_Curve", curve);
    let key = curve + 3;
    tree.float("time", key);
    tree.float("value", key);
    tree.float("inSlope", key);
    tree.float("outSlope", key);
    tree.int("weightedMode", key);
    tree.float("inWeight", key);
    tree.float("outWeight", key);
    tree.int("m_PreInfinity", curve);
    tree.int("m_PostInfinity", curve);
    tree.int("m_RotationOrder", curve);

    tree.float_vector("ParameterFadeInTimes", 1);
    tree.float_vector("ParameterFadeOutTimes", 1);
    tree.float("MotionLength", 1);
    tree.finish()
}

/// Writes the bytes `cubism_fade_motion_tree` describes.
pub(crate) fn cubism_fade_motion_object(name: &str) -> Vec<u8> {
    let mut data = Vec::new();
    push_pptr(&mut data);
    data.push(1);
    align(&mut data);
    push_pptr(&mut data);
    push_string(&mut data, name);

    push_string(&mut data, "Idle");
    push_f32(&mut data, 0.5);
    push_f32(&mut data, 1.234_567_8);

    // One bound parameter, one bound part and one of the reserved names, so
    // all three target branches of the converter are reached.
    let parameters = ["ParamAngleX", "PartArmA", "Opacity"];
    push_i32(&mut data, i32::try_from(parameters.len()).unwrap());
    for parameter in parameters {
        push_string(&mut data, parameter);
    }

    push_i32(&mut data, i32::try_from(parameters.len()).unwrap());
    for (index, _) in parameters.iter().enumerate() {
        // Three keys, so the converter has both an opening segment and an
        // interior one to emit.
        push_i32(&mut data, 3);
        for key in 0..3_u8 {
            let step = f32::from(key);
            push_f32(&mut data, step * 0.5);
            push_f32(&mut data, if index == 0 { step * 0.25 } else { 0.002_5 });
            push_f32(&mut data, step * 1.5);
            push_f32(&mut data, step * -1.5);
            push_i32(&mut data, 0); // weightedMode
            push_f32(&mut data, 0.333_333_3);
            push_f32(&mut data, 0.333_333_3);
        }
        push_i32(&mut data, 0); // m_PreInfinity
        push_i32(&mut data, 0); // m_PostInfinity
        push_i32(&mut data, 0); // m_RotationOrder
    }

    push_i32(&mut data, i32::try_from(parameters.len()).unwrap());
    push_f32(&mut data, 0.000_49);
    push_f32(&mut data, -0.000_4);
    push_f32(&mut data, 0.5);
    push_i32(&mut data, i32::try_from(parameters.len()).unwrap());
    push_f32(&mut data, 0.25);
    push_f32(&mut data, 2.0);
    push_f32(&mut data, 0.75);
    push_f32(&mut data, 1.0);
    data
}

/// The tree for a `CubismExpressionData` behaviour.
///
/// Field names and order follow the managed
/// `CubismUnityClasses/CubismExpressionData.cs`.
pub(crate) fn cubism_expression_tree() -> Vec<Value> {
    let mut tree = TreeBuilder::new();
    tree.push("MonoBehaviour", "Base", -1, 0, 0);
    pptr(&mut tree, "m_GameObject", "GameObject", 1);
    tree.byte("m_Enabled", 1);
    pptr(&mut tree, "m_Script", "MonoScript", 1);
    tree.string("m_Name", 1);

    tree.string("Type", 1);
    tree.float("FadeInTime", 1);
    tree.float("FadeOutTime", 1);
    tree.open_vector("SerializableExpressionParameter", "Parameters", 1);
    let parameter = 4;
    tree.string("Id", parameter);
    tree.float("Value", parameter);
    tree.int("Blend", parameter);
    tree.finish()
}

/// Writes the bytes `cubism_expression_tree` describes.
pub(crate) fn cubism_expression_object(name: &str) -> Vec<u8> {
    let mut data = Vec::new();
    push_pptr(&mut data);
    data.push(1);
    align(&mut data);
    push_pptr(&mut data);
    push_string(&mut data, name);

    push_string(&mut data, "Live2D Expression");
    push_f32(&mut data, 1.0);
    push_f32(&mut data, 1.234_567_8);

    // One parameter per blend mode, with values that only print the same on
    // both sides if the number format matches.
    let parameters: [(&str, f32, i32); 5] = [
        ("ParamAngleX", 0.8, 0),
        ("ParamAngleY", -0.000_4, 1),
        ("ParamMouthOpenY", 2.0, 2),
        // Zero and a value past the point where the default float format
        // switches to scientific notation. This document's numbers go through
        // Newtonsoft's default float rather than "0.###", and these are the
        // two values that show it: zero prints as 0.0 rather than 0, and
        // 1.5e8 prints in full where one decade further would not.
        ("ParamZero", 0.0, 0),
        ("ParamLarge", 1.5e8, 0),
    ];
    push_i32(&mut data, i32::try_from(parameters.len()).unwrap());
    for (id, value, blend) in parameters {
        push_string(&mut data, id);
        push_f32(&mut data, value);
        push_i32(&mut data, blend);
    }
    data
}

/// Builds a `CubismMoc` behaviour whose payload is a synthetic MOC3 header.
///
/// The MOC behaviour is read without a `TypeTree` on both sides: the managed
/// reader skips a fixed 28 bytes for the two pointers and the enabled flag,
/// then the name, then a length-prefixed byte array. Only the header tables
/// matter, and their offsets are what both implementations key on -- 64 for the
/// count table, 68 for the canvas block, 76 and 264 for the two identifier
/// tables, which are the fixed positions the format defines.
pub(crate) fn cubism_moc_object(name: &str, sdk_version: u8) -> Vec<u8> {
    const IDENTIFIER: usize = 64;
    // Far enough past the 268-byte header for the tables to have room.
    const COUNT_TABLE: usize = 0x120;
    const CANVAS_INFO: usize = 0x140;
    const PART_IDS: usize = 0x180;

    let parts = ["PartArmA", "PartArmB", "PartCore"];
    let parameters = ["ParamAngleX", "ParamAngleY"];
    let parameter_ids = PART_IDS + parts.len() * IDENTIFIER;

    let mut moc = vec![0_u8; parameter_ids + parameters.len() * IDENTIFIER];
    moc[..4].copy_from_slice(b"MOC3");
    moc[4] = sdk_version;
    moc[5] = 0; // little-endian
    let put_u32 = |moc: &mut Vec<u8>, offset: usize, value: u32| {
        moc[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    put_u32(&mut moc, 64, u32::try_from(COUNT_TABLE).unwrap());
    put_u32(&mut moc, 68, u32::try_from(CANVAS_INFO).unwrap());
    put_u32(&mut moc, 76, u32::try_from(PART_IDS).unwrap());
    put_u32(&mut moc, 264, u32::try_from(parameter_ids).unwrap());
    put_u32(&mut moc, COUNT_TABLE, u32::try_from(parts.len()).unwrap());
    put_u32(
        &mut moc,
        COUNT_TABLE + 20,
        u32::try_from(parameters.len()).unwrap(),
    );

    // Pixels per unit, then the centre, then the canvas size. Values with more
    // than three decimals, so a document that rounds them shows it.
    for (index, value) in [1234.5678_f32, -0.000_4, 0.002_5, 1920.0, 1080.5]
        .into_iter()
        .enumerate()
    {
        let at = CANVAS_INFO + index * 4;
        moc[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    for (index, identifier) in parts.iter().chain(parameters.iter()).enumerate() {
        let at = if index < parts.len() {
            PART_IDS + index * IDENTIFIER
        } else {
            parameter_ids + (index - parts.len()) * IDENTIFIER
        };
        moc[at..at + identifier.len()].copy_from_slice(identifier.as_bytes());
    }

    let mut data = Vec::new();
    push_pptr(&mut data);
    data.push(1);
    align(&mut data);
    push_pptr(&mut data);
    push_string(&mut data, name);
    push_i32(&mut data, i32::try_from(moc.len()).unwrap());
    data.extend_from_slice(&moc);
    align(&mut data);
    data
}

/// A `MonoScript` naming the Cubism class a behaviour belongs to.
///
/// The managed extractor classifies `Live2D` behaviours by this name rather
/// by their contents, so a fixture that leaves it out reaches none of that
/// code.
pub(crate) fn mono_script(class_name: &str) -> Vec<u8> {
    let mut data = Vec::new();
    push_string(&mut data, class_name);
    push_i32(&mut data, 0); // m_ExecutionOrder
    data.extend_from_slice(&[0x55; 16]); // m_PropertiesHash
    push_string(&mut data, class_name);
    push_string(&mut data, "Live2D.Cubism.Core");
    push_string(&mut data, "Live2D.Cubism.dll");
    data
}

/// The same MOC behaviour, with its script pointer aimed at `script_path_id`.
pub(crate) fn cubism_moc_object_with_script(
    name: &str,
    sdk_version: u8,
    script_path_id: i64,
) -> Vec<u8> {
    let mut data = cubism_moc_object(name, sdk_version);
    // The script pointer is the second one, after the game object pointer and
    // the aligned enabled flag.
    data[16..20].copy_from_slice(&0_i32.to_le_bytes());
    data[20..28].copy_from_slice(&script_path_id.to_le_bytes());
    data
}

/// A `GameObject` naming a node and listing its components.
pub(crate) fn game_object(name: &str, components: &[i64]) -> Vec<u8> {
    let mut data = Vec::new();
    push_i32(&mut data, i32::try_from(components.len()).unwrap());
    for path_id in components {
        push_i32(&mut data, 0);
        data.extend_from_slice(&path_id.to_le_bytes());
    }
    push_i32(&mut data, 0); // m_Layer
    push_string(&mut data, name);
    data
}

/// A `Transform` linking a node to its parent and children.
pub(crate) fn transform(game_object: i64, children: &[i64], father: i64) -> Vec<u8> {
    let mut data = Vec::new();
    push_pptr_to(&mut data, game_object);
    // Rotation, position, scale: identity.
    for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0] {
        data.extend_from_slice(&value.to_le_bytes());
    }
    push_i32(&mut data, i32::try_from(children.len()).unwrap());
    for path_id in children {
        push_pptr_to(&mut data, *path_id);
    }
    push_pptr_to(&mut data, father);
    data
}

fn push_pptr_to(data: &mut Vec<u8>, path_id: i64) {
    push_i32(data, 0);
    data.extend_from_slice(&path_id.to_le_bytes());
}

/// The prefix every `MonoBehaviour` shares: owner, enabled flag, script, name.
fn behaviour_prefix(game_object: i64, script: i64, name: &str) -> Vec<u8> {
    let mut data = Vec::new();
    push_pptr_to(&mut data, game_object);
    data.push(1);
    align(&mut data);
    push_pptr_to(&mut data, script);
    push_string(&mut data, name);
    data
}

/// The tree for a behaviour whose only field is one object pointer.
pub(crate) fn pointer_behaviour_tree(target: &str, field: &str) -> Vec<Value> {
    let mut tree = TreeBuilder::new();
    tree.push("MonoBehaviour", "Base", -1, 0, 0);
    pptr(&mut tree, "m_GameObject", "GameObject", 1);
    tree.byte("m_Enabled", 1);
    pptr(&mut tree, "m_Script", "MonoScript", 1);
    tree.string("m_Name", 1);
    pptr(&mut tree, field, target, 1);
    tree.finish()
}

/// A behaviour whose only field is one object pointer, such as the
/// `CubismModel` that names its MOC.
pub(crate) fn pointer_behaviour(game_object: i64, script: i64, name: &str, target: i64) -> Vec<u8> {
    let mut data = behaviour_prefix(game_object, script, name);
    push_pptr_to(&mut data, target);
    data
}

/// The tree for a `CubismPosePart`.
///
/// Field names come from the managed extractor, which reads `GroupIndex` and
/// `Link` out of the parsed behaviour.
pub(crate) fn pose_part_tree() -> Vec<Value> {
    let mut tree = TreeBuilder::new();
    tree.push("MonoBehaviour", "Base", -1, 0, 0);
    pptr(&mut tree, "m_GameObject", "GameObject", 1);
    tree.byte("m_Enabled", 1);
    pptr(&mut tree, "m_Script", "MonoScript", 1);
    tree.string("m_Name", 1);
    tree.int("GroupIndex", 1);
    tree.string_vector("Link", 1);
    tree.finish()
}

/// A `CubismPosePart` behaviour.
pub(crate) fn pose_part(
    game_object: i64,
    script: i64,
    group_index: i32,
    links: &[&str],
) -> Vec<u8> {
    let mut data = behaviour_prefix(game_object, script, "");
    push_i32(&mut data, group_index);
    push_i32(&mut data, i32::try_from(links.len()).unwrap());
    for link in links {
        push_string(&mut data, link);
    }
    data
}

/// The tree for a display-info behaviour, which carries a name and an optional
/// override.
pub(crate) fn display_info_tree() -> Vec<Value> {
    let mut tree = TreeBuilder::new();
    tree.push("MonoBehaviour", "Base", -1, 0, 0);
    pptr(&mut tree, "m_GameObject", "GameObject", 1);
    tree.byte("m_Enabled", 1);
    pptr(&mut tree, "m_Script", "MonoScript", 1);
    tree.string("m_Name", 1);
    tree.string("Name", 1);
    tree.string("DisplayName", 1);
    tree.finish()
}

/// A display-info behaviour.
pub(crate) fn display_info(
    game_object: i64,
    script: i64,
    name: &str,
    display_name: &str,
) -> Vec<u8> {
    let mut data = behaviour_prefix(game_object, script, "");
    push_string(&mut data, name);
    push_string(&mut data, display_name);
    data
}

/// The MOC behaviour, owned by a game object and naming its script.
pub(crate) fn cubism_moc_behaviour(game_object: i64, script: i64, sdk_version: u8) -> Vec<u8> {
    let mut data = cubism_moc_object_with_script("moc", sdk_version, script);
    data[0..4].copy_from_slice(&0_i32.to_le_bytes());
    data[4..12].copy_from_slice(&game_object.to_le_bytes());
    data
}
