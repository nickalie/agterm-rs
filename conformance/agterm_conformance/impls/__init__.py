"""Adapters per `--impl`."""

from __future__ import annotations

from agterm_conformance.impls.base import Adapter
from agterm_conformance.impls.rust import RustAdapter
from agterm_conformance.impls.swift_linux import SwiftLinuxAdapter
from agterm_conformance.impls.swift_macos import SwiftMacosAdapter
from agterm_conformance.plugin import ImplOptions

ADAPTERS: dict[str, type[Adapter]] = {
    SwiftMacosAdapter.name: SwiftMacosAdapter,
    SwiftLinuxAdapter.name: SwiftLinuxAdapter,
    RustAdapter.name: RustAdapter,
}


def make_adapter(options: ImplOptions) -> Adapter:
    if options.impl not in ADAPTERS:
        raise ValueError(f"unknown implementation {options.impl!r}")
    if options.impl == SwiftMacosAdapter.name:
        return SwiftMacosAdapter(options.bin, options.ctl, launch_mode=options.macos_launch_mode)
    return ADAPTERS[options.impl](options.bin, options.ctl)
