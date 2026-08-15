// Writes MonoBehaviour schemas from a game's managed assemblies.
//
// A release build almost never ships type trees for its own scripts, so a
// MonoBehaviour arrives as a MonoScript reference and an opaque byte range.
// The layout lives in the assembly the script was compiled into, and this
// walks a directory of them -- a dummy DLL set from an IL2CPP dump works, and
// so does a Mono build's Managed directory -- turning every serializable
// UnityEngine.Object subclass into the JSON document
// MonoBehaviourSchemaRegistry::from_json reads.
//
//     dotnet run --project tools/monoschema -- <dll-directory> <unity-version> <output.json>
//                                              [--assembly <name>]...
//
// --assembly narrows the output to the named assemblies, repeatable and
// matched without the .dll suffix; without it every assembly in the directory
// is walked, which for a large game is tens of megabytes of JSON.
//
// This is deliberately a separate program. Opening a game's managed assemblies
// is a different trust decision from reading its data files, and nothing in
// the Rust crates links a managed reader or loads a DLL: they consume the JSON
// this writes.

using System.Reflection;
using System.Text.Json;

using AssetStudio;

using Mono.Cecil;

// The classes Unity serializes through the MonoBehaviour path. ScriptableObject
// assets are stored as class 114 too, so both roots are walked.
string[] serializedRoots = ["UnityEngine.MonoBehaviour", "UnityEngine.ScriptableObject"];

// The types an enum can be backed by, which is also the set a dummy DLL puts
// in place of System.Enum.
string[] integralTypeNames =
[
    "System.Byte", "System.SByte", "System.Int16", "System.UInt16",
    "System.Int32", "System.UInt32", "System.Int64", "System.UInt64",
    "System.Char", "System.Boolean",
];

var positional = new List<string>();
var assemblyFilter = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
for (var index = 0; index < args.Length; index++)
{
    if (args[index] == "--assembly")
    {
        if (index + 1 >= args.Length)
        {
            Console.Error.WriteLine("--assembly needs an assembly name");
            return 2;
        }
        assemblyFilter.Add(TrimDllSuffix(args[++index]));
        continue;
    }
    positional.Add(args[index]);
}

if (positional.Count != 3)
{
    Console.Error.WriteLine(
        "usage: monoschema <dll-directory> <unity-version> <output.json> [--assembly <name>]...");
    return 2;
}

var directory = positional[0];
var unityVersionText = positional[1];
var outputPath = positional[2];

if (!Directory.Exists(directory))
{
    Console.Error.WriteLine($"{directory}: not a directory");
    return 2;
}

UnityVersion unityVersion;
try
{
    unityVersion = new UnityVersion(unityVersionText);
}
catch (Exception error)
{
    Console.Error.WriteLine($"{unityVersionText}: not a Unity version: {error.Message}");
    return 2;
}

var loader = new AssemblyLoader();
loader.Load(directory);

// AssemblyLoader exposes lookup but not enumeration, and its resolver is the
// one every Resolve() below depends on, so the modules are read back off it
// rather than opened a second time against a resolver that knows nothing.
var moduleField = typeof(AssemblyLoader).GetField(
    "moduleDic",
    BindingFlags.Instance | BindingFlags.NonPublic);
if (moduleField?.GetValue(loader) is not Dictionary<string, ModuleDefinition> modules)
{
    Console.Error.WriteLine("AssemblyLoader no longer keeps its modules in moduleDic");
    return 1;
}
if (modules.Count == 0)
{
    Console.Error.WriteLine($"{directory}: no assembly in it could be read");
    return 1;
}

var repairedEnums = RepairDummyEnums(modules, [.. integralTypeNames]);
if (repairedEnums > 0)
{
    Console.Error.WriteLine($"repaired {repairedEnums} enum(s) that declared a primitive base type");
}

var helper = new SerializedTypeHelper(unityVersion);
var entries = new List<Dictionary<string, object?>>();
var skipped = 0;
var incomplete = 0;

foreach (var (moduleName, module) in modules.OrderBy(pair => pair.Key, StringComparer.Ordinal))
{
    if (assemblyFilter.Count > 0 && !assemblyFilter.Contains(TrimDllSuffix(moduleName)))
    {
        continue;
    }

    foreach (var type in EnumerateTypes(module))
    {
        // A generic definition has no single layout, and Unity does not
        // serialize an abstract or interface type as an asset either.
        if (type.HasGenericParameters || type.IsInterface || type.IsAbstract)
        {
            continue;
        }
        if (!DerivesFromSerializedRoot(type))
        {
            continue;
        }

        List<TypeTreeNode> nodes;
        try
        {
            nodes = [];
            helper.AddMonoBehaviour(nodes, 0);
            nodes.AddRange(new TypeDefinitionConverter(type, helper, 1).ConvertToTypeTreeNodes());
        }
        catch (Exception error)
        {
            // A dummy DLL set is routinely incomplete: a field can reference a
            // type no assembly in the directory defines. Losing that one class
            // is the right outcome, and counting the losses keeps it visible.
            skipped++;
            Console.Error.WriteLine($"  skipped {moduleName}:{type.FullName}: {error.Message}");
            continue;
        }

        var dropped = UnresolvableSerializedFields(type);
        if (dropped.Count > 0)
        {
            // The schema is short and nothing downstream can tell why. The
            // Rust reader still refuses the object -- its type tree will not
            // account for every byte -- but a message here names the missing
            // assembly, which is the only place the problem can be fixed.
            incomplete++;
            Console.Error.WriteLine(
                $"  incomplete {moduleName}:{type.FullName}: "
                + $"{string.Join(", ", dropped)} dropped, their types resolve to nothing");
        }

        entries.Add(new Dictionary<string, object?>
        {
            ["assembly"] = moduleName,
            ["namespace"] = type.Namespace ?? string.Empty,
            // Nested classes are serialized under their own name; the
            // MonoScript in the file records exactly this.
            ["class"] = type.Name,
            ["nodes"] = nodes.Select(node => new Dictionary<string, object>
            {
                ["level"] = node.m_Level,
                ["type"] = node.m_Type,
                ["name"] = node.m_Name,
                ["meta_flags"] = node.m_MetaFlag,
                ["type_flags"] = node.m_TypeFlags,
            }).ToList(),
        });
    }
}

if (entries.Count == 0)
{
    Console.Error.WriteLine("no serializable class was found, so the document would say nothing");
    return 1;
}

var document = new Dictionary<string, object?>
{
    ["version"] = 1,
    // Informational: the reader ignores it. Entries carry no unity_version
    // because they apply wherever their layout does, and recording an exact
    // build string here would decline every file whose own string differs.
    ["generated_for"] = unityVersion.FullVersion,
    ["entries"] = entries,
};

await using (var stream = File.Create(outputPath))
{
    await JsonSerializer.SerializeAsync(
        stream,
        document,
        new JsonSerializerOptions { WriteIndented = false });
}

Console.WriteLine(
    $"{entries.Count} class(es) from {modules.Count} assembly(ies) -> {outputPath}"
    + (skipped > 0 ? $", {skipped} skipped" : string.Empty)
    + (incomplete > 0 ? $", {incomplete} incomplete" : string.Empty));
return 0;

/// <summary>
/// Names the fields a class loses because their type is not in this directory.
/// </summary>
/// <remarks>
/// Unity's serialization logic answers "no, do not serialize" both for a field
/// that genuinely is not serialized and for one whose type it could not
/// resolve, and the two are indistinguishable in the tree that comes out. A
/// dummy DLL set missing one assembly therefore produces schemas that are
/// quietly short for every class touching it. Asking separately -- would this
/// field qualify, and does its type resolve -- separates the two.
/// </remarks>
static List<string> UnresolvableSerializedFields(TypeDefinition type)
{
    var dropped = new List<string>();
    for (TypeDefinition? current = type; current is not null;)
    {
        foreach (var field in current.Fields)
        {
            if (field.IsStatic || field.IsInitOnly || !LooksSerialized(field))
            {
                continue;
            }
            if (!ResolvesCompletely(field.FieldType))
            {
                dropped.Add($"{current.Name}.{field.Name}");
            }
        }
        try
        {
            current = current.BaseType?.Resolve();
        }
        catch
        {
            break;
        }
    }
    return dropped;
}

static bool LooksSerialized(FieldDefinition field) =>
    field.IsPublic
    || field.CustomAttributes.Any(attribute =>
        attribute.AttributeType.Name is "SerializeField" or "SerializeReference");

static bool ResolvesCompletely(TypeReference reference)
{
    switch (reference)
    {
        case ArrayType array:
            return ResolvesCompletely(array.ElementType);
        case GenericInstanceType generic:
            return generic.GenericArguments.All(ResolvesCompletely)
                && ResolvesCompletely(generic.ElementType);
        case GenericParameter:
            return true;
    }
    if (reference.IsPrimitive || reference.Namespace.StartsWith("System", StringComparison.Ordinal))
    {
        return true;
    }
    try
    {
        return reference.Resolve() is not null;
    }
    catch
    {
        return false;
    }
}

/// <summary>
/// Gives back the enums an Il2CppDumper dummy assembly lost.
/// </summary>
/// <remarks>
/// A dummy DLL writes an enum with its underlying type as the base --
/// <c>System.Int32</c> rather than <c>System.Enum</c> -- and Cecil then does
/// not see an enum at all. Unity's serialization logic asks whether the field
/// type is a serializable value type, gets no for a class deriving from
/// Int32, and drops the field. Nothing reports it: the schema is simply four
/// bytes short, and the only symptom is a read running off the end of the
/// object much later. UnityEngine.UI.ScrollRect alone loses three fields.
///
/// An enum is still recognizable by shape -- one instance field named
/// value__, of an integral type -- so the base type is put back before any
/// conversion runs.
/// </remarks>
static int RepairDummyEnums(Dictionary<string, ModuleDefinition> modules, HashSet<string> integral)
{
    var repaired = new HashSet<string>(StringComparer.Ordinal);
    foreach (var module in modules.Values)
    {
        var systemEnum = new TypeReference(
            "System",
            "Enum",
            module,
            module.TypeSystem.CoreLibrary);
        foreach (var type in EnumerateTypes(module))
        {
            if (type.IsInterface || type.BaseType is null)
            {
                continue;
            }
            if (type.BaseType is { Namespace: "System", Name: "Enum" })
            {
                continue;
            }
            if (!integral.Contains(type.BaseType.FullName))
            {
                continue;
            }
            var instanceFields = type.Fields.Where(field => !field.IsStatic).ToList();
            if (instanceFields.Count != 1
                || instanceFields[0].Name != "value__"
                || !integral.Contains(instanceFields[0].FieldType.FullName))
            {
                continue;
            }
            // TypeDefinition.IsValueType is derived from the base type, so
            // this fixes both the enum test and the value-type test at once.
            type.BaseType = systemEnum;
            repaired.Add(type.FullName);
        }
    }

    if (repaired.Count == 0)
    {
        return 0;
    }

    // A reference from another module carries its own value-type flag, which
    // the same defect leaves clear. Repairing the definition does not reach it.
    foreach (var module in modules.Values)
    {
        foreach (var type in EnumerateTypes(module))
        {
            foreach (var field in type.Fields)
            {
                RepairReference(field.FieldType, repaired);
            }
        }
    }
    return repaired.Count;
}

static void RepairReference(TypeReference reference, HashSet<string> repaired)
{
    switch (reference)
    {
        case ArrayType array:
            RepairReference(array.ElementType, repaired);
            return;
        case GenericInstanceType generic:
            foreach (var argument in generic.GenericArguments)
            {
                RepairReference(argument, repaired);
            }
            return;
        // A TypeDefinition derives the flag and refuses to have it set.
        case TypeDefinition:
            return;
        default:
            if (repaired.Contains(reference.FullName))
            {
                reference.IsValueType = true;
            }
            return;
    }
}

static string TrimDllSuffix(string name) =>
    name.EndsWith(".dll", StringComparison.OrdinalIgnoreCase) ? name[..^4] : name;

static IEnumerable<TypeDefinition> EnumerateTypes(ModuleDefinition module)
{
    foreach (var type in module.Types)
    {
        yield return type;
        // Nested types are serialized too, and a game's UI scripts nest freely.
        foreach (var nested in Nested(type))
        {
            yield return nested;
        }
    }

    static IEnumerable<TypeDefinition> Nested(TypeDefinition type)
    {
        foreach (var nested in type.NestedTypes)
        {
            yield return nested;
            foreach (var deeper in Nested(nested))
            {
                yield return deeper;
            }
        }
    }
}

bool DerivesFromSerializedRoot(TypeDefinition type)
{
    var current = type.BaseType;
    // Bounded rather than while(true): a malformed assembly can describe a
    // cycle, and no real hierarchy is anywhere near this deep.
    for (var depth = 0; current is not null && depth < 64; depth++)
    {
        if (serializedRoots.Contains(current.FullName))
        {
            return true;
        }
        TypeDefinition? resolved;
        try
        {
            resolved = current.Resolve();
        }
        catch
        {
            return false;
        }
        current = resolved?.BaseType;
    }
    return false;
}
