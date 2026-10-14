#!/usr/bin/env python3
"""Samples CPU and GPU use of a process tree: measure.py ROOT_PID SECONDS OUT.json

CPU: utime+stime of every process under ROOT_PID (children appear as they spawn), as % of one core, also per
command name. GPU: `nvidia-smi pmon` per-process SM/decoder % for pids in the tree, and whole-GPU utilization;
Intel (i915) per-client engine busy time from /proc/PID/fdinfo (drm-engine-*), for the tree and for niri."""
import json
import os
import subprocess
import sys
import time

TICK = os.sysconf("SC_CLK_TCK")


def tree(root):
    children = {}
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            with open(f"/proc/{pid}/stat") as f:
                stat = f.read()
        except OSError:
            continue
        rest = stat[stat.rfind(")") + 2:].split()
        children.setdefault(int(rest[1]), []).append(int(pid))
    out, todo = [], [root]
    while todo:
        pid = todo.pop()
        out.append(pid)
        todo.extend(children.get(pid, []))
    return out


def cpu_ticks(pid):
    try:
        with open(f"/proc/{pid}/stat") as f:
            stat = f.read()
        with open(f"/proc/{pid}/comm") as f:
            comm = f.read().strip()
    except OSError:
        return None
    rest = stat[stat.rfind(")") + 2:].split()
    return comm, int(rest[11]) + int(rest[12])


def drm_engines(pids):
    """{(pdev, client_id): {engine: ns}} for the DRM clients the pids hold (i915 reports engine time)."""
    clients = {}
    for pid in pids:
        try:
            fds = os.listdir(f"/proc/{pid}/fdinfo")
        except OSError:
            continue
        for fd in fds:
            try:
                with open(f"/proc/{pid}/fdinfo/{fd}") as f:
                    text = f.read()
            except OSError:
                continue
            if "drm-engine-" not in text:
                continue
            fields = dict(line.split(":", 1) for line in text.splitlines() if ":" in line)
            key = (fields.get("drm-pdev", "").strip(), fields.get("drm-client-id", "").strip())
            clients[key] = {k[len("drm-engine-"):]: int(v.split()[0]) for k, v in fields.items()
                            if k.startswith("drm-engine-") and not k.startswith("drm-engine-capacity")}
    return clients


def engine_busy(before, after, seconds):
    busy = {}
    for key, engines in after.items():
        for engine, ns in engines.items():
            delta = ns - before.get(key, {}).get(engine, 0)
            busy[engine] = busy.get(engine, 0) + delta
    return {engine: round(100.0 * ns / (seconds * 1e9), 1) for engine, ns in busy.items() if ns}


def niri_pids():
    out = []
    for pid in os.listdir("/proc"):
        try:
            if pid.isdigit() and open(f"/proc/{pid}/comm").read().strip() == "niri":
                out.append(int(pid))
        except OSError:
            pass
    return out


def main():
    root, seconds, out = int(sys.argv[1]), float(sys.argv[2]), sys.argv[3]
    pmon = subprocess.Popen(["nvidia-smi", "pmon", "-s", "u", "-d", "1"], stdout=subprocess.PIPE, text=True)
    util = subprocess.Popen(["nvidia-smi", "--query-gpu=utilization.gpu,utilization.decoder", "--format=csv,noheader,nounits",
                             "-lms", "1000"], stdout=subprocess.PIPE, text=True)
    first, start = {}, time.monotonic()
    pids_seen = set()
    for pid in tree(root):
        sample = cpu_ticks(pid)
        if sample:
            first[pid] = sample
    last = dict(first)
    drm_before = drm_engines(tree(root))
    niri_before = drm_engines(niri_pids())
    drm_after = drm_before
    while time.monotonic() - start < seconds:
        time.sleep(0.5)
        for pid in tree(root):
            sample = cpu_ticks(pid)
            if sample:
                first.setdefault(pid, (sample[0], 0))  # spawned during the window: counts from zero
                last[pid] = sample
                pids_seen.add(pid)
        drm_after = {**drm_after, **drm_engines(tree(root))}
    niri_after = drm_engines(niri_pids())
    elapsed = time.monotonic() - start
    pmon.terminate()
    util.terminate()
    per_name, total = {}, 0
    for pid, (comm, ticks) in last.items():
        delta = ticks - first.get(pid, (comm, ticks))[1]
        total += delta
        per_name[comm] = per_name.get(comm, 0) + delta
    to_pct = lambda ticks: round(100.0 * ticks / TICK / elapsed, 1)
    gpu_sm, gpu_dec, gpu_rows = {}, {}, 0
    for line in pmon.stdout.read().splitlines():
        parts = line.split()
        if not parts or parts[0].startswith("#") or len(parts) < 8:
            continue
        pid = int(parts[1]) if parts[1].isdigit() else -1
        if pid in pids_seen or pid in first:
            name = parts[-1]
            for table, value in ((gpu_sm, parts[3]), (gpu_dec, parts[6])):
                if value.isdigit():
                    table.setdefault(name, []).append(int(value))
            gpu_rows += 1
    util_rows = [line.split(",") for line in util.stdout.read().splitlines() if "," in line]
    gpu_util = [int(r[0]) for r in util_rows if r[0].strip().isdigit()]
    dec_util = [int(r[1]) for r in util_rows if r[1].strip().isdigit()]
    avg = lambda xs: round(sum(xs) / len(xs), 1) if xs else None
    result = {
        "seconds": round(elapsed, 1),
        "cpu_pct_total": to_pct(total),
        "cpu_pct_by_name": {k: to_pct(v) for k, v in sorted(per_name.items(), key=lambda kv: -kv[1]) if v},
        "gpu_sm_pct_by_name_avg": {k: avg(v) for k, v in gpu_sm.items()},
        "gpu_dec_pct_by_name_avg": {k: avg(v) for k, v in gpu_dec.items()},
        "gpu_util_pct_whole_gpu_avg": avg(gpu_util),
        "gpu_decoder_pct_whole_gpu_avg": avg(dec_util),
        "intel_engine_busy_pct_tree": engine_busy(drm_before, drm_after, elapsed),
        "intel_engine_busy_pct_niri": engine_busy(niri_before, niri_after, elapsed),
    }
    with open(out, "w") as f:
        json.dump(result, f, indent=2)
    print(json.dumps(result, indent=2))


main()
