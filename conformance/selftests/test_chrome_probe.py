import importlib.util
import struct
import sys
import zlib
from pathlib import Path

import pytest

spec = importlib.util.spec_from_file_location(
    "chrome_probe", Path(__file__).resolve().parent.parent / "tools" / "chrome_probe.py"
)
assert spec is not None and spec.loader is not None
chrome_probe = importlib.util.module_from_spec(spec)
sys.modules["chrome_probe"] = chrome_probe
spec.loader.exec_module(chrome_probe)


def _chunk(kind: bytes, body: bytes) -> bytes:
    return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))


def _filtered(kind: int, line: bytes, prior: bytes, bpp: int) -> bytes:
    """The encoder's side of `_unfilter`, to build a PNG with every filter type."""
    out = bytearray(len(line))
    for i, x in enumerate(line):
        a = line[i - bpp] if i >= bpp else 0
        b = prior[i]
        c = prior[i - bpp] if i >= bpp else 0
        predictor = [0, a, b, (a + b) >> 1, chrome_probe._paeth(a, b, c)][kind]
        out[i] = (x - predictor) & 0xFF
    return bytes(out)


def _png(rows: list[list[tuple[int, ...]]], color_type: int) -> bytes:
    channels = {2: 3, 6: 4}[color_type]
    width, height = len(rows[0]), len(rows)
    raw, prior = bytearray(), bytes(width * channels)
    for y, row in enumerate(rows):
        line = bytes(channel for pixel in row for channel in pixel)
        kind = y % 5  # None, Sub, Up, Average, Paeth in turn
        raw += bytes([kind]) + _filtered(kind, line, prior, channels)
        prior = line
    header = struct.pack(">IIBBBBB", width, height, 8, color_type, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + _chunk(b"IHDR", header) + _chunk(b"IDAT", zlib.compress(bytes(raw))) + _chunk(b"IEND", b"")


def _pixels(width: int, height: int, alpha: bool) -> list[list[tuple[int, ...]]]:
    def pixel(x: int, y: int) -> tuple[int, ...]:
        rgb = ((x * 37 + y) % 256, (y * 53 + 7) % 256, (x * y) % 256)
        return (*rgb, 255) if alpha else rgb

    return [[pixel(x, y) for x in range(width)] for y in range(height)]


@pytest.mark.parametrize("color_type", [2, 6])
def test_every_png_filter_decodes(color_type: int) -> None:
    rows = _pixels(9, 11, color_type == 6)
    image = chrome_probe.read_png(_png(rows, color_type))
    assert (image.width, image.height) == (9, 11)
    for y in range(11):
        for x in range(9):
            assert image.pixel(x, y) == rows[y][x][:3], (x, y)


def test_unsupported_pngs_are_refused() -> None:
    with pytest.raises(chrome_probe.ProbeError, match="not a PNG"):
        chrome_probe.read_png(b"GIF89a")
    grey = b"\x89PNG\r\n\x1a\n" + _chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 0, 0, 0, 0))
    with pytest.raises(chrome_probe.ProbeError, match="color type 0"):
        chrome_probe.read_png(grey + _chunk(b"IDAT", zlib.compress(b"\x00\x00")) + _chunk(b"IEND", b""))


def _image(width: int, height: int, fill: tuple[int, int, int]) -> "chrome_probe.Image":
    return chrome_probe.Image(width, height, 3, [bytes(fill) * width for _ in range(height)])


def test_the_dominant_color_ignores_a_little_text() -> None:
    image = _image(20, 20, (1, 2, 3))
    rows = [bytearray(row) for row in image.rows]
    rows[5][0:3] = bytes((255, 255, 255))
    image = chrome_probe.Image(20, 20, 3, [bytes(row) for row in rows])
    color, share = chrome_probe.dominant(image, (0, 0, 20, 20), step=1)
    assert color == (1, 2, 3) and share == pytest.approx(399 / 400)
    assert chrome_probe.contains(image, (0, 0, 10, 10), (255, 255, 254))
    assert not chrome_probe.contains(image, (1, 0, 10, 10), (255, 255, 255))


def test_the_wash_matches_the_core() -> None:
    # agterm_core::chrome's tests: (21, 121, 241) under a 0.30 wash
    assert chrome_probe.washed((21, 121, 241), False, 0.30) == (15, 85, 169)
    assert chrome_probe.washed((21, 121, 241), True, 0.30) == (91, 161, 245)


def test_a_theme_background_is_read_from_its_file(tmp_path: Path) -> None:
    (tmp_path / "Dracula").write_text("palette = 0=#21222c\nbackground = #282a36\nforeground = #f8f8f2\n")
    assert chrome_probe.theme_background(tmp_path, "Dracula") == (0x28, 0x2A, 0x36)
    (tmp_path / "Bare").write_text("foreground = #000000\n")
    with pytest.raises(chrome_probe.ProbeError, match="no background"):
        chrome_probe.theme_background(tmp_path, "Bare")


def test_the_boxes_stay_inside_their_regions() -> None:
    layout = chrome_probe.Layout(1280, 800, sidebar=220)
    for name in ("title", "sidebar_body", "terminal", "prompt"):
        left, top, right, bottom = getattr(layout, name)
        assert 0 <= left < right <= 1280 and 0 <= top < bottom <= 800, name
    assert layout.sidebar_body[2] < 220 < layout.terminal[0]
    assert layout.prompt[0] > 220, "the cursor is searched for in the deck only"


def test_a_region_of_mixed_colors_is_refused() -> None:
    image = _image(1280, 800, (9, 9, 9))
    # red over the terminal box's right part only
    rows = [bytes((9, 9, 9)) * 900 + bytes((200, 0, 0) if y >= 400 else (9, 9, 9)) * 380 for y in range(800)]
    mixed = chrome_probe.Image(1280, 800, 3, rows)
    layout = chrome_probe.Layout(1280, 800, sidebar=220)
    assert chrome_probe.regions(image, layout) == dict.fromkeys(("title", "sidebar_body", "terminal"), (9, 9, 9))
    with pytest.raises(chrome_probe.ProbeError, match="terminal box is not one color"):
        chrome_probe.regions(mixed, layout)
