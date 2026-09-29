#!/usr/bin/env python3
"""Repeat-run benchmark harness: bench/run.py [--bin PATH] [--runs N] file.spar...
Reports min/median/mean/stdev wall time per file (seconds)."""
import argparse, statistics, subprocess, time, os
ap = argparse.ArgumentParser()
ap.add_argument("--bin", default=os.path.join(os.path.dirname(__file__), "..", "target", "release", "spar"))
ap.add_argument("--runs", type=int, default=7)
ap.add_argument("files", nargs="+")
a = ap.parse_args()
for f in a.files:
    ts = []
    for _ in range(a.runs):
        t = time.perf_counter()
        subprocess.run([a.bin, "exec", f], stdout=subprocess.DEVNULL, check=True)
        ts.append(time.perf_counter() - t)
    sd = statistics.stdev(ts) if len(ts) > 1 else 0.0
    print(f"{os.path.basename(f):16} min={min(ts):.3f} med={statistics.median(ts):.3f} "
          f"mean={statistics.mean(ts):.3f} sd={sd:.3f} n={len(ts)}")
