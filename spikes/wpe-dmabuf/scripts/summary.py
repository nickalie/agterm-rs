#!/usr/bin/env python3
"""Prints the compact part of spike result files: summary.py RESULT.json..."""
import json
import sys

KEYS = ["frame_size", "frames", "dmabuf_frames", "shm_frames", "damage_ratio", "fence_waits", "shm_copy_us_total",
        "buffers_changed", "dmabuf_imports", "dmabuf_import_failures", "pixel_upload_bytes", "external_draws"]

for path in sys.argv[1:]:
    d = json.load(open(path))
    print(f"== {path.rsplit('/', 1)[-1]}  elapsed {d['elapsed_s']:.1f}s")
    print("  " + ", ".join(f"{k}={d[k]:.3g}" if isinstance(d[k], float) else f"{k}={d[k]}" for k in KEYS))
    for name in ("wpe_frames", "gpui_paints"):
        s = d[name]
        print(f"  {name}: n={s['count']} fps={s['fps']:.1f} p50={s['interval_ms_p50']:.1f} "
              f"p95={s['interval_ms_p95']:.1f} p99={s['interval_ms_p99']:.1f} max={s['interval_ms_max']:.1f} "
              f">25ms={s['intervals_over_25ms']}")
    bench = d["bench"]
    if bench and isinstance(bench, list):
        if len(bench) > 3:
            print("  first:", json.dumps(bench[0])[:300])
        print("  last:", json.dumps(bench[-1])[:700])
