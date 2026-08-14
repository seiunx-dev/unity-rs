//! Systematic malformed-input sweep.
//!
//! The library's stated contract for untrusted input is that it reports an
//! error rather than crashing. Individual modules each have a rejection test or
//! two, but those check the cases someone thought of; a reader is broken by the
//! cases nobody did.
//!
//! This takes valid inputs and damages them in bulk -- single-byte flips,
//! truncations, and oversized length fields -- then requires every result to be
//! either a successful parse or an `Err`. A panic fails the test and names the
//! exact mutation, so a failure is reproducible rather than a hint.
//!
//! What it does not cover: a mutation that makes the reader loop forever or
//! allocate without bound would hang rather than fail here. Bounded allocation
//! is enforced by the read limits and checked separately.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use assetstudio_core::audio::{detect_direct_wav, write_direct_wav};
use assetstudio_core::cubism_moc::{CubismMocReadLimits, try_read_cubism_moc};
use assetstudio_core::source::Region;
use assetstudio_core::studio::Studio;
use assetstudio_core::texture::TextureReadLimits;

#[path = "support/containers.rs"]
mod containers;

use containers::{BlocksInfo, BundleEntry, BundleLayout, Compression};

/// A small deterministic pseudorandom source.
///
/// Seeded rather than random so a failure reproduces exactly; the values only
/// need to spread the mutations around, not to be statistically good.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // SplitMix64.
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        usize::try_from(self.next() % bound as u64).expect("a bounded index fits")
    }
}

/// Reads everything a caller can reach from a file, ignoring errors.
///
/// The point is to run the code, not to check what it produces: any `Err` is a
/// pass. Only a panic is a failure.
fn exercise(bytes: &[u8]) -> bool {
    let Ok(studio) = Studio::open_region("malformed", Region::from_bytes(bytes.to_vec())) else {
        return false;
    };
    for object in studio.objects() {
        let _ = object.name();
        let _ = object.read_raw(1 << 20);
    }
    for file in studio.files() {
        let _ = file.unity_version();
    }
    for resource in studio.resources() {
        let mut sink = Vec::new();
        let _ = resource.write(&mut sink);
    }
    true
}

/// The result of feeding one damaged input to the reader.
struct Outcome {
    panicked: bool,
    parsed: bool,
}

/// Runs `exercise` with the panic hook silenced.
fn feed(bytes: &[u8]) -> Outcome {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = catch_unwind(AssertUnwindSafe(|| exercise(bytes)));
    std::panic::set_hook(previous);
    Outcome {
        panicked: result.is_err(),
        parsed: result.unwrap_or(false),
    }
}

fn panics(bytes: &[u8]) -> bool {
    feed(bytes).panicked
}

/// The inputs to damage: a bare serialized file and the same file inside the
/// containers a game ships, so the sweep covers header dispatch, block tables
/// and compression as well as the object table.
fn seeds() -> Vec<(&'static str, Vec<u8>)> {
    const REVISION: &str = "2022.3.62f1";
    let asset = text_asset_file();
    let mut seeds = vec![("assets", asset.clone())];
    for (name, layout) in [
        ("unityfs-plain", BundleLayout::v6(REVISION)),
        (
            "unityfs-lz4",
            BundleLayout {
                blocks: Compression::Lz4,
                directory: Compression::Lz4,
                ..BundleLayout::v6(REVISION)
            },
        ),
        (
            "unityfs-at-end",
            BundleLayout {
                info: BlocksInfo::AtEnd,
                ..BundleLayout::v6(REVISION)
            },
        ),
    ] {
        let entry = BundleEntry {
            path: "asset.assets",
            bytes: &asset,
        };
        seeds.push((name, containers::unity_fs(&layout, &[entry])));
    }
    seeds
}

/// A minimal valid v22 file with one `TextAsset`.
fn text_asset_file() -> Vec<u8> {
    let mut object = Vec::new();
    push_aligned_string(&mut object, "malformed");
    push_aligned_string(&mut object, "payload bytes");
    single_object_file(49, &object)
}

/// Wraps one object of `class_id` in a v22 serialized file.
fn single_object_file(class_id: i32, object: &[u8]) -> Vec<u8> {
    objects_file(&[(class_id, 7, object.to_vec())])
}

/// Wraps several objects in a v22 serialized file.
fn objects_file(objects: &[(i32, i64, Vec<u8>)]) -> Vec<u8> {
    let mut classes: Vec<i32> = Vec::new();
    for (class_id, _, _) in objects {
        if !classes.contains(class_id) {
            classes.push(*class_id);
        }
    }
    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"2022.3.62f1\0");
    metadata.extend_from_slice(&13_i32.to_le_bytes());
    metadata.push(0);
    metadata.extend_from_slice(&i32::try_from(classes.len()).unwrap().to_le_bytes());
    for class_id in &classes {
        metadata.extend_from_slice(&class_id.to_le_bytes());
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        // A MonoBehaviour record carries a script hash before the type hash.
        if *class_id == 114 {
            metadata.extend_from_slice(&[0; 16]);
        }
        metadata.extend_from_slice(&[0; 16]);
    }

    let mut data = Vec::new();
    let mut records = Vec::new();
    for (class_id, path_id, payload) in objects {
        while !data.len().is_multiple_of(4) {
            data.push(0);
        }
        records.push((
            *path_id,
            i64::try_from(data.len()).unwrap(),
            u32::try_from(payload.len()).unwrap(),
            i32::try_from(classes.iter().position(|value| value == class_id).unwrap()).unwrap(),
        ));
        data.extend_from_slice(payload);
    }
    metadata.extend_from_slice(&i32::try_from(records.len()).unwrap().to_le_bytes());
    for (path_id, offset, size, type_index) in records {
        while !(48 + metadata.len()).is_multiple_of(4) {
            metadata.push(0);
        }
        metadata.extend_from_slice(&path_id.to_le_bytes());
        metadata.extend_from_slice(&offset.to_le_bytes());
        metadata.extend_from_slice(&size.to_le_bytes());
        metadata.extend_from_slice(&type_index.to_le_bytes());
    }
    for _ in 0..3 {
        metadata.extend_from_slice(&0_i32.to_le_bytes());
    }
    metadata.push(0);

    let data_offset = (48 + metadata.len()).next_multiple_of(16);
    let file_size = data_offset + data.len();
    let mut output = vec![0_u8; 48];
    output[8..12].copy_from_slice(&22_u32.to_be_bytes());
    output[20..24].copy_from_slice(&u32::try_from(metadata.len()).unwrap().to_be_bytes());
    output[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
    output[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
    output.extend_from_slice(&metadata);
    output.resize(data_offset, 0);
    output.extend_from_slice(&data);
    output
}

#[test]
fn single_byte_flips_never_panic() {
    const MUTATIONS: usize = 3_000;

    let mut rng = Rng(0x5EED_0001);
    for (name, seed) in seeds() {
        // The seed itself must parse, or the sweep would be damaging something
        // that was already broken and every result would be a vacuous Err.
        assert!(
            Studio::open_region(name, Region::from_bytes(seed.clone())).is_ok(),
            "the {name} seed does not parse, so mutating it proves nothing"
        );
        let mut still_parsed = 0_usize;
        for _ in 0..MUTATIONS {
            let mut damaged = seed.clone();
            let offset = rng.below(damaged.len());
            damaged[offset] ^= 1 << (rng.next() % 8);
            let outcome = feed(&damaged);
            assert!(
                !outcome.panicked,
                "{name}: flipping a bit at offset {offset} panicked"
            );
            still_parsed += usize::from(outcome.parsed);
        }
        // A sweep where every mutation is rejected at the header would never
        // reach the object table, and would pass while testing almost nothing.
        assert!(
            still_parsed * 5 >= MUTATIONS,
            "{name}: only {still_parsed} of {MUTATIONS} damaged inputs still \
             parsed, so the sweep barely got past the header"
        );
    }
}

#[test]
fn truncations_never_panic() {
    let mut rng = Rng(0x5EED_0002);
    for (name, seed) in seeds() {
        // Every length from empty to whole, plus a spread of interior cuts:
        // the short ones catch header reads, the long ones catch a table that
        // points past the end.
        for length in 0..seed.len().min(96) {
            assert!(
                !panics(&seed[..length]),
                "{name}: truncating to {length} bytes panicked"
            );
        }
        for _ in 0..500 {
            let length = rng.below(seed.len());
            assert!(
                !panics(&seed[..length]),
                "{name}: truncating to {length} bytes panicked"
            );
        }
    }
}

#[test]
fn oversized_lengths_never_panic() {
    // The values a length field takes when it is wrong in the ways that matter:
    // huge, negative when read as signed, and the boundaries either side.
    const POISON: &[u32] = &[
        u32::MAX,
        u32::MAX - 1,
        0x8000_0000,
        0x7FFF_FFFF,
        0x4000_0000,
        0x0100_0000,
    ];

    let mut rng = Rng(0x5EED_0003);

    for (name, seed) in seeds() {
        for _ in 0..2_000 {
            let mut damaged = seed.clone();
            if damaged.len() < 8 {
                continue;
            }
            let offset = rng.below(damaged.len() - 4);
            let poison = POISON[rng.below(POISON.len())];
            damaged[offset..offset + 4].copy_from_slice(&poison.to_le_bytes());
            assert!(
                !panics(&damaged),
                "{name}: writing {poison:#x} at offset {offset} panicked"
            );
        }
    }
}

/// A 2022.3 `Texture2D` wrapping an arbitrary payload.
///
/// Written here rather than shared with the differential because this needs
/// only one revision, and the point is to get damaged bytes as far as the
/// decoder rather than to compare anything.
fn texture2d_object(name: &str, width: i32, height: i32, format: i32, data: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    push_aligned_string(&mut output, name);
    output.extend_from_slice(&0_i32.to_le_bytes()); // forced fallback format
    output.push(0); // downscale fallback
    output.push(0); // alpha channel optional
    pad(&mut output);
    output.extend_from_slice(&width.to_le_bytes());
    output.extend_from_slice(&height.to_le_bytes());
    output.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    output.extend_from_slice(&0_i32.to_le_bytes()); // mips stripped
    output.extend_from_slice(&format.to_le_bytes());
    output.extend_from_slice(&1_i32.to_le_bytes()); // mip count
    output.push(1); // readable
    output.push(0); // preprocessed
    output.push(0); // ignore mip limit
    pad(&mut output);
    push_aligned_string(&mut output, ""); // mip limit group
    output.push(0); // streaming mipmaps
    pad(&mut output);
    output.extend_from_slice(&0_i32.to_le_bytes()); // streaming priority
    output.extend_from_slice(&1_i32.to_le_bytes()); // image count
    output.extend_from_slice(&2_i32.to_le_bytes()); // dimension
    output.extend_from_slice(&[0; 24]); // GL texture settings
    output.extend_from_slice(&0_i32.to_le_bytes()); // lightmap format
    output.extend_from_slice(&0_i32.to_le_bytes()); // colour space
    output.extend_from_slice(&0_i32.to_le_bytes()); // platform blob
    pad(&mut output);
    output.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    output.extend_from_slice(data);
    output.extend_from_slice(&0_i64.to_le_bytes()); // stream offset
    output.extend_from_slice(&0_u32.to_le_bytes()); // stream size
    push_aligned_string(&mut output, ""); // stream path
    output
}

fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
    output.extend_from_slice(&u32::try_from(value.len()).unwrap().to_le_bytes());
    output.extend_from_slice(value.as_bytes());
    pad(output);
}

fn pad(output: &mut Vec<u8>) {
    while !output.len().is_multiple_of(4) {
        output.push(0);
    }
}

/// Damages real compressed payloads inside a texture and decodes them.
///
/// This is where the panic guard around the external decoders earns its keep:
/// `texture2ddecoder`'s ASTC path is known to panic on malformed input, and the
/// contract is that a caller sees an error instead. Feeding the payloads in
/// raw would prove nothing -- they are not serialized files, so the reader
/// would reject them at the sniffer without a decoder ever running, which is
/// exactly what the first version of this test did.
#[test]
fn damaged_texture_payloads_never_panic() {
    const MUTATIONS: usize = 300;
    // (fixture, texture format code, width, height, damage still decodes)
    //
    // A block format has no integrity check, so nearly any bit flip still
    // decodes to something -- which is the case that matters, because it means
    // damaged data reaches the decoder rather than being turned away. Crunch
    // is a container with a header it validates, so it rejects essentially
    // every mutation; that is correct behaviour and the panic check still runs
    // through its entry point.
    const CASES: &[(&str, i32, i32, i32, bool)] = &[
        ("astc/astc-rgba-8x8.bin", 57, 16, 16, true),
        ("astc/astc-hdr-4x4.bin", 66, 8, 8, true),
        ("bc6h/one-subset.bin", 24, 8, 8, true),
        ("crunch/unity_dxt1.crn", 28, 512, 512, false),
    ];

    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut rng = Rng(0x5EED_0004);
    for (relative, format, width, height, damage_decodes) in CASES {
        let payload = std::fs::read(directory.join(relative))
            .unwrap_or_else(|error| panic!("cannot read {relative}: {error}"));

        // The undamaged texture must decode, or the sweep is damaging something
        // that never worked and every result is a vacuous error.
        let clean = single_object_file(
            28,
            &texture2d_object("clean", *width, *height, *format, &payload),
        );
        assert!(decodes(&clean), "{relative} does not decode before damage");

        let mut decoded = 0_usize;
        for _ in 0..MUTATIONS {
            let mut damaged = payload.clone();
            let offset = rng.below(damaged.len());
            damaged[offset] ^= 1 << (rng.next() % 8);
            let bytes = single_object_file(
                28,
                &texture2d_object("damaged", *width, *height, *format, &damaged),
            );
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let result = catch_unwind(AssertUnwindSafe(|| decodes(&bytes)));
            std::panic::set_hook(previous);
            assert!(
                result.is_ok(),
                "{relative}: flipping a bit at offset {offset} panicked"
            );
            decoded += usize::from(result.unwrap_or(false));
        }
        assert_eq!(
            decoded > 0,
            *damage_decodes,
            "{relative}: {decoded} of {MUTATIONS} damaged payloads decoded, \
             which is not what this format is expected to do"
        );
    }
}

/// Decodes mip zero of the first texture in `bytes`, reporting success.
fn decodes(bytes: &[u8]) -> bool {
    let Ok(studio) = Studio::open_region("damaged", Region::from_bytes(bytes.to_vec())) else {
        return false;
    };
    studio.objects().next().is_some_and(|object| {
        object
            .decode_texture_mip(0, TextureReadLimits::default())
            .is_ok()
    })
}

/// Damages real FSB5 audio and converts it, which reaches the codec dispatch
/// and the Vorbis decoder behind it.
///
/// The audio path takes a `Region` directly, so unlike the texture payloads
/// this needs no object around it -- the public entry point is the one a
/// caller reaches. Vorbis is the interesting one: its setup header is
/// reconstructed from a table rather than read from the stream, so a damaged
/// stream can disagree with a setup that parsed cleanly.
#[test]
fn damaged_audio_never_panics() {
    const MUTATIONS: usize = 600;

    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio");
    let mut rng = Rng(0x5EED_0005);
    for relative in ["fsb5-vorbis-stereo.fsb", "fsb5-vorbis-stereo-silence.fsb"] {
        let payload = std::fs::read(directory.join(relative))
            .unwrap_or_else(|error| panic!("cannot read {relative}: {error}"));
        assert!(
            converts(&payload),
            "{relative} does not convert before damage, so mutating it proves nothing"
        );

        let mut converted = 0_usize;
        for _ in 0..MUTATIONS {
            let mut damaged = payload.clone();
            let offset = rng.below(damaged.len());
            damaged[offset] ^= 1 << (rng.next() % 8);
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let result = catch_unwind(AssertUnwindSafe(|| converts(&damaged)));
            std::panic::set_hook(previous);
            assert!(
                result.is_ok(),
                "{relative}: flipping a bit at offset {offset} panicked"
            );
            converted += usize::from(result.unwrap_or(false));
        }
        // FSB5 carries sizes and a CRC the reader checks, so most damage is
        // rejected -- but if nothing converted, the decoder was never reached.
        assert!(
            converted > 0,
            "{relative}: no damaged payload converted, so no decoder ran"
        );
    }
}

/// Converts an audio payload to WAV, reporting success.
fn converts(bytes: &[u8]) -> bool {
    let region = Region::from_bytes(bytes.to_vec());
    let Ok(Some(kind)) = detect_direct_wav(&region, Some(16)) else {
        return false;
    };
    let mut sink = Vec::new();
    write_direct_wav(&region, kind, 8 << 20, &mut sink).is_ok()
}

/// Damages a MOC3 header, which is read as offsets into its own payload.
///
/// The format puts four table offsets at fixed positions and then slices
/// fixed-width identifier records at each, so a damaged offset or count is a
/// direct invitation to read out of bounds -- the one place in this crate where
/// the payload chooses where the reader looks next.
#[test]
fn damaged_moc_headers_never_panic() {
    const MUTATIONS: usize = 1_500;
    const SCRIPT_PATH_ID: i64 = 200;
    const MOC_PATH_ID: i64 = 201;

    let seed = moc_payload();
    let file = |moc: &[u8]| {
        objects_file(&[
            (115, SCRIPT_PATH_ID, mono_script("CubismMoc")),
            (114, MOC_PATH_ID, moc_behaviour(SCRIPT_PATH_ID, moc)),
        ])
    };
    assert!(
        reads_moc(&file(&seed)),
        "the MOC seed does not read, so mutating it proves nothing"
    );

    let mut rng = Rng(0x5EED_0006);
    let mut read = 0_usize;
    for _ in 0..MUTATIONS {
        let mut damaged = seed.clone();
        // Bias towards the header, where the offsets and counts live: damage
        // out in the identifier tables only changes the strings.
        let offset = if rng.next() % 2 == 0 {
            rng.below(300)
        } else {
            rng.below(damaged.len())
        };
        damaged[offset] ^= 1 << (rng.next() % 8);
        let bytes = file(&damaged);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = catch_unwind(AssertUnwindSafe(|| reads_moc(&bytes)));
        std::panic::set_hook(previous);
        assert!(
            result.is_ok(),
            "MOC: flipping a bit at offset {offset} panicked"
        );
        read += usize::from(result.unwrap_or(false));
    }
    assert!(
        read > 0,
        "no damaged MOC header was read, so the parser was never reached"
    );
}

fn reads_moc(bytes: &[u8]) -> bool {
    let Ok(studio) = Studio::open_region("damaged", Region::from_bytes(bytes.to_vec())) else {
        return false;
    };
    (0..studio.file_count()).any(|file_index| {
        studio
            .objects()
            .filter(|object| object.file_index() == file_index)
            .any(|object| {
                try_read_cubism_moc(
                    studio.collection(),
                    file_index,
                    object.object_index(),
                    CubismMocReadLimits::default(),
                )
                .is_ok_and(|moc| moc.is_some())
            })
    })
}

/// A MOC3 header with both identifier tables populated.
fn moc_payload() -> Vec<u8> {
    const IDENTIFIER: usize = 64;
    const COUNT_TABLE: usize = 0x120;
    const CANVAS_INFO: usize = 0x140;
    const PART_IDS: usize = 0x180;

    let parts = ["PartA", "PartB"];
    let parameters = ["ParamX", "ParamY", "ParamZ"];
    let parameter_ids = PART_IDS + parts.len() * IDENTIFIER;
    let mut moc = vec![0_u8; parameter_ids + parameters.len() * IDENTIFIER];
    moc[..4].copy_from_slice(b"MOC3");
    moc[4] = 4; // SDK 4.2
    let put = |moc: &mut Vec<u8>, at: usize, value: u32| {
        moc[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };
    put(&mut moc, 64, u32::try_from(COUNT_TABLE).unwrap());
    put(&mut moc, 68, u32::try_from(CANVAS_INFO).unwrap());
    put(&mut moc, 76, u32::try_from(PART_IDS).unwrap());
    put(&mut moc, 264, u32::try_from(parameter_ids).unwrap());
    put(&mut moc, COUNT_TABLE, u32::try_from(parts.len()).unwrap());
    put(
        &mut moc,
        COUNT_TABLE + 20,
        u32::try_from(parameters.len()).unwrap(),
    );
    for (index, identifier) in parts.iter().chain(parameters.iter()).enumerate() {
        let at = if index < parts.len() {
            PART_IDS + index * IDENTIFIER
        } else {
            parameter_ids + (index - parts.len()) * IDENTIFIER
        };
        moc[at..at + identifier.len()].copy_from_slice(identifier.as_bytes());
    }
    moc
}

fn moc_behaviour(script_path_id: i64, moc: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&0_i32.to_le_bytes());
    data.extend_from_slice(&0_i64.to_le_bytes());
    data.push(1);
    pad(&mut data);
    data.extend_from_slice(&0_i32.to_le_bytes());
    data.extend_from_slice(&script_path_id.to_le_bytes());
    push_aligned_string(&mut data, "moc");
    data.extend_from_slice(&u32::try_from(moc.len()).unwrap().to_le_bytes());
    data.extend_from_slice(moc);
    pad(&mut data);
    data
}

fn mono_script(class_name: &str) -> Vec<u8> {
    let mut data = Vec::new();
    push_aligned_string(&mut data, class_name);
    data.extend_from_slice(&0_i32.to_le_bytes());
    data.extend_from_slice(&[0x55; 16]);
    push_aligned_string(&mut data, class_name);
    push_aligned_string(&mut data, "Live2D.Cubism.Core");
    push_aligned_string(&mut data, "Live2D.Cubism.dll");
    data
}
