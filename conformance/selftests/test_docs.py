from pathlib import Path

from agterm_conformance.impls import swift_linux, swift_macos
from agterm_conformance.impls.base import CAPABILITIES

RUST_BUILD = Path(__file__).resolve().parents[1] / "docs" / "rust-build.md"
HOST_DEPENDENT = frozenset({"live-sessions"})


def _claim(name: str, static: frozenset[str]) -> str:
    if name in HOST_DEPENDENT:
        return "host"
    return "yes" if name in static else ""


def expected_table() -> list[str]:
    return [
        f"| `{name}` | {_claim(name, swift_macos.BASE_CAPABILITIES)} | {_claim(name, swift_linux.CAPABILITIES)} "
        f"| {meaning} |"
        for name, meaning in CAPABILITIES.items()
    ]


def test_capability_table_matches_the_vocabulary() -> None:
    documented = [line for line in RUST_BUILD.read_text().splitlines() if line.startswith("| `")]
    expected = expected_table()
    assert documented == expected, "rust-build.md capability table drifted; expected rows:\n" + "\n".join(expected)
