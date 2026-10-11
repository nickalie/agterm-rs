import hashlib
import importlib.util
import json
import struct
import sys
from pathlib import Path
from types import ModuleType
from typing import Any

import pytest

TOOLS = Path(__file__).resolve().parent.parent / "tools"
FIXTURE = Path(__file__).with_name("fixtures") / "release-v0.36.0.json"
IMAGE = b"not really a disk image"
ARM64 = struct.pack("<II", 0xFEEDFACF, 0x0100000C) + bytes(24)
X86_64 = struct.pack("<II", 0xFEEDFACF, 0x01000007) + bytes(24)


def _load(name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, TOOLS / f"{name}.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module  # dataclasses resolve their module by name
    spec.loader.exec_module(module)
    return module


fr = _load("fetch_reference")


def release(digest: bytes | None = IMAGE, **overrides: Any) -> dict[str, Any]:
    data = json.loads(FIXTURE.read_text())
    if digest is not None:
        data["assets"][0]["digest"] = "sha256:" + hashlib.sha256(digest).hexdigest()
    data.update(overrides)
    return data


def asset(name: str) -> dict[str, str]:
    return {"name": name, "browser_download_url": f"https://example.invalid/{name}"}


class Network:
    """Records what a fetch asked for; refuses everything when `offline`."""

    def __init__(self, offline: bool = False, executable: bytes = ARM64, bundle: bool = True) -> None:
        self.offline, self.executable, self.bundle = offline, executable, bundle
        self.files: list[str] = []
        self.unpacked: list[Path] = []
        self.verified: list[Path] = []

    def get_release(self, repo: str, tag: str) -> dict[str, Any]:
        raise AssertionError(f"release API called for {repo} {tag}")

    def get_file(self, url: str, target: Path) -> None:
        assert not self.offline, f"download of {url} while offline"
        self.files.append(url)
        target.write_bytes(IMAGE)

    def unpack(self, image: Path, destination: Path) -> None:
        self.unpacked.append(image)
        if self.bundle:
            macos = destination / "agterm.app" / "Contents" / "MacOS"
            macos.mkdir(parents=True)
            (macos / "agterm").write_bytes(self.executable)
            (macos / "agtermctl").write_bytes(self.executable)

    def verify(self, bundle: Path) -> None:
        self.verified.append(bundle)

    def fetch(self, cache: Path, data: dict[str, Any] | None = None, **kwargs: Any) -> Any:
        return fr.fetch(
            "v0.36.0",
            cache,
            release=data,
            get_release=self.get_release,
            get_file=self.get_file,
            unpack=self.unpack,
            verify=self.verify,
            **kwargs,
        )


def test_fixture_release_selects_the_versioned_dmg() -> None:
    chosen = fr.select_asset(json.loads(FIXTURE.read_text()), "v0.36.0")
    assert chosen["name"] == "agterm-0.36.0.dmg"
    assert fr.expected_digest(chosen) == "aa09d955b5c1ebafaa61f169febe582ad2f6189774efe2ea582cd74da04f36bf"


@pytest.mark.parametrize(
    ("names", "chosen"),
    [
        (["agterm-0.36.0.dmg.sha256", "other.dmg", "agterm-0.36.0.dmg"], "agterm-0.36.0.dmg"),
        (["notes.txt", "agterm.dmg"], "agterm.dmg"),
        (["notes.txt", "agterm.zip"], "agterm.zip"),
        (["agterm.zip", "agterm.dmg"], "agterm.dmg"),
    ],
)
def test_asset_selection_prefers_the_versioned_dmg_then_a_single_image(names: list[str], chosen: str) -> None:
    assert fr.select_asset({"assets": [asset(name) for name in names]}, "v0.36.0")["name"] == chosen


@pytest.mark.parametrize(
    ("data", "message"),
    [
        ({"assets": [asset("a.dmg"), asset("b.dmg")]}, "several .dmg assets"),
        ({"assets": [asset("notes.txt")]}, "no .dmg or .zip asset"),
        ({"assets": []}, "no .dmg or .zip asset"),
        ({"draft": True, "assets": [asset("agterm-0.36.0.dmg")]}, "is a draft"),
    ],
)
def test_asset_selection_refuses_an_ambiguous_or_empty_release(data: dict[str, Any], message: str) -> None:
    with pytest.raises(fr.FetchError, match=message):
        fr.select_asset(data, "v0.36.0")


@pytest.mark.parametrize(("digest", "expected"), [(None, None), ("", None), ("sha256:" + "AB" * 32, "ab" * 32)])
def test_expected_digest(digest: str | None, expected: str | None) -> None:
    assert fr.expected_digest({"digest": digest}) == expected


@pytest.mark.parametrize("digest", ["md5:abc", "sha256:short"])
def test_unsupported_digest_is_refused(digest: str) -> None:
    with pytest.raises(fr.FetchError, match="unsupported asset digest"):
        fr.expected_digest({"digest": digest})


def _fat(*cpus: int, wide: bool = False) -> bytes:
    if wide:
        entries = b"".join(struct.pack(">iiQQII", cpu, 0, 0, 0, 0, 0) for cpu in cpus)
        return struct.pack(">II", 0xCAFEBABF, len(cpus)) + entries
    return struct.pack(">II", 0xCAFEBABE, len(cpus)) + b"".join(struct.pack(">iiIII", cpu, 0, 0, 0, 0) for cpu in cpus)


@pytest.mark.parametrize(
    ("content", "archs"),
    [
        (ARM64, ["arm64"]),
        (X86_64, ["x86_64"]),
        (struct.pack(">II", 0xFEEDFACF, 0x0100000C), ["arm64"]),
        (_fat(0x01000007, 0x0100000C), ["x86_64", "arm64"]),
        (_fat(0x0100000C, 0x01000007, wide=True), ["arm64", "x86_64"]),
        (_fat(0x0100000D), ["cpu-0x100000d"]),
    ],
    ids=["thin-arm64", "thin-x86_64", "big-endian", "universal", "universal-64", "unknown-cpu"],
)
def test_macho_archs(tmp_path: Path, content: bytes, archs: list[str]) -> None:
    binary = tmp_path / "agterm"
    binary.write_bytes(content)
    assert fr.macho_archs(binary) == archs


@pytest.mark.parametrize(
    "content", [b"#!/bin/sh\necho hi\n", b"\x7fELF", struct.pack(">II", 0xCAFEBABE, 2) + bytes(20)]
)
def test_macho_archs_refuses_other_files(tmp_path: Path, content: bytes) -> None:
    binary = tmp_path / "agterm"
    binary.write_bytes(content)
    with pytest.raises(fr.FetchError):
        fr.macho_archs(binary)


def test_fetch_downloads_verifies_extracts_and_records(tmp_path: Path) -> None:
    network = Network()
    reference = network.fetch(tmp_path, release())
    assert network.files == ["https://github.com/umputun/agterm/releases/download/v0.36.0/agterm-0.36.0.dmg"]
    assert reference.bundle == str(tmp_path / "v0.36.0" / "agterm.app")
    assert reference.ctl == str(tmp_path / "v0.36.0" / "agterm.app" / "Contents" / "MacOS" / "agtermctl")
    assert reference.archs == ["arm64"]
    assert reference.digest == hashlib.sha256(IMAGE).hexdigest()
    assert len(network.verified) == 1
    assert sorted(path.name for path in tmp_path.iterdir()) == ["v0.36.0"]
    assert sorted(path.name for path in (tmp_path / "v0.36.0").iterdir()) == ["agterm.app", "reference.json"]


def test_cached_tag_answers_without_the_network(tmp_path: Path) -> None:
    first = Network().fetch(tmp_path, release())
    offline = Network(offline=True)
    assert offline.fetch(tmp_path) == first
    assert offline.files == [] and offline.unpacked == []


def test_refresh_fetches_a_cached_tag_again(tmp_path: Path) -> None:
    Network().fetch(tmp_path, release())
    network = Network(executable=X86_64)
    assert network.fetch(tmp_path, release(), refresh=True).archs == ["x86_64"]
    assert len(network.files) == 1


@pytest.mark.parametrize("damage", ["marker", "executable", "ctl", "tag"])
def test_damaged_cache_is_fetched_again(tmp_path: Path, damage: str) -> None:
    Network().fetch(tmp_path, release())
    entry = tmp_path / "v0.36.0"
    if damage == "marker":
        (entry / "reference.json").write_text("{not json")
    elif damage == "executable":
        (entry / "agterm.app" / "Contents" / "MacOS" / "agterm").unlink()
    elif damage == "ctl":
        (entry / "agterm.app" / "Contents" / "MacOS" / "agtermctl").unlink()
    else:
        recorded = json.loads((entry / "reference.json").read_text())
        (entry / "reference.json").write_text(json.dumps(dict(recorded, tag="v0.35.0")))
    network = Network()
    network.fetch(tmp_path, release())
    assert len(network.files) == 1


def test_digest_mismatch_fails_and_leaves_no_cache_entry(tmp_path: Path) -> None:
    network = Network()
    with pytest.raises(fr.FetchError, match="does not match the release digest aa09d955"):
        network.fetch(tmp_path, release(digest=None))
    assert network.unpacked == []
    assert list(tmp_path.iterdir()) == []


def test_failed_refresh_keeps_the_previous_copy(tmp_path: Path) -> None:
    first = Network().fetch(tmp_path, release())
    with pytest.raises(fr.FetchError, match="does not match"):
        Network().fetch(tmp_path, release(digest=b"something else"), refresh=True)
    assert Network(offline=True).fetch(tmp_path) == first


def test_image_without_the_app_fails_and_leaves_no_cache_entry(tmp_path: Path) -> None:
    with pytest.raises(fr.FetchError, match="holds no agterm.app"):
        Network(bundle=False).fetch(tmp_path, release())
    assert list(tmp_path.iterdir()) == []


def test_unsigned_bundle_fails_before_it_is_cached(tmp_path: Path) -> None:
    network = Network()

    def refuse(bundle: Path) -> None:
        raise fr.FetchError("fails codesign verification")

    network.verify = refuse  # type: ignore[method-assign]
    with pytest.raises(fr.FetchError, match="codesign"):
        network.fetch(tmp_path, release())
    assert list(tmp_path.iterdir()) == []


@pytest.mark.parametrize(
    ("overrides", "message"),
    [
        ({"tag_name": "v0.35.0"}, "asked for v0.36.0, the release is v0.35.0"),
        ({"assets": [{"name": "agterm-0.36.0.dmg"}]}, "no usable download url"),
        ({"assets": [{"name": "../agterm-0.36.0.dmg", "browser_download_url": "https://x/y"}]}, "no usable"),
    ],
)
def test_release_that_does_not_match_the_request_is_refused(
    tmp_path: Path, overrides: dict[str, Any], message: str
) -> None:
    with pytest.raises(fr.FetchError, match=message):
        Network().fetch(tmp_path, release(**overrides))
    assert not (tmp_path / "v0.36.0").exists()


def test_release_api_is_used_when_no_release_json_is_given(tmp_path: Path) -> None:
    network = Network()
    asked: list[tuple[str, str]] = []

    def get_release(repo: str, tag: str) -> dict[str, Any]:
        asked.append((repo, tag))
        return release()

    network.get_release = get_release  # type: ignore[method-assign]
    network.fetch(tmp_path)
    assert asked == [("umputun/agterm", "v0.36.0")]


def test_main_prints_cached_paths_without_the_network(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    reference = Network().fetch(tmp_path, release())
    assert fr.main(["--tag", "v0.36.0", "--cache", str(tmp_path), "--allow-foreign-arch"]) == 0
    assert capsys.readouterr().out.splitlines() == [
        f"bundle={reference.bundle}",
        f"ctl={reference.ctl}",
        "archs=arm64",
    ]
    assert fr.main(["--tag", "v0.36.0", "--cache", str(tmp_path), "--json", "--allow-foreign-arch"]) == 0
    assert json.loads(capsys.readouterr().out)["bundle"] == reference.bundle


@pytest.mark.skipif(sys.platform == "darwin", reason="extraction works on macOS")
def test_main_fails_cleanly_where_the_image_cannot_be_extracted(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    fixture = tmp_path / "release.json"
    fixture.write_text(json.dumps(release()))
    monkeypatch.setattr(fr, "download", lambda url, target: target.write_bytes(IMAGE))
    cache = tmp_path / "cache"
    code = fr.main(["--tag", "v0.36.0", "--cache", str(cache), "--release-json", str(fixture)])
    assert code == 1
    assert "extracting the app needs macOS" in capsys.readouterr().err
    assert list(cache.iterdir()) == []


@pytest.mark.parametrize(
    ("host", "archs", "code"),
    [("arm64", ["arm64"], 0), ("x86_64", ["arm64"], 2), ("arm64", ["x86_64"], 0), ("x86_64", ["x86_64", "arm64"], 0)],
)
def test_main_refuses_a_build_the_mac_cannot_run(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, host: str, archs: list[str], code: int
) -> None:
    Network().fetch(tmp_path, release())
    marker = tmp_path / "v0.36.0" / "reference.json"
    marker.write_text(json.dumps(dict(json.loads(marker.read_text()), archs=archs)))
    monkeypatch.setattr(fr.sys, "platform", "darwin")
    monkeypatch.setattr(fr, "host_arch", lambda: host)
    assert fr.main(["--tag", "v0.36.0", "--cache", str(tmp_path)]) == code


def test_cached_tag_from_another_repository_is_fetched_again(tmp_path: Path) -> None:
    Network().fetch(tmp_path, release())
    network = Network()
    assert network.fetch(tmp_path, release(), repo="someone/agterm").repo == "someone/agterm"
    assert len(network.files) == 1
    assert Network(offline=True).fetch(tmp_path, repo="someone/agterm").repo == "someone/agterm"


def test_relative_cache_answers_from_another_working_directory(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    (tmp_path / "sub").mkdir()
    monkeypatch.chdir(tmp_path)
    first = Network().fetch(Path("cache"), release())
    assert Path(first.bundle).is_absolute()
    monkeypatch.chdir(tmp_path / "sub")
    assert Network(offline=True).fetch(Path("../cache")) == first
