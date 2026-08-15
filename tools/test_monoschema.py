#!/usr/bin/env python3
"""Builds a tiny managed assembly and verifies both schema scoping modes."""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
GENERATOR = ROOT / "tools" / "monoschema" / "MonoSchemaGenerator.csproj"
UNITY_VERSION = "2022.3.62f1"


def run(command: list[str], *, cwd: Path = ROOT) -> None:
    result = subprocess.run(command, cwd=cwd, capture_output=True, text=True)
    if result.returncode == 0:
        return
    output = (result.stderr or result.stdout).strip()
    raise RuntimeError(f"{' '.join(command)} failed:\n{output}")


def managed_repo() -> Path:
    configured = os.environ.get("ASSETSTUDIO_REPO")
    repository = Path(configured) if configured else ROOT.parent / "AssetStudio"
    required = repository / "AssetStudio" / "AssetStudio.csproj"
    if not required.is_file():
        raise RuntimeError(
            "the schema generator needs the managed AssetStudio oracle; "
            f"looked for {required}. Set ASSETSTUDIO_REPO to its checkout"
        )
    return repository.resolve()


def write_fixture(directory: Path) -> Path:
    directory.mkdir(parents=True)
    project = directory / "SchemaFixture.csproj"
    project.write_text(
        """<Project Sdk=\"Microsoft.NET.Sdk\">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <AssemblyName>SchemaFixture</AssemblyName>
    <Nullable>enable</Nullable>
  </PropertyGroup>
</Project>
""",
        encoding="utf-8",
    )
    (directory / "Fixture.cs").write_text(
        """namespace UnityEngine
{
    public class Object { }
    public class MonoBehaviour : Object { }
    public class ScriptableObject : Object { }
}

namespace Fixture
{
    public sealed class Counter : UnityEngine.MonoBehaviour
    {
        public int Value;
    }
}
""",
        encoding="utf-8",
    )
    run(["dotnet", "build", str(project), "-c", "Release", "--nologo"])
    return directory / "bin" / "Release" / "net10.0"


def generate(
    assembly_directory: Path,
    output: Path,
    oracle: Path,
    *extra: str,
) -> dict[str, object]:
    run(
        [
            "dotnet",
            "run",
            "--project",
            str(GENERATOR),
            "--configuration",
            "Release",
            "--no-restore",
            f"-p:AssetStudioRepo={oracle}",
            "--",
            str(assembly_directory),
            UNITY_VERSION,
            str(output),
            "--assembly",
            "SchemaFixture",
            *extra,
        ]
    )
    return json.loads(output.read_text(encoding="utf-8"))


def entries(document: dict[str, object]) -> list[dict[str, object]]:
    assert document["version"] == 1, document
    assert document["generated_for"] == UNITY_VERSION, document
    value = document["entries"]
    assert isinstance(value, list) and value, document
    assert all(isinstance(entry, dict) for entry in value), document
    return value


def main() -> int:
    oracle = managed_repo()
    run(
        [
            "dotnet",
            "restore",
            str(GENERATOR),
            "--ignore-failed-sources",
            "-p:NuGetAudit=false",
            f"-p:AssetStudioRepo={oracle}",
        ]
    )
    with tempfile.TemporaryDirectory(prefix="assetstudio-monoschema-") as temporary:
        root = Path(temporary)
        assembly_directory = write_fixture(root / "fixture")

        versioned = entries(
            generate(assembly_directory, root / "versioned.json", oracle)
        )
        assert all(entry.get("unity_version") == UNITY_VERSION for entry in versioned)
        assert any(
            entry.get("namespace") == "Fixture" and entry.get("class") == "Counter"
            for entry in versioned
        ), versioned

        unversioned = entries(
            generate(
                assembly_directory,
                root / "unversioned.json",
                oracle,
                "--unversioned",
            )
        )
        assert all("unity_version" not in entry for entry in unversioned), unversioned

    print("MonoBehaviour schema generator scopes entries by version by default")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
