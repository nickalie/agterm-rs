#!/usr/bin/env bash
# Regenerate the checked-in crates/agterm-core/src/ghostty_config/keys.rs from ghostty's src/config/Config.zig at the
# GHOSTTY_REV of build/pins.env (the libghostty whose config diagnostics upstream and the GTK build count): every
# config key with its value kind, the compatibility handlers of renamed and removed keys, and the X11 color names.
# Run after a GHOSTTY_REV bump, then review the diff: the script fails on a field type or handler shape it does not
# know, so a new kind of value is decided in agterm-core's validate.rs, not guessed here.
#
# Usage: scripts/gen-ghostty-config-keys.sh [SOURCE]   (SOURCE: an unpacked ghostty source tree at GHOSTTY_REV;
#                                                       default: the archive of GHOSTTY_REV, fetched)
set -euo pipefail

# shellcheck source=lib/native.sh
source "$(dirname "$0")/lib/native.sh"

OUT="$NATIVE_ROOT/crates/agterm-core/src/ghostty_config/keys.rs"
SOURCE="${1:-}"
if [[ -z "$SOURCE" ]]; then
  mkdir -p "$VENDOR_ROOT"
  # the scratch dir lives in the vendor root, not in a shared /tmp
  SOURCE="$(mktemp -d "$VENDOR_ROOT/.ghostty-config-keys.XXXXXX")"
  trap 'rm -rf "$SOURCE"' EXIT
  echo "fetching ghostty $GHOSTTY_REV..." >&2
  curl -fsSL "$GHOSTTY_REPO/archive/$GHOSTTY_REV.tar.gz" \
    | tar -xz -C "$SOURCE" --strip-components=1 --wildcards '*/src/*'
fi
[[ -s "$SOURCE/src/config/Config.zig" ]] || die "no src/config/Config.zig under $SOURCE"

python3 -I - "$SOURCE" "$GHOSTTY_REV" >"$OUT.tmp" <<'PY'
import re
import sys
from pathlib import Path

SRC = Path(sys.argv[1])
REV = sys.argv[2]
CONFIG_TEXT = (SRC / "src/config/Config.zig").read_text()

# Field types declared outside Config.zig: the alias Config.zig uses -> (file, declaration; None for a file struct).
EXTERNAL = {
    "terminal.CursorStyle": ("src/terminal/cursor.zig", "Style"),
    "inputpkg.OptionAsAlt": ("src/input/config.zig", "OptionAsAlt"),
    "build_config.ReleaseChannel": ("src/build/Config.zig", "ReleaseChannel"),
    "WindowPaddingBalance": ("src/renderer/size.zig", "PaddingBalance"),
    "MetricModifier": ("src/font/Metrics.zig", "Modifier"),
    "Command": ("src/config/command.zig", "Command"),
    "RepeatableReadableIO": ("src/config/io.zig", "RepeatableReadableIO"),
    "RepeatableStringMap": ("src/config/RepeatableStringMap.zig", None),
    "KeyRemapSet": ("src/input/key_mods.zig", "RemapSet"),
}
# Types with a parseCLI that validate.rs ports, by the kind it checks them as. Any other parseCLI type is opaque.
PORTED = {
    "Color": "Color",
    "TerminalColor": "TerminalColor",
    "BoldColor": "BoldColor",
    "Duration": "Duration",
    "WindowPadding": "WindowPadding",
    "WindowDecoration": "WindowDecoration",
    "Theme": "Theme",
    "Palette": "Palette",
    "RepeatableString": "RepeatableString",
    "Path": "Path",
    "RepeatablePath": "RepeatablePath",
    # ghostty_config/binds.rs: the trigger, flag, table and action grammar of Binding.zig and Keybinds.parseCLI
    "Keybinds": "Keybind",
}
# Types with a parseCLI validate.rs does not port (by their declaration's name): their values are accepted as is, the
# residual differences ghostty_config/mod.rs lists. Any other unported parseCLI type stops the script.
OPAQUE = {
    "Modifier", "Command", "RepeatableReadableIO", "RepeatableStringMap", "RemapSet", "RepeatableFontVariation",
    "RepeatableCodepointMap", "RepeatableClipboardCodepointMap", "FontStyle", "RepeatableLink", "RepeatableCommand",
    "QuickTerminalSize", "BackgroundBlur", "SelectionWordChars", "MouseScrollMultiplier", "WorkingDirectory",
    "ColorList",
}
PATHS = {"Path": "src/config/path.zig", "RepeatablePath": "src/config/path.zig"}
INTS = {"u8": (False, 8), "u16": (False, 16), "u21": (False, 21), "u32": (False, 32), "u64": (False, 64),
        "usize": (False, 64), "i8": (True, 8), "i16": (True, 16), "i32": (True, 32), "i64": (True, 64),
        "isize": (True, 64)}
STRINGS = {"[]const u8", "[:0]const u8"}
FIELD = re.compile(r'^(?:@"([a-z0-9-]+)"|([a-z][a-z0-9_]*))\s*:\s*(.+?)\s*=')
VARIANT = re.compile(r'^(?:@"([^"]+)"|([A-Za-z_][A-Za-z0-9_]*))\s*(?:=\s*[^,]+)?,$')
BOOL_FIELD = re.compile(r'^(?:@"([^"]+)"|([A-Za-z_][A-Za-z0-9_]*))\s*:\s*bool\b')


def die(message):
    sys.exit(f"gen-ghostty-config-keys: {message}")


def strip_code(text):
    """Comments, strings and char literals blanked (lengths kept), @"identifiers" kept, so braces can be matched."""
    out, i, n = [], 0, len(text)
    while i < n:
        c = text[i]
        if text.startswith("//", i):
            end = text.find("\n", i)
            end = n if end < 0 else end
            out.append(" " * (end - i))
            i = end
        elif c in "\"'":
            j = i + 1
            while j < n and text[j] != c and text[j] != "\n":
                j += 2 if text[j] == "\\" else 1
            if c == '"' and text[i - 1:i] == "@":
                out.append(text[i:j + 1])
            else:
                out.append(c + " " * (j - i - 1) + c)
            i = j + 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def declaration(text, name):
    """`const NAME = ...` at the top level of `text`, up to its semicolon, comments and strings blanked."""
    code = strip_code(text)
    if name is None:
        return "struct {" + code + "}"
    match = re.search(rf"^(?:pub )?const {re.escape(name)} = ", code, re.M)
    if not match:
        die(f"no top-level declaration of {name}")
    depth = 0
    for i in range(match.end(), len(code)):
        if code[i] in "{(":
            depth += 1
        elif code[i] in "})":
            depth -= 1
        elif code[i] == ";" and depth == 0:
            return code[match.end():i]
    die(f"unterminated declaration of {name}")


def top_level_lines(body):
    """The lines of a container body at its own nesting level, nested blocks dropped."""
    depth, line, lines = 0, [], []
    for c in body[body.index("{") + 1:]:
        if c in "{(":
            depth += 1
        elif c in "})":
            if depth == 0:
                break
            depth -= 1
        elif depth == 0:
            if c == "\n":
                lines.append("".join(line).strip())
                line = []
            else:
                line.append(c)
    lines.append("".join(line).strip())
    return [entry for entry in lines if entry]


def opaque(name, body):
    """A parseCLI validate.rs does not port, listed in OPAQUE: whether it needs a value, or refuses every one."""
    if name not in OPAQUE:
        die(f"{name} has a parseCLI that is neither ported nor known opaque: port it in validate.rs, or add it to "
            "OPAQUE and to the residual differences of ghostty_config/mod.rs")
    match = re.search(r"\bfn parseCLI\(", body)
    following = re.search(r"\bfn \w+\(", body[match.end():])
    snippet = body[match.end():match.end() + following.start()] if following else body[match.end():]
    function = snippet[snippet.index("{") + 1:]
    statements = [s.strip() for s in function.split(";") if s.strip() and not s.strip().startswith("_ =")]
    if statements[:1] == ["return error.NotImplemented"]:
        return "Kind::NotImplemented"
    needs = "true" if "error.ValueRequired" in snippet else "false"
    return f"Kind::Opaque {{ needs_value: {needs} }}"


def classify(name, body, raw):
    header = body.split("{", 1)[0]
    if re.search(r"\bfn parseCLI\(", body):
        return f"Kind::{PORTED[name]}" if name in PORTED else opaque(name, body)
    if re.match(r"\s*lib\.Enum\(", body):
        return enum_kind(name, re.findall(r'"([^"]+)"', raw))
    if re.search(r"\benum\b", header) and "union" not in header:
        variants = [m.group(1) or m.group(2) for m in map(VARIANT.match, top_level_lines(body)) if m]
        return enum_kind(name, variants)
    if "packed struct" in header:
        flags = []
        for line in top_level_lines(body):
            if line.startswith(("pub ", "const ", "fn ", "test ")):
                continue
            match = BOOL_FIELD.match(line)
            if not match:
                die(f"packed struct {name} has a field that is not a bool: {line}")
            flags.append(match.group(1) or match.group(2))
        return "Kind::Flags(&[" + ", ".join(f'"{flag}"' for flag in flags) + "])"
    die(f"{name} is a struct or union without parseCLI ({header.strip()}): decide its kind in validate.rs")


def enum_kind(name, variants):
    if not variants:
        die(f"enum {name} without variants")
    return "Kind::Enum(&[" + ", ".join(f'"{variant}"' for variant in variants) + "])"


def resolve(zig_type):
    base = zig_type[1:] if zig_type.startswith("?") else zig_type
    if base == "bool":
        return "Kind::Bool"
    if base in INTS:
        signed, bits = INTS[base]
        return f"Kind::Int {{ signed: {str(signed).lower()}, bits: {bits} }}"
    if base in ("f32", "f64"):
        return "Kind::Float"
    if base in STRINGS:
        return "Kind::String"
    if base.startswith("Limit("):
        return "Kind::Limit"
    if base in PATHS:
        text = (SRC / PATHS[base]).read_text()
        return classify(base, declaration(text, base), text)
    if base in EXTERNAL:
        path, decl = EXTERNAL[base]
        text = (SRC / path).read_text()
        body = declaration(text, decl)
        return classify(base if base in PORTED else (decl or base), body, raw_declaration(text, decl))
    if not re.fullmatch(r"[A-Z][A-Za-z0-9]*", base):
        die(f"unknown field type {zig_type}")
    return classify(base, declaration(CONFIG_TEXT, base), raw_declaration(CONFIG_TEXT, base))


def raw_declaration(text, name):
    if name is None:
        return text
    match = re.search(rf"^(?:pub )?const {re.escape(name)} = ", text, re.M)
    return text[match.end():match.end() + len(declaration(text, name))]


def fields():
    result = []
    for line in CONFIG_TEXT.splitlines():
        match = FIELD.match(line)
        if match and not (match.group(1) or match.group(2)).startswith("_"):
            result.append((match.group(1) or match.group(2), match.group(3)))
    return sorted(result)


def function_body(name):
    match = re.search(rf"^fn {re.escape(name)}\(.*?^\}}", CONFIG_TEXT, re.S | re.M)
    if not match:
        die(f"no function {name}")
    return match.group(0)


def compatibility():
    block = re.search(r"pub const compatibility = .*?initComptime\(&\.\{(.*?)\n\}\);", CONFIG_TEXT, re.S)
    if not block:
        die("no compatibility map")
    entries = []
    for key, handler in re.findall(r'\.\{\s*"([a-z0-9-]+)",\s*([^}]+?)\s*\}', block.group(1)):
        renamed = re.fullmatch(r'cli\.compatibilityRenamed\(Config, "([a-z0-9-]+)"\)', handler)
        if renamed:
            entries.append((key, f'Compat::Renamed("{renamed.group(1)}")'))
            continue
        if not re.fullmatch(r"compat[A-Za-z]+", handler):
            die(f"unknown compatibility handler {handler}")
        body = function_body(handler)
        value = re.search(r'std\.mem\.eql\(u8, value orelse "", "([^"]+)"\)', body)
        if value:
            entries.append((key, f'Compat::Value("{value.group(1)}")'))
        elif "cli.args.parseBool(value_ orelse \"t\") catch return false" in body:
            entries.append((key, "Compat::Bool"))
        else:
            die(f"compatibility handler {handler} has an unknown shape")
    return sorted(entries)


def x11_colors():
    colors = {}
    for line in (SRC / "src/terminal/res/rgb.txt").read_text().splitlines():
        if not line.strip():
            continue
        rgb = tuple(int(line[at:at + 3]) for at in (0, 4, 8))
        name = line[12:].strip(" \t").lower()
        if colors.setdefault(name, rgb) != rgb:
            die(f"X11 color {name} has two values")
    return sorted(colors.items())


def main():
    keys = fields()
    if len(keys) != len({name for name, _ in keys}):
        die("duplicate keys")
    out = [
        "//! Generated by scripts/gen-ghostty-config-keys.sh from ghostty's `src/config/Config.zig` at the `GHOSTTY_REV`",
        "//! of build/pins.env: every config key with its value kind, the compatibility handlers of renamed and removed",
        "//! keys, and the X11 color names colors may be given by (`src/terminal/res/rgb.txt`, MIT/X11 license). Do not",
        "//! edit by hand.",
        "",
        "use super::validate::{Compat, KeySpec, Kind};",
        "",
        "/// The Ghostty revision the tables were generated from.",
        f'pub const GHOSTTY_REV: &str = "{REV}";',
        "",
        "/// Every key ghostty's config knows, sorted by name, with its Zig type and the kind its values are checked as.",
        "pub static KEYS: &[KeySpec] = &[",
    ]
    for name, zig in keys:
        out.append(f'    KeySpec {{ name: "{name}", zig: "{zig}", kind: {resolve(zig)} }},')
    out += [
        "];",
        "",
        "/// Keys ghostty still takes after a rename or removal, sorted; consulted only when the key itself fails.",
        "pub static COMPATIBILITY: &[(&str, Compat)] = &[",
    ]
    out += [f'    ("{key}", {handler}),' for key, handler in compatibility()]
    out += [
        "];",
        "",
        "/// The X11 color names, lowercased and sorted (ghostty matches them ignoring ASCII case).",
        "pub static X11_COLORS: &[(&str, [u8; 3])] = &[",
    ]
    out += [f'    ("{name}", [{r}, {g}, {b}]),' for name, (r, g, b) in x11_colors()]
    out.append("];")
    print("\n".join(out))


main()
PY
rustfmt --edition 2024 --config-path "$NATIVE_ROOT/rustfmt.toml" "$OUT.tmp"
mv "$OUT.tmp" "$OUT"
echo "→ wrote $OUT" >&2
