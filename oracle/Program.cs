using System.Text.Json;
using System.Collections.Concurrent;
using System.Buffers.Binary;
using System.Reflection;
using AssetStudio;
using System.Text;
using K4os.Compression.LZ4;

if (args.Length != 1)
{
    Console.Error.WriteLine("usage: AssetStudioOracle <asset-file>");
    return 2;
}

Logger.Default = new OracleLogger();
var manager = new AssetsManager { LoadViaTypeTree = false };
try
{
    manager.LoadFilesAndFolders(args[0]);
    var files = manager.AssetsFileList.Select(file => new
    {
        Path = file.fileName,
        UnityVersion = file.version.ToString(),
        Objects = file.Objects.Select(OracleObject).ToArray(),
    }).ToArray();
    var resourceField = typeof(AssetsManager).GetField(
        "resourceFileReaders",
        BindingFlags.Instance | BindingFlags.NonPublic
    ) ?? throw new MissingFieldException(nameof(AssetsManager), "resourceFileReaders");
    var resourceReaders = resourceField.GetValue(manager)
        as ConcurrentDictionary<string, BinaryReader>
        ?? throw new InvalidCastException("managed resource-file table has an unexpected type");
    var resources = resourceReaders
        .OrderBy(entry => PortableFileName(entry.Key), StringComparer.OrdinalIgnoreCase)
        .ThenBy(entry => PortableFileName(entry.Key), StringComparer.Ordinal)
        .ThenBy(entry => entry.Key, StringComparer.Ordinal)
        .Select(entry => new
        {
            Path = PortableFileName(entry.Key),
            Data = StreamBytes(entry.Value),
        })
        .ToArray();
    Console.Write(JsonSerializer.Serialize(new
    {
        Files = files,
        Resources = resources,
        Live2D = Live2DPackages(manager),
    }));
    return 0;
}
finally
{
    manager.Clear();
}

// Runs the real managed Live2D extractor over whatever model the file holds
// and reports the documents it wrote.
//
// This drives CubismLive2DExtractor.ExtractCubismModel rather than
// reimplementing its grouping here: an oracle that restated the conversion
// would only compare this repository against its own reading of it, which is
// the weak pattern the sprite rows were already corrected for. The extractor
// classifies behaviours by their MonoScript class name, so a file without a
// CubismMoc-scripted behaviour simply produces nothing.
static object? Live2DPackages(AssetsManager manager)
{
    var objects = manager.AssetsFileList.SelectMany(file => file.Objects).ToList();
    var mocMono = objects.OfType<MonoBehaviour>().FirstOrDefault(behaviour =>
        behaviour.m_Script.TryGet(out var script) && script.m_ClassName == "CubismMoc");
    var modelMono = objects.OfType<MonoBehaviour>().FirstOrDefault(behaviour =>
        behaviour.m_Script.TryGet(out var script) && script.m_ClassName == "CubismModel");
    // The CLI only reaches the extractor for a group its model discovery
    // paired with a CubismModel component. Running it for a lone MOC would
    // extract a model that AssetStudio itself would never offer, and the
    // difference would be this harness rather than either implementation.
    if (mocMono == null || modelMono == null)
    {
        return null;
    }

    // The CLI populates this from its own model discovery, which builds a
    // CubismModel around the game object that owns the CubismModel behaviour.
    // Without it the extractor names the model after its output directory,
    // which is a temporary path here and would make every file name differ for
    // a reason that has nothing to do with either implementation.
    var mocDict = new Dictionary<MonoBehaviour, CubismModel>();
    if (modelMono.m_GameObject.TryGet(out var modelGameObject))
    {
        mocDict[mocMono] = new CubismModel(modelGameObject) { CubismModelMono = modelMono };
    }
    CubismLive2DExtractor.Live2DExtractor.MocDict = mocDict;
    var destination = Path.Combine(
        Path.GetTempPath(),
        $"assetstudio-oracle-live2d-{Environment.ProcessId}-{Guid.NewGuid():N}");
    try
    {
        var extractor = new CubismLive2DExtractor.Live2DExtractor(
            new KeyValuePair<MonoBehaviour, List<AssetStudio.Object>>(mocMono, objects));
        extractor.ExtractCubismModel(destination, CubismLive2DExtractor.Live2DMotionMode.MonoBehaviour);

        var documents = new SortedDictionary<string, object>(StringComparer.Ordinal);
        foreach (var path in Directory.EnumerateFiles(destination, "*", SearchOption.AllDirectories))
        {
            var relative = Path.GetRelativePath(destination, path).Replace('\\', '/');
            // JSON documents compare as values; anything else compares by
            // size and hash, since two encoders will not agree byte for byte
            // and the decoded-pixel rows already cover texture content.
            documents[relative] = relative.EndsWith(".json", StringComparison.Ordinal)
                ? new { Value = JsonSerializer.Deserialize<JsonElement>(File.ReadAllText(path)), Text = Bytes(File.ReadAllBytes(path)) }
                : (object)Bytes(File.ReadAllBytes(path));
        }
        return documents;
    }
    finally
    {
        if (Directory.Exists(destination))
        {
            Directory.Delete(destination, recursive: true);
        }
    }
}

static object OracleObject(AssetStudio.Object value)
{
    object? payload = value switch
    {
        Texture2D texture => TexturePayload(texture),
        Material material => MaterialPayload(material),
        Mesh mesh => MeshPayload(mesh),
        Sprite sprite => SpritePayload(sprite),
        Shader shader => ShaderPayload(shader),
        TextAsset text => new
        {
            Name = text.m_Name,
            Script = Bytes(text.m_Script),
        },
        AudioClip audio => new
        {
            Name = audio.m_Name,
            Extension = AudioExtension(audio),
            Data = Bytes(audio.m_AudioData.GetData()),
        },
        AnimationClip clip => AnimationClipPayload(clip),
        MonoBehaviour behaviour => MonoBehaviourPayload(behaviour),
        Avatar avatar => new
        {
            Name = avatar.m_Name,
            Tos = avatar.m_TOS.Select(entry => new
            {
                Key = entry.Key,
                Value = entry.Value,
            }).ToArray(),
        },
        AnimatorController controller => new
        {
            Name = controller.m_Name,
            Tos = controller.m_TOS.Select(entry => new
            {
                Key = entry.Key,
                Value = entry.Value,
            }).ToArray(),
            AnimationClips = controller.m_AnimationClips.Select(entry => new
            {
                FileId = entry.m_FileID,
                PathId = entry.m_PathID,
            }).ToArray(),
        },
        Font font => new
        {
            Name = font.m_Name,
            Extension = font.m_FontData.AsSpan().StartsWith("OTTO"u8) ? ".otf" : ".ttf",
            Data = Bytes(font.m_FontData),
        },
        MovieTexture movie => new
        {
            Name = movie.m_Name,
            Extension = ".ogv",
            Data = Bytes(movie.m_MovieData),
        },
        BuildSettings build => new
        {
            Levels = build.levels,
            Scenes = build.scenes,
        },
        PlayerSettings player => new
        {
            CompanyName = player.companyName,
            ProductName = player.productName,
        },
        VideoClip video => new
        {
            Name = video.m_Name,
            Extension = Path.GetExtension(video.m_OriginalPath),
            Data = Bytes(video.m_VideoData.GetData()),
        },
        _ when value.classID == 123456 => new
        {
            Dump = value.Dump(),
        },
        _ => null,
    };
    return new
    {
        PathId = value.m_PathID,
        ClassId = value.classID,
        ByteSize = value.byteSize,
        // MonoBehaviour carries m_Name and the managed reader parses it, but
        // the class sits under Behaviour rather than NamedObject, so the base
        // class alone under-reports what this implementation knows about the
        // file. This is not a shim to make the sides agree: it reports the
        // managed parse, which is what the oracle is for.
        Name = value switch
        {
            NamedObject named => named.m_Name,
            MonoBehaviour behaviour => behaviour.m_Name,
            _ => null,
        },
        Raw = Bytes(value.GetRawData()),
        Payload = payload,
    };
}

// An absent list and an empty one both mean the clip has no curves of that
// kind, so both hash as empty rather than one becoming null: the row stays a
// value comparison instead of a presence check.
//
// Curve keyframes hashed as a flat little-endian stream: the path, then every
// keyframe's time, value and both tangents as raw float bits. Bit patterns
// rather than decimal text so a rounding difference cannot hide.
static object QuaternionCurves(List<QuaternionCurve> curves)
{
    var values = new List<uint>();
    foreach (var curve in curves ?? [])
    {
        AppendPath(values, curve.path);
        foreach (var key in curve.curve.m_Curve)
        {
            values.Add(BitConverter.SingleToUInt32Bits(key.time));
            AppendQuaternion(values, key.value);
            AppendQuaternion(values, key.inSlope);
            AppendQuaternion(values, key.outSlope);
        }
    }
    return UInt32Values(values);
}

static object Vector3Curves(List<Vector3Curve> curves)
{
    var values = new List<uint>();
    foreach (var curve in curves ?? [])
    {
        AppendPath(values, curve.path);
        foreach (var key in curve.curve.m_Curve)
        {
            values.Add(BitConverter.SingleToUInt32Bits(key.time));
            AppendVector3(values, key.value);
            AppendVector3(values, key.inSlope);
            AppendVector3(values, key.outSlope);
        }
    }
    return UInt32Values(values);
}

static object FloatCurves(List<FloatCurve> curves)
{
    var values = new List<uint>();
    foreach (var curve in curves ?? [])
    {
        AppendPath(values, curve.path);
        AppendPath(values, curve.attribute);
        values.Add(unchecked((uint)curve.classID));
        foreach (var key in curve.curve.m_Curve)
        {
            values.Add(BitConverter.SingleToUInt32Bits(key.time));
            values.Add(BitConverter.SingleToUInt32Bits(key.value));
            values.Add(BitConverter.SingleToUInt32Bits(key.inSlope));
            values.Add(BitConverter.SingleToUInt32Bits(key.outSlope));
        }
    }
    return UInt32Values(values);
}

static void AppendPath(List<uint> values, string path)
{
    values.Add((uint)(path?.Length ?? 0));
    foreach (var character in path ?? string.Empty)
    {
        values.Add(character);
    }
}

static void AppendVector3(List<uint> values, Vector3 value)
{
    values.Add(BitConverter.SingleToUInt32Bits(value.X));
    values.Add(BitConverter.SingleToUInt32Bits(value.Y));
    values.Add(BitConverter.SingleToUInt32Bits(value.Z));
}

static void AppendQuaternion(List<uint> values, Quaternion value)
{
    values.Add(BitConverter.SingleToUInt32Bits(value.X));
    values.Add(BitConverter.SingleToUInt32Bits(value.Y));
    values.Add(BitConverter.SingleToUInt32Bits(value.Z));
    values.Add(BitConverter.SingleToUInt32Bits(value.W));
}

// Cubism data is a MonoBehaviour whose layout comes from the Live2D SDK's own
// types, so a reader has to take it from the TypeTree the file carries. When
// the tree describes a physics rig, this runs the managed conversion the CLI
// would run and hands back the physics3.json it produces, parsed rather than as
// text: the two implementations format JSON differently, and formatting is not
// what the comparison is about.
static object MonoBehaviourPayload(MonoBehaviour behaviour)
{
    // A CubismMoc behaviour is read raw rather than through a TypeTree, so it
    // is recognised by its script rather than by a field name.
    if (behaviour.m_Script.TryGet(out var script) && script.m_ClassName == "CubismMoc")
    {
        using var moc = new AssetStudio.CubismMoc(behaviour);
        return new
        {
            Name = behaviour.m_Name,
            Moc = new
            {
                Version = (int)moc.Version,
                moc.VersionDescription,
                // Bit patterns, matching how the curve rows carry keyframes:
                // comparing the JSON spelling of a float would compare two
                // languages' formatters rather than the value.
                CanvasWidth = BitConverter.SingleToUInt32Bits(moc.CanvasWidth),
                CanvasHeight = BitConverter.SingleToUInt32Bits(moc.CanvasHeight),
                CentralPosX = BitConverter.SingleToUInt32Bits(moc.CentralPosX),
                CentralPosY = BitConverter.SingleToUInt32Bits(moc.CentralPosY),
                PixelPerUnit = BitConverter.SingleToUInt32Bits(moc.PixelPerUnit),
                moc.PartCount,
                moc.ParamCount,
                // Sorted because the managed side collects these into hash
                // sets, which have no order to compare.
                PartNames = moc.PartNames.OrderBy(name => name, StringComparer.Ordinal).ToArray(),
                ParamNames = moc.ParamNames.OrderBy(name => name, StringComparer.Ordinal).ToArray(),
            },
            Physics = (JsonElement?)null,
            Motion = (JsonElement?)null,
            Expression = (JsonElement?)null,
            PhysicsText = (string?)null,
            MotionText = (string?)null,
            ExpressionText = (string?)null,
        };
    }

    var parsed = behaviour.ToType();
    string? physics = null;
    string? motion = null;
    string? expressionJson = null;
    if (parsed != null && parsed.Contains("_rig"))
    {
        // The fps argument is the fallback the converter uses when the rig
        // does not carry one of its own.
        physics = CubismLive2DExtractor.CubismParsers.ParsePhysics(parsed, 30f);
    }
    else if (parsed != null && parsed.Contains("Parameters") && parsed.Contains("FadeInTime"))
    {
        // exp3.json goes out with no custom converter, so its floats take
        // Newtonsoft's default format rather than the one the segment lists use.
        var expression = Newtonsoft.Json.JsonConvert
            .DeserializeObject<CubismLive2DExtractor.CubismExpression3Json>(
                Newtonsoft.Json.JsonConvert.SerializeObject(parsed));
        expressionJson = Newtonsoft.Json.JsonConvert.SerializeObject(
            expression, Newtonsoft.Json.Formatting.Indented);
    }
    else if (parsed != null && parsed.Contains("ParameterIds"))
    {
        // The fade-motion route to motion3.json: one behaviour in, one
        // document out, which is what makes it comparable on its own.
        var fade = Newtonsoft.Json.JsonConvert
            .DeserializeObject<CubismLive2DExtractor.CubismUnityClasses.CubismFadeMotionData>(
                Newtonsoft.Json.JsonConvert.SerializeObject(parsed));
        var motionJson = new CubismLive2DExtractor.CubismMotion3Json(
            fade,
            // The names a model would supply. Both sides receive the same
            // pair, which is what lets the comparison reach the Parameter and
            // PartOpacity branches instead of only the unbound fallback.
            new System.Collections.Generic.HashSet<string> { "ParamAngleX" },
            new System.Collections.Generic.HashSet<string> { "PartArmA" },
            false);
        motion = Newtonsoft.Json.JsonConvert.SerializeObject(
            motionJson,
            Newtonsoft.Json.Formatting.Indented,
            new CubismLive2DExtractor.MyJsonConverter());
    }
    return new
    {
        Name = behaviour.m_Name,
        Moc = (object?)null,
        Physics = physics == null
            ? (JsonElement?)null
            : JsonSerializer.Deserialize<JsonElement>(physics),
        Motion = motion == null
            ? (JsonElement?)null
            : JsonSerializer.Deserialize<JsonElement>(motion),
        Expression = expressionJson == null
            ? (JsonElement?)null
            : JsonSerializer.Deserialize<JsonElement>(expressionJson),
        // The document text, not only what it parses to. The layout and the
        // number spellings are part of what these files are.
        PhysicsText = physics == null ? null : Bytes(Encoding.UTF8.GetBytes(physics)),
        MotionText = motion == null ? null : Bytes(Encoding.UTF8.GetBytes(motion)),
        ExpressionText = expressionJson == null ? null : Bytes(Encoding.UTF8.GetBytes(expressionJson)),
    };
}

static object AnimationClipPayload(AnimationClip clip)
{
    var muscle = clip.m_MuscleClip;
    var acl = muscle?.m_Clip?.data?.m_ACLClip;
    var streaming = clip.m_StreamingInfo;
    return new
    {
        Name = clip.m_Name,
        SampleRateBits = BitConverter.SingleToUInt32Bits(clip.m_SampleRate),
        WrapMode = clip.m_WrapMode,
        EulerCurveCount = clip.m_EulerCurves?.Count ?? 0,
        // Keyframe values, not just curve counts. Everything below used to be
        // compared by shape alone, so the times, values and tangents each
        // reader produced were only ever checked against its own expectations.
        RotationCurves = QuaternionCurves(clip.m_RotationCurves),
        EulerCurves = Vector3Curves(clip.m_EulerCurves),
        PositionCurves = Vector3Curves(clip.m_PositionCurves),
        ScaleCurves = Vector3Curves(clip.m_ScaleCurves),
        FloatCurves = FloatCurves(clip.m_FloatCurves),
        MusclePresent = muscle is not null,
        StreamedCurveCount = muscle?.m_Clip?.data?.m_StreamedClip?.curveCount,
        Acl = acl is null ? null : new
        {
            FrameCount = acl.m_FrameCount,
            BoneCount = acl.m_BoneCount,
            SampleRateBits = BitConverter.SingleToUInt32Bits(acl.m_SampleRate),
            CurveCount = acl.m_CurveCount,
            Tracks = Bytes(acl.m_Tracks),
            DecoderMap = acl.m_ACLDecoderMap,
            UseFastSampleMode = acl.m_UseACLFastSampleMode,
        },
        Streaming = streaming is null ? null : new
        {
            Offset = streaming.offset,
            Size = streaming.size,
            Path = streaming.path,
        },
    };
}

// Covers the 5.3-5.4 subprogram blob as well as the legacy direct script. The
// guard that used to sit here refused the blob, so the converted-text path was
// compared against nothing.
//
// It goes through ShaderProgram rather than ShaderConverter.Convert/WriteTo,
// which would be the natural entry point, because ShaderConverter's static
// constructor throws: its HeaderBytes field is initialized from the `header`
// field declared ~880 lines below it, and C# runs static initializers in
// declaration order, so `header` is still null. That poisons the whole type,
// which is why the header bytes are spelled out here. ShaderProgram is a
// separate type with its own initializer, so the managed reader and exporter --
// the part this actually needs to compare against -- are still what runs.
//
// 5.5+ serialized shaders remain uncovered: reaching them means reproducing
// more of ConvertSerializedShader here, which is worth doing only once the
// upstream initializer is fixed and Convert can be called directly.
// Compares the decoded pixels, not only the stored payload.
//
// Every block decoder -- BC1 through BC7, ETC, EAC, ASTC, PVRTC, the Crunch
// variants and the Switch deswizzle -- used to be compared against nothing at
// all here, because only the raw `image_data` bytes were hashed and those come
// straight off disk. The managed decoder is the original C++ implementation and
// the Rust one is an independent port of it, so this is a real cross-check.
//
// Rows come out in Unity's own bottom-up order because `ConvertToImage`'s flip
// is not applied, matching what the Rust reader returns. Pixels are normalized
// from BGRA to RGBA, and a Switch-swizzled texture is cropped from its padded
// stride to the declared width.
static object TexturePayload(Texture2D texture)
{
    object decoded;
    using (var pixels = texture.DecodeBgra32())
    {
        if (pixels is null)
        {
            decoded = null;
        }
        else
        {
            var rgba = new byte[checked(pixels.Width * pixels.Height * 4)];
            var source = pixels.Pixels;
            var destination = 0;
            for (var row = 0; row < pixels.Height; row++)
            {
                var rowStart = row * pixels.SourceWidth * 4;
                for (var column = 0; column < pixels.Width; column++)
                {
                    var offset = rowStart + column * 4;
                    rgba[destination] = source[offset + 2];
                    rgba[destination + 1] = source[offset + 1];
                    rgba[destination + 2] = source[offset];
                    rgba[destination + 3] = source[offset + 3];
                    destination += 4;
                }
            }
            decoded = Bytes(rgba);
        }
    }
    return new
    {
        Name = texture.m_Name,
        Width = texture.m_Width,
        Height = texture.m_Height,
        TextureFormat = (int)texture.m_TextureFormat,
        MipCount = texture.m_MipCount,
        Data = Bytes(texture.image_data.GetData()),
        Decoded = decoded,
    };
}

static object ShaderPayload(Shader shader)
{
    ReadOnlySpan<byte> header = "//////////////////////////////////////////\n//\n// NOTE: This is *not* a valid shader file\n//\n///////////////////////////////////////////\n"u8;
    if (shader.compressedBlob is not null)
    {
        throw new InvalidDataException("Shader oracle does not yet cover 5.5+ serialized shaders");
    }

    byte[] body;
    if (shader.m_SubProgramBlob is not null)
    {
        var decompressed = new byte[shader.decompressedSize];
        LZ4Codec.Decode(shader.m_SubProgramBlob, decompressed);
        using var blobReader = new BinaryReader(new MemoryStream(decompressed));
        var program = new ShaderProgram(blobReader, shader.version);
        program.Read(blobReader, 0);
        var script = Encoding.UTF8.GetString(shader.m_Script ?? Array.Empty<byte>());
        body = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false)
            .GetBytes(program.Export(script));
    }
    else
    {
        body = shader.m_Script ?? Array.Empty<byte>();
    }

    var hash = ContinueFnv1a64(body, ContinueFnv1a64(header, 0xcbf29ce484222325UL));
    return new
    {
        Name = shader.m_Name,
        Data = new
        {
            Size = checked(header.Length + body.LongLength),
            Fnv64 = hash.ToString("x16"),
        },
    };
}

// Compares against the managed sprite pipeline itself rather than a crop
// reimplemented here. GetImage is what AssetStudio exports through: it resolves
// an atlas render-data entry when there is one, cuts the texture to the
// sprite's rect, applies the tight-mesh mask and the alpha texture, and
// downscales. A hand-written rectangle crop -- which is what used to be here --
// only ever compared this reader against a second copy of its own assumptions
// and could not reach the tight-mesh path at all.
static object SpritePayload(Sprite sprite)
{
    using var image = sprite.GetImage(SpriteMaskMode.Export);
    if (image is null)
    {
        throw new InvalidDataException($"Sprite {sprite.m_PathID} produced no image");
    }
    var pixels = new byte[checked(image.Width * image.Height * 4)];
    image.CopyPixelDataTo(pixels);
    // ImageSharp hands back BGRA; the Rust manifest reports RGBA.
    for (var offset = 0; offset < pixels.Length; offset += 4)
    {
        (pixels[offset], pixels[offset + 2]) = (pixels[offset + 2], pixels[offset]);
    }
    return new
    {
        Name = sprite.m_Name,
        Width = image.Width,
        Height = image.Height,
        Pixels = Bytes(pixels),
    };
}

static object MaterialPayload(Material material)
{
    return new
    {
        Name = material.m_Name,
        Shader = new
        {
            FileId = material.m_Shader.m_FileID,
            PathId = material.m_Shader.m_PathID,
        },
        TextureEnvironments = material.m_SavedProperties.m_TexEnvs.Select(entry => new
        {
            Name = entry.Key,
            Texture = new
            {
                FileId = entry.Value.m_Texture.m_FileID,
                PathId = entry.Value.m_Texture.m_PathID,
            },
            ScaleBits = new[]
            {
                BitConverter.SingleToUInt32Bits(entry.Value.m_Scale.X),
                BitConverter.SingleToUInt32Bits(entry.Value.m_Scale.Y),
            },
            OffsetBits = new[]
            {
                BitConverter.SingleToUInt32Bits(entry.Value.m_Offset.X),
                BitConverter.SingleToUInt32Bits(entry.Value.m_Offset.Y),
            },
        }).ToArray(),
        Integers = (material.m_SavedProperties.m_Ints
            ?? new List<KeyValuePair<string, int>>()).Select(entry => new
        {
            Name = entry.Key,
            Value = entry.Value,
        }).ToArray(),
        Floats = material.m_SavedProperties.m_Floats.Select(entry => new
        {
            Name = entry.Key,
            ValueBits = BitConverter.SingleToUInt32Bits(entry.Value),
        }).ToArray(),
        Colors = material.m_SavedProperties.m_Colors.Select(entry => new
        {
            Name = entry.Key,
            ValueBits = new[]
            {
                BitConverter.SingleToUInt32Bits(entry.Value.R),
                BitConverter.SingleToUInt32Bits(entry.Value.G),
                BitConverter.SingleToUInt32Bits(entry.Value.B),
                BitConverter.SingleToUInt32Bits(entry.Value.A),
            },
        }).ToArray(),
    };
}

static object MeshPayload(Mesh mesh)
{
    mesh.ProcessData();
    return new
    {
        Obj = MeshObj(mesh),
        Name = mesh.m_Name,
        VertexCount = mesh.m_VertexCount,
        Vertices = FloatValues(mesh.m_Vertices),
        // An empty channel counts as absent: this reader allocates a zero-length
        // array where the Rust one leaves the option unset, and both mean the
        // mesh has no such channel.
        Normals = mesh.m_Normals is null or { Length: 0 } ? null : FloatValues(mesh.m_Normals),
        Uv0 = mesh.m_UV0 is null or { Length: 0 } ? null : FloatValues(mesh.m_UV0),
        Indices = UInt32Values(mesh.m_Indices),
    };
}

// Drives the managed OBJ writer rather than restating it here, for the reason
// the Live2D rows drive the managed extractor: an oracle that reimplements the
// thing it checks proves only that the reimplementation agrees with itself.
// The geometry rows below already compare what the writer is given; this
// compares what it produces, which is where the negated axis, the reversed
// winding, the one-based indices and the invariant number format live.
//
// The method is private, so this reaches it the same way the resource-table
// row reaches a private field. The writer is constructed exactly as
// ReadMeshPayloadInto constructs it, because the newline it is given is part
// of the document.
static object MeshObj(Mesh mesh)
{
    var method = typeof(AssetStudioCore.AssetStudioSession).GetMethod(
        "WriteMeshObj",
        BindingFlags.Static | BindingFlags.NonPublic
    ) ?? throw new MissingMethodException("AssetStudioSession", "WriteMeshObj");

    using var buffer = new MemoryStream();
    using (var writer = new StreamWriter(buffer, new UTF8Encoding(false), 8192, leaveOpen: true)
    {
        NewLine = "\r\n",
    })
    {
        try
        {
            method.Invoke(null, new object[] { mesh, writer });
        }
        catch (TargetInvocationException error) when (error.InnerException is not null)
        {
            throw error.InnerException;
        }
        writer.Flush();
    }
    return Bytes(buffer.ToArray());
}

static object FloatValues(IEnumerable<float> values)
{
    return UInt32Values(values.Select(value => BitConverter.SingleToUInt32Bits(value)));
}

static object UInt32Values(IEnumerable<uint> values)
{
    long count = 0;
    long size = 0;
    var hash = 0xcbf29ce484222325UL;
    Span<byte> bytes = stackalloc byte[4];
    foreach (var value in values)
    {
        BinaryPrimitives.WriteUInt32LittleEndian(bytes, value);
        hash = ContinueFnv1a64(bytes, hash);
        count = checked(count + 1);
        size = checked(size + bytes.Length);
    }
    return new
    {
        Count = count,
        Data = new { Size = size, Fnv64 = hash.ToString("x16") },
    };
}

// A null array is not an empty one, and real files carry both: a Font whose
// data never loaded reaches here as null and used to take the whole run down.
static object? Bytes(byte[]? value) => value == null ? null : new
{
    Size = value.LongLength,
    Fnv64 = Fnv1a64(value).ToString("x16"),
};

static object StreamBytes(BinaryReader reader)
{
    lock (reader)
    {
        var stream = reader.BaseStream;
        var position = stream.Position;
        try
        {
            stream.Position = 0;
            var buffer = new byte[64 * 1024];
            long size = 0;
            var hash = 0xcbf29ce484222325UL;
            while (true)
            {
                var read = stream.Read(buffer, 0, buffer.Length);
                if (read == 0)
                {
                    break;
                }
                size = checked(size + read);
                hash = ContinueFnv1a64(buffer.AsSpan(0, read), hash);
            }
            return new { Size = size, Fnv64 = hash.ToString("x16") };
        }
        finally
        {
            stream.Position = position;
        }
    }
}

static ulong Fnv1a64(ReadOnlySpan<byte> value)
{
    return ContinueFnv1a64(value, 0xcbf29ce484222325UL);
}

static ulong ContinueFnv1a64(ReadOnlySpan<byte> value, ulong hash)
{
    foreach (var item in value)
    {
        hash ^= item;
        hash = unchecked(hash * 0x100000001b3UL);
    }
    return hash;
}

static string PortableFileName(string path)
{
    var slash = Math.Max(path.LastIndexOf('/'), path.LastIndexOf('\\'));
    var archive = path.LastIndexOf("::", StringComparison.Ordinal);
    var start = Math.Max(slash + 1, archive < 0 ? 0 : archive + 2);
    return path[start..];
}

static string AudioExtension(AudioClip audio) => audio.m_CompressionFormat switch
{
    AudioCompressionFormat.AAC => ".m4a",
    _ => ".fsb",
};

sealed class OracleLogger : ILogger
{
    public void Log(LoggerEvent loggerEvent, string message, bool ignoreLevel = false)
    {
        if (loggerEvent >= LoggerEvent.Warning)
        {
            Console.Error.WriteLine(message);
        }
    }
}
