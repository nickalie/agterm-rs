"""The human output `agtermctl` prints for a protocol result, so a CLI test can compare it exactly.

Mirrors the reference CLI's `SocketClient.formatResponse` and its helpers, and `EventFormatter.human`. A test gets
the result from the protocol (or the same command's `--json`), renders it here and compares the CLI's stdout.
"""

from __future__ import annotations

import os
import re
import time
from collections.abc import Mapping, Sequence
from typing import Any

UUID = re.compile(r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}")


def normalize(text: str, paths: Mapping[str, str] | None = None) -> str:
    """Ids become `<id>` and each path in `paths` (longest first) its placeholder."""
    for path, placeholder in sorted((paths or {}).items(), key=lambda item: -len(item[0])):
        text = text.replace(os.fspath(path), placeholder)
    return UUID.sub("<id>", text)


def _or(value: Any, default: Any) -> Any:
    """Swift's `??`: only a missing value falls back; an empty string is kept."""
    return default if value is None else value


def swift_double(value: float) -> str:
    """Swift's `Double.description` for the values the CLI prints: `300.0`, `271.34`."""
    return repr(float(value))


def human(result: Mapping[str, Any] | None, echo_id: bool = False) -> str:
    """`formatResponse` for an ok response, without the trailing newline `print` adds."""
    result = result or {}
    if "tree" in result:
        return tree(result["tree"])
    if "windows" in result:
        return windows(result["windows"])
    if "themes" in result:
        return themes(
            result["themes"], result.get("theme"), result.get("sync", False), result.get("light"), result.get("dark")
        )
    if "keymap" in result:
        return keymap(result["keymap"])
    if "hooks" in result:
        return hooks(result["hooks"])
    if "remote" in result:
        return remote_tree(result["remote"])
    if "zmx" in result:
        return zmx(result["zmx"])
    if "restore" in result:
        return restore_status(result["restore"])
    if "app" in result:
        app = result["app"]
        return f"{app['version']} ({app['commit']})" if app.get("commit") else app["version"]
    if "text" in result:
        return result["text"]
    if "exitCode" in result:
        return f"exit {result['exitCode']}"
    if "affected" in result:
        return "1 session" if result["affected"] == 1 else f"{result['affected']} sessions"
    if "count" in result:
        return "ok" if result["count"] == 0 else f"{result['count']} diagnostic(s)"
    if "cursor" in result:
        return str(result["cursor"]["column"])
    if "width" in result and "height" in result:
        return f"{result['width']} {result['height']}"
    if "ratio" in result:
        return f"{result['ratio']:.3f}"
    if "sidebarWidth" in result:
        return swift_double(result["sidebarWidth"])
    if echo_id and "id" in result:
        return result["id"]
    return "ok"


def restore_status(status: Mapping[str, Any]) -> str:
    lines = [
        f"configured: {status['configured']} (next launch)",
        f"this launch: requested {status['requestedAtLaunch']}, active {status['active']}",
    ]
    if status.get("restartRequired"):
        lines.append("restart agterm to apply the configured mode")
    if status.get("unavailableReason") is not None:
        lines.append(f"live unavailable: {status['unavailableReason']}")
    return "\n".join(lines)


def remote_tree(remote: Mapping[str, Any]) -> str:
    place = f" on {remote['host']}" if remote.get("host") is not None else ""
    if not remote["sessions"]:
        return f"no attachable sessions{place}"
    rows = []
    for session in remote["sessions"]:
        panes = session["panes"]
        split = ""
        if len(panes) > 1:
            axis = session.get("splitAxis")
            split = f" (split{'' if axis is None else ' ' + axis})"
        running = [os.path.basename(pane["foreground"][0]) for pane in panes if pane.get("foreground")]
        detail = "  ".join(part for part in (session.get("context"), " | ".join(running) or None) if part is not None)
        tail = f"  {detail}" if detail else ""
        at = f"{session['windowName']}/{session['workspaceName']}/{session['name']}"
        rows.append(f"  {at}{split}  [{session['id']}]  {session['cwd']}{tail}")
    return "\n".join(rows)


def zmx(inventory: Mapping[str, Any]) -> str:
    lines = [restore_status(inventory["restore"])]
    if not inventory["inventoryComplete"]:
        lines.append("inventory incomplete: some pane is unaccounted for, so nothing can be pruned")
    if not inventory["entries"]:
        return "\n".join([*lines, "no daemons"])
    return "\n".join([*lines, "", *(_zmx_row(entry) for entry in inventory["entries"])])


def _zmx_row(entry: Mapping[str, Any]) -> str:
    clients = entry.get("clients")
    count = "-" if clients is None else f"{clients} client{'' if clients == 1 else 's'}"
    owner = "-"
    if entry.get("sessionID") is not None:
        window = _or(entry.get("windowName"), _or(entry.get("windowState"), "?"))
        path = " / ".join([window, _or(entry.get("workspaceName"), "?"), _or(entry.get("sessionName"), "?")])
        pane = f" ({entry['pane']})" if entry.get("pane") is not None else ""
        window_id = f" win {entry['windowID'][:8]}" if entry.get("windowID") is not None else ""
        owner = f"{entry['sessionID'][:8]} {path}{pane}{window_id}"
    state = entry["state"] if entry.get("windowState") is None else f"{entry['state']} [{entry['windowState']} window]"
    observation = f"{entry['observation']} outdated" if entry.get("outdated") is True else entry["observation"]
    return f"{entry['daemon']}  {state}  {observation}  {count}  {owner}"


def themes(
    names: Sequence[str], current: str | None, sync: bool = False, light: str | None = None, dark: str | None = None
) -> str:
    def active(name: str | None) -> bool:
        return (name is not None and name in (light, dark)) if sync else name == current

    def line(name: str | None, label: str) -> str:
        return ("* " if active(name) else "  ") + label

    body = "\n".join([line(None, "default ghostty"), *(line(name, name) for name in names)])
    if not sync:
        return body
    header = (
        f"syncing with macOS appearance — light: {_or(light, 'default ghostty')}, dark: {_or(dark, 'default ghostty')}"
    )
    return f"{header}\n{body}"


def hooks(listing: Mapping[str, Any]) -> str:
    lines = [f"hooks: {listing['path']}"]
    if not listing["hooks"]:
        lines.append("  (no hooks)")
    for hook in listing["hooks"]:
        row = f"  line {hook['line']}: on {hook['kind']} {hook['command']}"
        if hook.get("runningPid") is not None:
            row += f"  running pid {hook['runningPid']}"
            if hook.get("elapsedSeconds") is not None:
                row += f" for {int(hook['elapsedSeconds'])}s"
        if hook["pending"] > 0:
            row += f"  pending {hook['pending']}"
        if hook["dropped"] > 0:
            row += f"  dropped {hook['dropped']}"
        if hook.get("lastFailure") is not None:
            row += f"  last failure: {hook['lastFailure']}"
        if hook.get("retired") is True:
            row += "  (retired, removed from the file)"
        lines.append(row)
    if listing["diagnostics"]:
        lines += ["", "diagnostics:", *(f"    line {d['line']}: {d['message']}" for d in listing["diagnostics"])]
    return "\n".join(lines)


def keymap(listing: Mapping[str, Any]) -> str:
    lines = [f"keymap: {listing['path']}", "", "actions:"]
    width = max((len(action["action"]) for action in listing["actions"]), default=0)
    for action in listing["actions"]:
        mark = "*" if action.get("overridden") is True else " "
        binds = "|".join(([action["chord"]] if action.get("chord") is not None else []) + action.get("alternates", []))
        repeat = "  --repeat" if action.get("repeats") is True else ""
        lines.append(f"  {mark} {action['action'].ljust(width)}  {binds or '-'}{repeat}")
    if listing["commands"]:
        lines += ["", "commands:"]
        for command in listing["commands"]:
            row = f"    {command['name']}  {_or(command.get('shortcut'), '(palette only)')}"
            if command["repeats"]:
                row += "  --repeat"
            if command["errorHud"]:
                row += f"  --error-hud --error-position {command['errorPosition']}"
                if command.get("errorPane") is not None:
                    row += f" --error-pane {command['errorPane']}"
            lines.append(row)
    if listing["diagnostics"]:
        lines += ["", "diagnostics:"]
        lines += [
            f"    line {d['line']}: {d['message']}" if d["line"] > 0 else f"    {d['message']}"
            for d in listing["diagnostics"]
        ]
    if listing.get("menu") is not None:
        lines += ["", "menu:"]
        lines += [
            f"    {item['chord']}  {item['menu']} ▸ {item['title']}"
            + ("  (disabled)" if item.get("enabled") is False else "")
            for item in listing["menu"]
        ]
    return "\n".join(lines)


def windows(nodes: Sequence[Mapping[str, Any]]) -> str:
    return "\n".join(
        f"{w['id']}  {w['name']}" + (" [open]" if w["open"] else "") + (" [active]" if w["active"] else "")
        for w in nodes
    )


def tree(top: Mapping[str, Any]) -> str:
    lines = []
    for workspace in top["workspaces"]:
        lines.append(f"{'*' if workspace['active'] else ' '} {workspace['name']}  [{workspace['id']}]")
        for session in workspace["sessions"]:
            split = " (split)" if session["split"] else (" (split hidden)" if session.get("hasSplit") is True else "")
            tags = split + (" (not realized)" if session.get("realized") is False else "")
            tags += (" (overlay)" if session["overlay"] else "") + (" (scratch)" if session["scratch"] else "")
            split_cwd = session.get("splitCwd")
            suffix = f"  split cwd: {split_cwd}" if split_cwd is not None and split_cwd != session["cwd"] else ""
            if session.get("title") is not None:
                suffix += f"  title: {session['title']}"
            if session.get("liveAttribution") is not None:
                suffix += f"  live attribution: {session['liveAttribution']}"
            if session.get("splitLiveAttribution") is not None:
                suffix += f"  split live attribution: {session['splitLiveAttribution']}"
            if (presentation := session.get("presentation")) is not None:
                suffix += f"  presentation: {presentation['state']}"
                suffix += ", presenter" if presentation.get("mode") == "presenter" else ""
                suffix += f" ({presentation['error']})" if presentation.get("error") is not None else ""
            if (presenters := session.get("presenters")) is not None:
                suffix += "  presented remotely" if presenters.get("presenter") is True else ""
                suffix += f"  mirrored by: {presenters['mirrors']}" if presenters["mirrors"] > 0 else ""
            mark = "*" if session["active"] else " "
            lines.append(f"  {mark} {session['name']}{tags}  [{session['id']}]  {session['cwd']}{suffix}")
    return "\n".join(lines)


def event(item: Mapping[str, Any]) -> str:
    """`EventFormatter.human`: the clock in the local time zone, the kind, the session name, then its arguments."""
    clock = time.strftime("%H:%M:%S", time.localtime(item["ts"]))
    payload = item.get("payload") or {}
    name = _or(payload.get("name"), _or(item.get("window"), "-"))
    kind = item["kind"]
    if kind == "status":
        parts = [clock, kind, name, _or(payload.get("status"), "idle")]
        parts += [f"previous={payload['previous']}"] if payload.get("previous") is not None else []
        parts += [f"pane={payload['pane']}"] if payload.get("pane") is not None else []
        parts += ["blink"] if payload.get("blink") is True else []
        parts += [f"color={payload['color']}"] if payload.get("color") is not None else []
        parts += [f"shape={payload['shape']}"] if payload.get("shape") is not None else []
        return " ".join(parts)
    if kind == "notify":
        return f"{clock} {kind} {name} {_or(payload.get('title'), name)}: {_or(payload.get('body'), '')}"
    if kind in ("session.created", "session.closed", "tree.changed"):
        return f"{clock} {kind} {name}"
    if kind == "session.selected":
        return f"{clock} {kind} {name} previous={_or(payload.get('previous'), '-')}"
    if kind in ("pane.split", "pane.scratch"):
        return f"{clock} {kind} {name} {_or(payload.get('status'), '')}"
    if kind in ("remote.opened", "remote.closed"):
        return f"{clock} {kind} {name} host={_or(payload.get('host'), '')}"
    raise ValueError(f"no human form for event kind {kind!r}")
