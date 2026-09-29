#!/usr/bin/env python3
"""Build a reproducible engineering inventory for release review.

This file deliberately does not claim to be a complete legal notice. It
records the package metadata and in-repository notice files that a release
reviewer must inspect, while keeping the source archive as the authoritative
corresponding source.
"""

from __future__ import annotations

import json
import os
import subprocess
import zipfile
from pathlib import Path, PurePosixPath


ROOT = Path(__file__).resolve().parents[2]


def cargo_packages() -> list[dict[str, str]]:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    metadata = json.loads(result.stdout)
    packages: list[dict[str, str]] = []
    for package in metadata.get("packages", []):
        source = package.get("source") or "workspace"
        packages.append(
            {
                "ecosystem": "Cargo",
                "name": str(package.get("name", "")),
                "version": str(package.get("version", "")),
                "license": str(package.get("license") or "UNSPECIFIED"),
                "source": source,
            }
        )
    return sorted(packages, key=lambda item: (item["ecosystem"], item["name"], item["version"]))


def npm_packages() -> list[dict[str, str]]:
    lock_path = ROOT / "apps" / "codex-plus-manager" / "package-lock.json"
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    packages: list[dict[str, str]] = []
    for package_path, package in lock.get("packages", {}).items():
        if not package_path or package_path == "":
            continue
        name = package_path.rsplit("node_modules/", 1)[-1]
        if name.startswith("@") and "/node_modules/" in package_path:
            name = package_path.rsplit("/node_modules/", 1)[-1]
        packages.append(
            {
                "ecosystem": "npm",
                "name": name,
                "version": str(package.get("version", "")),
                "license": str(package.get("license") or "UNSPECIFIED"),
                "source": str(package.get("resolved") or "package-lock.json"),
            }
        )
    return sorted(packages, key=lambda item: (item["ecosystem"], item["name"], item["version"]))


def tracked_checkout_files() -> list[str]:
    # The release source archive is created from the checked-out release tag.
    # Use its committed tree, not the index or untracked build/staging copies.
    result = subprocess.run(
        ["git", "ls-tree", "-r", "--name-only", "-z", "HEAD"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    return [path for path in result.stdout.split("\0") if path]


def is_notice_name(name: str) -> bool:
    return name in {"license", "notice", "copying"} or name.startswith(
        ("license.", "notice.", "copying.")
    )


def repository_notices() -> list[Path]:
    files: list[Path] = []
    for relative_path in tracked_checkout_files():
        path = ROOT / relative_path
        if is_notice_name(path.name.lower()):
            if not path.exists():
                raise FileNotFoundError(f"tracked notice missing from checkout: {relative_path}")
            files.append(path)
    return sorted(files, key=lambda path: path.relative_to(ROOT).as_posix())


def repository_asset_candidates() -> list[Path]:
    # This intentionally over-includes tracked source assets. Build-time includes,
    # frontend bundling, and installer conversion can all place them in artifacts;
    # the release reviewer must check the actual package separately.
    prefixes = (
        "assets/",
        "docs/images/",
        "apps/codex-plus-manager/src/assets/",
        "apps/codex-plus-manager/src-tauri/icons/",
    )
    files: list[Path] = []
    for relative_path in tracked_checkout_files():
        if not relative_path.startswith(prefixes):
            continue
        path = ROOT / relative_path
        if is_notice_name(path.name.lower()):
            continue
        if not path.exists():
            raise FileNotFoundError(f"tracked asset missing from checkout: {relative_path}")
        files.append(path)
    return sorted(files, key=lambda path: path.relative_to(ROOT).as_posix())


def embedded_archive_notices() -> list[str]:
    notices: list[str] = []
    for path in repository_asset_candidates():
        if path.suffix.lower() != ".zip":
            continue
        with zipfile.ZipFile(path) as archive:
            for member in archive.infolist():
                if member.is_dir() or not is_notice_name(
                    PurePosixPath(member.filename).name.lower()
                ):
                    continue
                notices.append(f"{path.relative_to(ROOT).as_posix()}!/{member.filename}")
    return sorted(notices)


def main() -> None:
    packages = cargo_packages() + npm_packages()
    notices = repository_notices()
    asset_candidates = repository_asset_candidates()
    archive_notices = embedded_archive_notices()
    output = os.environ.get("THIRD_PARTY_NOTICES_OUTPUT")
    output_path = Path(output) if output else ROOT / "THIRD-PARTY-NOTICES.md"
    if not output_path.is_absolute():
        output_path = ROOT / output_path
    output_path.parent.mkdir(parents=True, exist_ok=True)

    lines = [
        "# Third-party notices inventory",
        "",
        "This file is generated during release preparation from the locked Cargo and npm metadata.",
        "It is an engineering inventory for review, not a complete legal notice or legal opinion.",
        "Review every `UNSPECIFIED` entry and every bundled asset before publishing.",
        "",
        "## In-repository notice files",
        "",
    ]
    if notices:
        lines.extend(f"- `{path.relative_to(ROOT).as_posix()}`" for path in notices)
    else:
        lines.append("- None found outside generated output.")

    lines.extend(["", "## Notice files inside tracked ZIP assets", ""])
    if archive_notices:
        lines.extend(f"- `{path}`" for path in archive_notices)
    else:
        lines.append("- None found in tracked ZIP assets.")

    lines.extend(
        [
            "",
            "## Tracked in-repository asset review candidates",
            "",
            "These source-tree paths may be embedded, bundled, or converted by build and installer steps.",
            "This is an over-inclusive review checklist, not a list of actual binary contents or license conclusions.",
            "Compare the built artifacts and packaging scripts before release.",
            "",
        ]
    )
    if asset_candidates:
        lines.extend(f"- `{path.relative_to(ROOT).as_posix()}`" for path in asset_candidates)
    else:
        lines.append("- None found in the configured asset directories.")

    lines.extend(["", "## Locked dependency inventory", "", "| Ecosystem | Package | Version | License metadata | Source |", "| --- | --- | --- | --- | --- |"])
    for package in packages:
        lines.append(
            "| {ecosystem} | `{name}` | `{version}` | `{license}` | `{source}` |".format(**package)
        )
    lines.extend(
        [
            "",
            "## Release review gates",
            "",
            "- Confirm each package license and bundled asset notice against its upstream source.",
            "- Add or correct notices before release if metadata is `UNSPECIFIED` or incomplete.",
            "- Keep this inventory with the corresponding source archive; it does not replace the root AGPL license or a written source offer.",
            "",
        ]
    )
    output_path.write_text("\n".join(lines), encoding="utf-8")


if __name__ == "__main__":
    main()
