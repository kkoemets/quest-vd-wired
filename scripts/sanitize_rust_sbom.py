#!/usr/bin/env python3
"""Make cargo-cyclonedx output reproducible and safe to publish."""

from __future__ import annotations

import argparse
import copy
import json
import os
import re
import sys
import tempfile
import tomllib
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence
from urllib.parse import quote


EXPECTED_BOM_FORMAT = "CycloneDX"
EXPECTED_SPEC_VERSION = "1.5"
PRODUCT_COMPONENT_NAME = "gnirehtet-vd"
MAXIMUM_INPUT_BYTES = 32 * 1024 * 1024
GENERIC_PRIVATE_PATH_PATTERNS = (
    re.compile(r"/Users/", re.IGNORECASE),
    re.compile(r"/private/var/", re.IGNORECASE),
    re.compile(r"/home/", re.IGNORECASE),
    re.compile(r"/tmp/", re.IGNORECASE),
    re.compile(r"path\+file:", re.IGNORECASE),
    re.compile(r"file://", re.IGNORECASE),
    re.compile(r"(?:^|[^A-Za-z0-9])[A-Za-z]:[\\/]"),
)


class SanitizationError(ValueError):
    """Raised when an SBOM is unsafe or structurally invalid."""


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise SanitizationError(f"duplicate JSON object key: {key}")
        result[key] = value
    return result


def _reject_constant(value: str) -> None:
    raise SanitizationError(f"non-finite JSON number: {value}")


def load_document(path: Path) -> dict[str, Any]:
    try:
        size = path.stat().st_size
    except OSError as error:
        raise SanitizationError(f"cannot inspect input SBOM: {error}") from error
    if size > MAXIMUM_INPUT_BYTES:
        raise SanitizationError(
            f"input SBOM exceeds the {MAXIMUM_INPUT_BYTES}-byte safety limit"
        )
    try:
        document = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=_unique_object,
            parse_constant=_reject_constant,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise SanitizationError(f"cannot read input SBOM: {error}") from error
    if not isinstance(document, dict):
        raise SanitizationError("CycloneDX document must be a JSON object")
    return document


def _workspace_product(manifest_path: Path) -> tuple[str, str]:
    try:
        workspace_manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise SanitizationError(f"cannot read Rust workspace manifest: {error}") from error

    workspace = workspace_manifest.get("workspace")
    if not isinstance(workspace, dict):
        raise SanitizationError("Rust manifest does not define a workspace")
    members = workspace.get("members")
    workspace_package = workspace.get("package")
    if not isinstance(members, list) or not all(isinstance(member, str) for member in members):
        raise SanitizationError("Rust workspace members are missing or invalid")
    if not isinstance(workspace_package, dict):
        raise SanitizationError("Rust workspace package metadata is missing")
    workspace_version = workspace_package.get("version")
    if not isinstance(workspace_version, str) or not workspace_version:
        raise SanitizationError("Rust workspace version is missing or invalid")

    matches: list[tuple[str, str]] = []
    for member in members:
        member_manifest = manifest_path.parent / member / "Cargo.toml"
        try:
            member_document = tomllib.loads(member_manifest.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
            raise SanitizationError(f"cannot read workspace member {member}: {error}") from error
        package = member_document.get("package")
        if not isinstance(package, dict):
            continue
        name = package.get("name")
        if name != PRODUCT_COMPONENT_NAME:
            continue
        version_value = package.get("version")
        if isinstance(version_value, str):
            version = version_value
        elif isinstance(version_value, dict) and version_value.get("workspace") is True:
            version = workspace_version
        else:
            raise SanitizationError(
                f"{PRODUCT_COMPONENT_NAME} does not have a resolvable package version"
            )
        matches.append((name, version))

    if len(matches) != 1:
        raise SanitizationError(
            f"expected exactly one {PRODUCT_COMPONENT_NAME} workspace package, found {len(matches)}"
        )
    return matches[0]


def _required_dictionary(parent: Mapping[str, Any], key: str) -> dict[str, Any]:
    value = parent.get(key)
    if not isinstance(value, dict):
        raise SanitizationError(f"CycloneDX field {key!r} must be an object")
    return value


def _required_string(parent: Mapping[str, Any], key: str, context: str) -> str:
    value = parent.get(key)
    if not isinstance(value, str) or not value:
        raise SanitizationError(f"{context} field {key!r} must be a non-empty string")
    return value


def _component_references(document: Mapping[str, Any]) -> tuple[str, set[str]]:
    metadata = _required_dictionary(document, "metadata")
    root = _required_dictionary(metadata, "component")
    root_ref = _required_string(root, "bom-ref", "root component")
    components = document.get("components")
    if not isinstance(components, list):
        raise SanitizationError("CycloneDX components must be an array")

    references = {root_ref}
    for index, component in enumerate(components):
        if not isinstance(component, dict):
            raise SanitizationError(f"component {index} must be an object")
        reference = _required_string(component, "bom-ref", f"component {index}")
        if reference in references:
            raise SanitizationError(f"duplicate component reference: {reference}")
        references.add(reference)
    return root_ref, references


def _dependency_graph(document: Mapping[str, Any]) -> dict[str, tuple[str, ...]]:
    _root_ref, component_refs = _component_references(document)
    dependencies = document.get("dependencies")
    if not isinstance(dependencies, list):
        raise SanitizationError("CycloneDX dependencies must be an array")

    graph: dict[str, tuple[str, ...]] = {}
    for index, entry in enumerate(dependencies):
        if not isinstance(entry, dict):
            raise SanitizationError(f"dependency entry {index} must be an object")
        reference = _required_string(entry, "ref", f"dependency entry {index}")
        depends_on = entry.get("dependsOn", [])
        if not isinstance(depends_on, list) or not all(
            isinstance(dependency, str) and dependency for dependency in depends_on
        ):
            raise SanitizationError(f"dependency entry {reference} has an invalid dependsOn array")
        if len(depends_on) != len(set(depends_on)):
            raise SanitizationError(f"dependency entry {reference} contains duplicate edges")
        if reference in graph:
            raise SanitizationError(f"duplicate dependency entry: {reference}")
        graph[reference] = tuple(sorted(depends_on))

    graph_refs = set(graph)
    if graph_refs != component_refs:
        missing = sorted(component_refs - graph_refs)
        unknown = sorted(graph_refs - component_refs)
        raise SanitizationError(
            f"dependency graph is not closed (missing={missing}, unknown={unknown})"
        )
    dangling = sorted(
        dependency
        for dependencies_for_ref in graph.values()
        for dependency in dependencies_for_ref
        if dependency not in component_refs
    )
    if dangling:
        raise SanitizationError(f"dependency graph contains unknown references: {dangling}")
    return graph


def _replace_reference_strings(value: Any, replacements: Mapping[str, str]) -> Any:
    if isinstance(value, dict):
        return {key: _replace_reference_strings(child, replacements) for key, child in value.items()}
    if isinstance(value, list):
        return [_replace_reference_strings(child, replacements) for child in value]
    if isinstance(value, str):
        return replacements.get(value, value)
    return value


def _stable_product_purl(name: str, version: str) -> str:
    return f"pkg:cargo/{quote(name, safe='-._~')}@{quote(version, safe='-._~')}"


def _target_reference(root_purl: str, component: Mapping[str, Any]) -> str:
    component_type = _required_string(component, "type", "nested target component")
    component_name = _required_string(component, "name", "nested target component")
    identity = quote(f"{component_type}:{component_name}", safe="-._~")
    return f"{root_purl}?target={identity}"


def _normalize_order(document: dict[str, Any]) -> None:
    components = document["components"]
    assert isinstance(components, list)
    components.sort(key=lambda component: component["bom-ref"])

    metadata = document["metadata"]
    assert isinstance(metadata, dict)
    root = metadata["component"]
    assert isinstance(root, dict)
    targets = root.get("components")
    if isinstance(targets, list):
        targets.sort(key=lambda component: component["bom-ref"])

    dependencies = document["dependencies"]
    assert isinstance(dependencies, list)
    for entry in dependencies:
        assert isinstance(entry, dict)
        depends_on = entry.setdefault("dependsOn", [])
        assert isinstance(depends_on, list)
        depends_on.sort()
    dependencies.sort(key=lambda entry: entry["ref"])


def _walk_strings(value: Any, location: str = "$") -> Iterable[tuple[str, str]]:
    if isinstance(value, dict):
        for key, child in value.items():
            yield f"{location}.<key>", key
            yield from _walk_strings(child, f"{location}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from _walk_strings(child, f"{location}[{index}]")
    elif isinstance(value, str):
        yield location, value


def _assert_publishable(document: Mapping[str, Any], repository_root: Path) -> None:
    repository_paths = {
        str(repository_root.resolve()),
        str(repository_root.resolve()).replace("/", "\\"),
    }
    for location, value in _walk_strings(document):
        for pattern in GENERIC_PRIVATE_PATH_PATTERNS:
            if pattern.search(value):
                raise SanitizationError(f"private path reference remains at {location}")
        folded_value = value.casefold()
        for repository_path in repository_paths:
            if repository_path and repository_path.casefold() in folded_value:
                raise SanitizationError(f"repository path remains at {location}")


def sanitize_document(
    source: Mapping[str, Any], manifest_path: Path, repository_root: Path
) -> dict[str, Any]:
    document = copy.deepcopy(source)
    if document.get("bomFormat") != EXPECTED_BOM_FORMAT:
        raise SanitizationError("input is not a CycloneDX document")
    if document.get("specVersion") != EXPECTED_SPEC_VERSION:
        raise SanitizationError(
            f"expected CycloneDX {EXPECTED_SPEC_VERSION}, found {document.get('specVersion')!r}"
        )
    version = document.get("version")
    if isinstance(version, bool) or not isinstance(version, int) or version < 1:
        raise SanitizationError("CycloneDX document version must be a positive integer")

    product_name, product_version = _workspace_product(manifest_path)
    metadata = _required_dictionary(document, "metadata")
    root = _required_dictionary(metadata, "component")
    if root.get("name") != product_name or root.get("version") != product_version:
        raise SanitizationError(
            "CycloneDX root component does not match the Rust host package identity"
        )
    input_root_ref, input_component_refs = _component_references(document)
    input_graph = _dependency_graph(document)

    stable_root_ref = _stable_product_purl(product_name, product_version)
    replacements: dict[str, str] = {input_root_ref: stable_root_ref}
    targets = root.get("components", [])
    if not isinstance(targets, list):
        raise SanitizationError("root component targets must be an array")
    input_target_refs: set[str] = set()
    stable_target_refs: set[str] = set()
    for index, target in enumerate(targets):
        if not isinstance(target, dict):
            raise SanitizationError(f"root target component {index} must be an object")
        input_target_ref = _required_string(target, "bom-ref", f"root target component {index}")
        if input_target_ref in input_component_refs or input_target_ref in input_target_refs:
            raise SanitizationError(f"duplicate root target reference: {input_target_ref}")
        input_target_refs.add(input_target_ref)
        if target.get("version") != product_version:
            raise SanitizationError(
                f"root target component {index} does not match the Rust host version"
            )
        stable_target_ref = _target_reference(stable_root_ref, target)
        if stable_target_ref in stable_target_refs:
            raise SanitizationError(f"duplicate root target identity: {stable_target_ref}")
        stable_target_refs.add(stable_target_ref)
        replacements[input_target_ref] = stable_target_ref

    document = _replace_reference_strings(document, replacements)
    metadata = _required_dictionary(document, "metadata")
    root = _required_dictionary(metadata, "component")
    root["bom-ref"] = stable_root_ref
    root["purl"] = stable_root_ref
    targets = root.get("components", [])
    assert isinstance(targets, list)
    for target in targets:
        assert isinstance(target, dict)
        target["purl"] = target["bom-ref"]

    document.pop("serialNumber", None)
    metadata.pop("timestamp", None)
    _normalize_order(document)

    expected_graph = {
        replacements.get(reference, reference): tuple(
            sorted(replacements.get(dependency, dependency) for dependency in dependencies)
        )
        for reference, dependencies in input_graph.items()
    }
    output_graph = _dependency_graph(document)
    if output_graph != expected_graph:
        raise SanitizationError("dependency graph changed while sanitizing the SBOM")
    output_metadata = _required_dictionary(document, "metadata")
    output_root = _required_dictionary(output_metadata, "component")
    if (
        output_root.get("name") != product_name
        or output_root.get("version") != product_version
        or output_root.get("bom-ref") != stable_root_ref
        or output_root.get("purl") != stable_root_ref
    ):
        raise SanitizationError("sanitized Rust host component identity is invalid")
    _assert_publishable(document, repository_root)
    return document


def render_document(document: Mapping[str, Any]) -> bytes:
    try:
        text = json.dumps(
            document,
            ensure_ascii=False,
            allow_nan=False,
            indent=2,
            sort_keys=True,
        )
    except (TypeError, ValueError) as error:
        raise SanitizationError(f"cannot serialize sanitized SBOM: {error}") from error
    return f"{text}\n".encode("utf-8")


def write_atomic(output_path: Path, content: bytes) -> None:
    try:
        output_path.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(
            mode="wb",
            prefix=f".{output_path.name}.",
            suffix=".tmp",
            dir=output_path.parent,
            delete=False,
        ) as temporary:
            temporary_path = Path(temporary.name)
            temporary.write(content)
            temporary.flush()
            os.fsync(temporary.fileno())
        try:
            os.replace(temporary_path, output_path)
        except BaseException:
            temporary_path.unlink(missing_ok=True)
            raise
    except OSError as error:
        raise SanitizationError(f"cannot write sanitized SBOM: {error}") from error


def sanitize_file(
    input_path: Path, output_path: Path, manifest_path: Path, repository_root: Path
) -> None:
    document = load_document(input_path)
    sanitized = sanitize_document(document, manifest_path, repository_root)
    content = render_document(sanitized)
    # Re-parse the exact bytes that will be published before replacing the output.
    reparsed = json.loads(content, object_pairs_hook=_unique_object, parse_constant=_reject_constant)
    if reparsed != sanitized:
        raise SanitizationError("serialized SBOM did not round-trip exactly")
    _assert_publishable(reparsed, repository_root)
    write_atomic(output_path, content)


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path, help="raw cargo-cyclonedx JSON")
    parser.add_argument("output", type=Path, help="publishable CycloneDX JSON")
    parser.add_argument("--manifest", required=True, type=Path, help="Rust workspace manifest")
    parser.add_argument(
        "--repository-root", required=True, type=Path, help="repository path that must not leak"
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        sanitize_file(args.input, args.output, args.manifest, args.repository_root)
    except SanitizationError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
