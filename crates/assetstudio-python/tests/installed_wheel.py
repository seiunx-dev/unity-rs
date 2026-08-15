"""Checks metadata and typing files from an installed wheel, not the source tree."""

import ast
import inspect

from importlib.metadata import distribution, version
from importlib.resources import files
from pathlib import Path
from typing import Union

import assetstudio


FORBIDDEN_COMPONENTS = {"assetstudio-ffi", "assetstudio-gui", "assetstudiogui"}
FORBIDDEN_SUFFIXES = (".cs", ".csproj", ".fsproj", ".sln", ".slnx", ".vbproj")


def forbidden_delivery_path(name: str) -> bool:
    lowered = name.casefold().replace("\\", "/")
    return any(part in FORBIDDEN_COMPONENTS for part in lowered.split("/")) or lowered.endswith(
        FORBIDDEN_SUFFIXES
    )


def main() -> None:
    assert assetstudio.__version__ == version("assetstudio-rs")
    assert assetstudio.__all__.count("__version__") == 1
    assert len(assetstudio.__all__) == len(set(assetstudio.__all__))
    for name in assetstudio.__all__:
        assert hasattr(assetstudio, name), name

    package = files("assetstudio")
    installed_files = distribution("assetstudio-rs").files or ()
    forbidden = [str(path) for path in installed_files if forbidden_delivery_path(str(path))]
    assert not forbidden, f"wheel contains out-of-scope GUI/C ABI/.NET files: {forbidden}"
    stub = package.joinpath("__init__.pyi")
    assert stub.is_file()
    assert package.joinpath("py.typed").is_file()
    repository = Path(__file__).resolve().parents[3]
    for legal_file in ("LICENSE", "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_LICENSES.txt"):
        packaged = package.joinpath(legal_file)
        assert packaged.is_file(), legal_file
        assert packaged.read_bytes() == repository.joinpath(legal_file).read_bytes()

    # Parse with the oldest supported grammar and prove that every documented
    # runtime export is represented in the wheel's type stub. This catches a
    # surprisingly easy release failure where the native module grows but the
    # checked-in .pyi silently falls behind.
    tree = ast.parse(stub.read_text(encoding="utf-8"), feature_version=(3, 9))
    stub_names: set[str] = set()
    stub_class_members: dict[str, set[str]] = {}
    stub_class_methods: dict[
        str, dict[str, Union[ast.FunctionDef, ast.AsyncFunctionDef]]
    ] = {}
    for node in tree.body:
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            stub_names.add(node.name)
            if isinstance(node, ast.ClassDef):
                members: set[str] = set()
                methods: dict[str, Union[ast.FunctionDef, ast.AsyncFunctionDef]] = {}
                for member in node.body:
                    if isinstance(member, (ast.FunctionDef, ast.AsyncFunctionDef)):
                        members.add(member.name)
                        methods[member.name] = member
                    elif isinstance(member, ast.AnnAssign) and isinstance(
                        member.target, ast.Name
                    ):
                        members.add(member.target.id)
                    elif isinstance(member, ast.Assign):
                        members.update(
                            target.id
                            for target in member.targets
                            if isinstance(target, ast.Name)
                        )
                stub_class_members[node.name] = members
                stub_class_methods[node.name] = methods
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            stub_names.add(node.target.id)
        elif isinstance(node, ast.Assign):
            stub_names.update(
                target.id for target in node.targets if isinstance(target, ast.Name)
            )
    missing_stub_names = sorted(set(assetstudio.__all__) - stub_names)
    assert not missing_stub_names, missing_stub_names

    # A stub-only name is worse than a missing annotation: editors accept the
    # import, then the installed package raises ImportError at runtime. Keep
    # this direction separate from __all__ so public type aliases are checked
    # even though they are not native extension classes.
    runtime_names = {name for name in dir(assetstudio) if not name.startswith("_")}
    public_stub_names = {name for name in stub_names if not name.startswith("_")}
    phantom_stub_names = sorted(public_stub_names - runtime_names)
    assert not phantom_stub_names, phantom_stub_names

    # The package-level check above is not enough for extension classes: a new
    # PyO3 method can be callable at runtime while silently remaining invisible
    # to editors and type checkers. Compare every public member of each exported
    # native class with the installed stub. Dunder protocol methods are omitted
    # because Python's base classes contribute many that the package does not
    # redeclare.
    for class_name, declared_members in stub_class_members.items():
        runtime_class = getattr(assetstudio, class_name, None)
        if not isinstance(runtime_class, type):
            continue
        runtime_members = {
            name for name in dir(runtime_class) if not name.startswith("_")
        }
        public_stub_members = {
            name for name in declared_members if not name.startswith("_")
        }
        missing_members = sorted(runtime_members - public_stub_members)
        assert not missing_members, (class_name, missing_members)
        phantom_members = sorted(public_stub_members - runtime_members)
        assert not phantom_members, (class_name, phantom_members)

    # PyO3 exposes a real inspectable signature only when the signature macro
    # contains literal defaults. A Rust const appears as `Ellipsis`, which hid
    # two stale 1-GiB model-output defaults in the stub while the extension
    # actually used 512 MiB. Compare every literal default on the main class so
    # signatures remain one contract rather than two independently edited
    # documents.
    for method_name, stub_method in stub_class_methods["AssetStudio"].items():
        if method_name.startswith("_"):
            continue
        runtime_method = getattr(assetstudio.AssetStudio, method_name)
        if not callable(runtime_method):
            continue
        signature = inspect.signature(runtime_method)
        positional = [*stub_method.args.posonlyargs, *stub_method.args.args]
        defaults: list[tuple[str, ast.expr]] = []
        if stub_method.args.defaults:
            defaults.extend(
                (argument.arg, default)
                for argument, default in zip(
                    positional[-len(stub_method.args.defaults) :],
                    stub_method.args.defaults,
                )
            )
        defaults.extend(
            (argument.arg, default)
            for argument, default in zip(
                stub_method.args.kwonlyargs,
                stub_method.args.kw_defaults,
            )
            if default is not None
        )
        for parameter_name, default_node in defaults:
            try:
                expected = ast.literal_eval(default_node)
            except (ValueError, TypeError):
                continue
            parameter = signature.parameters.get(parameter_name)
            assert parameter is not None, (method_name, parameter_name, signature)
            assert parameter.default is not inspect.Parameter.empty, (
                method_name,
                parameter_name,
                "runtime default is missing",
            )
            assert parameter.default is not Ellipsis, (
                method_name,
                parameter_name,
                "runtime default is opaque",
            )
            assert parameter.default == expected, (
                method_name,
                parameter_name,
                parameter.default,
                expected,
            )


if __name__ == "__main__":
    main()
