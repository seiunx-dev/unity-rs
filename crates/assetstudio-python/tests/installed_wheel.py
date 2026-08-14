"""Checks metadata and typing files from an installed wheel, not the source tree."""

import ast

from importlib.metadata import version
from importlib.resources import files

import assetstudio


def main() -> None:
    assert assetstudio.__version__ == version("assetstudio-rs")
    assert assetstudio.__all__.count("__version__") == 1
    assert len(assetstudio.__all__) == len(set(assetstudio.__all__))
    for name in assetstudio.__all__:
        assert hasattr(assetstudio, name), name

    package = files("assetstudio")
    stub = package.joinpath("__init__.pyi")
    assert stub.is_file()
    assert package.joinpath("py.typed").is_file()

    # Parse with the oldest supported grammar and prove that every documented
    # runtime export is represented in the wheel's type stub. This catches a
    # surprisingly easy release failure where the native module grows but the
    # checked-in .pyi silently falls behind.
    tree = ast.parse(stub.read_text(encoding="utf-8"), feature_version=(3, 9))
    stub_names: set[str] = set()
    stub_class_members: dict[str, set[str]] = {}
    for node in tree.body:
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            stub_names.add(node.name)
            if isinstance(node, ast.ClassDef):
                members: set[str] = set()
                for member in node.body:
                    if isinstance(member, (ast.FunctionDef, ast.AsyncFunctionDef)):
                        members.add(member.name)
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
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            stub_names.add(node.target.id)
        elif isinstance(node, ast.Assign):
            stub_names.update(
                target.id for target in node.targets if isinstance(target, ast.Name)
            )
    missing_stub_names = sorted(set(assetstudio.__all__) - stub_names)
    assert not missing_stub_names, missing_stub_names

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


if __name__ == "__main__":
    main()
