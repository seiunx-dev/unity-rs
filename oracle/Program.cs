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
    Console.Write(JsonSerializer.Serialize(new { Files = files, Resources = resources }));
    return 0;
}
finally
{
    manager.Clear();
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
        Name = value is NamedObject named ? named.m_Name : null,
        Raw = Bytes(value.GetRawData()),
        Payload = payload,
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

static object SpritePayload(Sprite sprite)
{
    if (!sprite.m_RD.texture.TryGet(out var texture))
    {
        throw new InvalidDataException($"Sprite {sprite.m_PathID} texture does not resolve");
    }
    if ((int)texture.m_TextureFormat != 4
        || sprite.m_RD.settingsRaw.packed != 0
        || sprite.m_RD.settingsRaw.packingMode != SpritePackingMode.Rectangle)
    {
        throw new InvalidDataException("Sprite oracle only supports resident rectangle RGBA32");
    }
    var source = texture.image_data.GetData();
    var rect = sprite.m_RD.textureRect;
    var left = (int)MathF.Floor(rect.x);
    var top = (int)MathF.Floor(rect.y);
    var right = Math.Min((int)MathF.Ceiling(rect.x + rect.width), texture.m_Width);
    var bottom = Math.Min((int)MathF.Ceiling(rect.y + rect.height), texture.m_Height);
    var width = right - left;
    var height = bottom - top;
    if (left < 0 || top < 0 || width <= 0 || height <= 0
        || source.LongLength != checked((long)texture.m_Width * texture.m_Height * 4))
    {
        throw new InvalidDataException("Sprite oracle crop or texture payload is invalid");
    }
    long size = 0;
    var hash = 0xcbf29ce484222325UL;
    for (var y = 0; y < height; y++)
    {
        var sourceY = bottom - 1 - y;
        for (var x = left; x < right; x++)
        {
            var offset = checked((sourceY * texture.m_Width + x) * 4);
            hash = ContinueFnv1a64(source.AsSpan(offset, 4), hash);
            size = checked(size + 4);
        }
    }
    return new
    {
        Name = sprite.m_Name,
        Width = width,
        Height = height,
        Pixels = new { Size = size, Fnv64 = hash.ToString("x16") },
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

static object Bytes(byte[] value) => new
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
