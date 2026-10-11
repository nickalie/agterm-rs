#!/usr/bin/env python3
"""Fetch a released upstream agterm for reference-macos: download, verify, extract, cache by tag.

Prints `bundle=<agterm.app>`, `ctl=<agtermctl>` and `archs=<arch,...>` lines (the form `$GITHUB_OUTPUT` takes),
or one JSON object with `--json`. A cached tag answers without the network.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import struct
import subprocess
import sys
import tempfile
import urllib.request
from collections.abc import Callable, Mapping
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

DEFAULT_REPO = "umputun/agterm"
BUNDLE = "agterm.app"
CTL = Path("Contents/MacOS/agtermctl")
EXECUTABLE = Path("Contents/MacOS/agterm")
MARKER = "reference.json"
API = "https://api.github.com/repos/{repo}/releases/tags/{tag}"

# Mach-O magic and CPU types (mach/machine.h); a fat header is always big-endian.
_FAT_MAGICS = {0xCAFEBABE: ">2I", 0xCAFEBABF: ">2Q"}
_THIN_MAGICS = {b"\xcf\xfa\xed\xfe": "<", b"\xce\xfa\xed\xfe": "<", b"\xfe\xed\xfa\xcf": ">", b"\xfe\xed\xfa\xce": ">"}
_CPU_TYPES = {0x01000007: "x86_64", 0x0100000C: "arm64", 7: "i386", 12: "arm"}


class FetchError(Exception):
    pass


@dataclass(frozen=True)
class Reference:
    tag: str
    asset: str
    digest: str | None
    bundle: str
    ctl: str
    archs: list[str]


def default_cache() -> Path:
    base = os.environ.get("XDG_CACHE_HOME") or os.path.join(os.path.expanduser("~"), ".cache")
    return Path(base) / "agterm-conformance" / "reference"


def select_asset(release: Mapping[str, Any], tag: str) -> Mapping[str, Any]:
    """The release's app image: `agterm-<version>.dmg` by name, else its only `.dmg`, else its only `.zip`."""
    if release.get("draft"):
        raise FetchError(f"release {tag} is a draft")
    assets = [asset for asset in release.get("assets") or [] if isinstance(asset, Mapping)]
    version = tag.removeprefix("v")
    for asset in assets:
        if asset.get("name") == f"agterm-{version}.dmg":
            return asset
    names = [str(asset.get("name", "")) for asset in assets]
    for suffix in (".dmg", ".zip"):
        candidates = [asset for asset in assets if str(asset.get("name", "")).endswith(suffix)]
        if len(candidates) == 1:
            return candidates[0]
        if len(candidates) > 1:
            raise FetchError(f"release {tag} has several {suffix} assets, none named agterm-{version}.dmg: {names}")
    raise FetchError(f"release {tag} has no .dmg or .zip asset: {names}")


def expected_digest(asset: Mapping[str, Any]) -> str | None:
    """The hex sha256 GitHub records for the asset, when it does."""
    digest = asset.get("digest")
    if not digest:
        return None
    algorithm, _, value = str(digest).partition(":")
    if algorithm != "sha256" or len(value) != 64:
        raise FetchError(f"unsupported asset digest {digest!r}")
    return value.lower()


def macho_archs(path: Path) -> list[str]:
    """CPU architectures in a thin or universal Mach-O, read from its header."""
    with open(path, "rb") as source:
        head = source.read(8)
        if len(head) < 8:
            raise FetchError(f"{path} is not a Mach-O file")
        magic = struct.unpack(">I", head[:4])[0]
        if magic in _FAT_MAGICS:
            count = struct.unpack(">I", head[4:8])[0]
            width = 32 if magic == 0xCAFEBABF else 20
            entries = source.read(count * width)
            if count == 0 or len(entries) < count * width:
                raise FetchError(f"{path} has a truncated universal header")
            types = [struct.unpack(">i", entries[i * width : i * width + 4])[0] & 0xFFFFFFFF for i in range(count)]
        elif head[:4] in _THIN_MAGICS:
            types = [struct.unpack(_THIN_MAGICS[head[:4]] + "I", head[4:8])[0]]
        else:
            raise FetchError(f"{path} is not a Mach-O file")
    return [_CPU_TYPES.get(cpu, f"cpu-{cpu:#x}") for cpu in types]


def host_arch() -> str:
    machine = platform.machine()
    return {"aarch64": "arm64", "amd64": "x86_64"}.get(machine.lower(), machine)


def read_cached(cache: Path, tag: str) -> Reference | None:
    """A completed fetch of `tag`, or None when the cache has no usable copy."""
    try:
        recorded = json.loads((cache / tag / MARKER).read_text())
        reference = Reference(**recorded)
    except (OSError, ValueError, TypeError):
        return None
    if reference.tag != tag or not Path(reference.bundle, EXECUTABLE).is_file() or not Path(reference.ctl).is_file():
        return None
    return reference


def fetch_release(repo: str, tag: str) -> dict[str, Any]:
    request = urllib.request.Request(API.format(repo=repo, tag=tag), headers={"Accept": "application/vnd.github+json"})
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.load(response)


def download(url: str, target: Path) -> None:
    with urllib.request.urlopen(url, timeout=300) as response, open(target, "wb") as sink:
        shutil.copyfileobj(response, sink)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for block in iter(lambda: source.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def extract(image: Path, destination: Path) -> None:
    """Copy `agterm.app` out of a .dmg or .zip into `destination` (macOS: hdiutil/ditto keep the signature)."""
    if sys.platform != "darwin":
        raise FetchError("extracting the app needs macOS (hdiutil/ditto)")
    if image.suffix == ".zip":
        subprocess.run(["ditto", "-x", "-k", os.fspath(image), os.fspath(destination)], check=True)
        return
    mount = Path(tempfile.mkdtemp(prefix="agt-dmg-"))
    subprocess.run(
        ["hdiutil", "attach", "-nobrowse", "-readonly", "-noverify", "-mountpoint", os.fspath(mount), os.fspath(image)],
        check=True,
        stdout=subprocess.DEVNULL,
    )
    try:
        subprocess.run(["ditto", os.fspath(mount / BUNDLE), os.fspath(destination / BUNDLE)], check=True)
    finally:
        subprocess.run(["hdiutil", "detach", os.fspath(mount)], check=False, stdout=subprocess.DEVNULL)
        shutil.rmtree(mount, ignore_errors=True)


def verify_signature(bundle: Path) -> None:
    if sys.platform == "darwin":
        result = subprocess.run(
            ["codesign", "--verify", "--deep", "--strict", os.fspath(bundle)],
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            raise FetchError(f"{bundle} fails codesign verification: {result.stderr.strip()}")


def fetch(
    tag: str,
    cache: Path,
    repo: str = DEFAULT_REPO,
    refresh: bool = False,
    release: Mapping[str, Any] | None = None,
    get_release: Callable[[str, str], Mapping[str, Any]] | None = None,
    get_file: Callable[[str, Path], None] | None = None,
    unpack: Callable[[Path, Path], None] | None = None,
    verify: Callable[[Path], None] | None = None,
) -> Reference:
    """The cached reference for `tag`, fetching it first when absent (or when `refresh`)."""
    if not refresh and (cached := read_cached(cache, tag)) is not None:
        return cached
    get_file, unpack, verify = get_file or download, unpack or extract, verify or verify_signature
    release = release if release is not None else (get_release or fetch_release)(repo, tag)
    if release.get("tag_name") not in (None, tag):
        raise FetchError(f"asked for {tag}, the release is {release.get('tag_name')}")
    asset = select_asset(release, tag)
    name, url = str(asset["name"]), str(asset.get("browser_download_url") or "")
    if not url or os.sep in name or name in (".", ".."):
        raise FetchError(f"asset {name!r} has no usable download url")
    want = expected_digest(asset)
    cache.mkdir(parents=True, exist_ok=True)
    # Built beside the final directory and swapped in whole, so an interrupted fetch leaves no half copy.
    work = Path(tempfile.mkdtemp(prefix=f".{tag}-", dir=cache))
    try:
        image = work / name
        get_file(url, image)
        if want is not None and (got := sha256(image)) != want:
            raise FetchError(f"{name}: sha256 {got} does not match the release digest {want}")
        unpacked = work / "app"
        unpacked.mkdir()
        unpack(image, unpacked)
        image.unlink()
        if not (unpacked / BUNDLE / EXECUTABLE).is_file():
            raise FetchError(f"{name} holds no {BUNDLE}/{EXECUTABLE}")
        verify(unpacked / BUNDLE)
        final = cache / tag
        reference = Reference(
            tag=tag,
            asset=name,
            digest=want,
            bundle=os.fspath(final / BUNDLE),
            ctl=os.fspath(final / BUNDLE / CTL),
            archs=macho_archs(unpacked / BUNDLE / EXECUTABLE),
        )
        (unpacked / MARKER).write_text(json.dumps(asdict(reference), indent=2) + "\n")
        shutil.rmtree(final, ignore_errors=True)
        unpacked.rename(final)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    return reference


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--tag", required=True, help="release tag, e.g. v0.36.0")
    parser.add_argument("--repo", default=DEFAULT_REPO)
    parser.add_argument(
        "--cache", type=Path, default=None, help="cache root (default: ~/.cache/agterm-conformance/reference)"
    )
    parser.add_argument("--release-json", type=Path, help="use this release JSON instead of the GitHub API")
    parser.add_argument("--refresh", action="store_true", help="fetch again even when the tag is cached")
    parser.add_argument("--json", action="store_true", help="print one JSON object")
    parser.add_argument("--allow-foreign-arch", action="store_true", help="do not fail when the host cannot run it")
    args = parser.parse_args(argv)
    release = json.loads(args.release_json.read_text()) if args.release_json else None
    try:
        reference = fetch(args.tag, args.cache or default_cache(), args.repo, args.refresh, release)
    except (FetchError, OSError, subprocess.CalledProcessError) as error:
        print(f"fetch_reference: {error}", file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(asdict(reference)))
    else:
        print(f"bundle={reference.bundle}\nctl={reference.ctl}\narchs={','.join(reference.archs)}")
    # Rosetta runs x86_64 on arm64, never the reverse: an arm64-only build cannot start on an Intel Mac.
    runnable = host_arch() in reference.archs or (host_arch() == "arm64" and "x86_64" in reference.archs)
    if sys.platform == "darwin" and not runnable and not args.allow_foreign_arch:
        print(
            f"fetch_reference: {args.tag} is {'/'.join(reference.archs)}, this host is {host_arch()}", file=sys.stderr
        )
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
