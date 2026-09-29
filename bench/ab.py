#!/usr/bin/env python3
"""Interleaved A/B: bench/ab.py binA binB runs file.spar...  (min / median wall seconds)."""
import subprocess, sys, time, statistics, os
a, b, runs = sys.argv[1], sys.argv[2], int(sys.argv[3])
for f in sys.argv[4:]:
    ts = {a: [], b: []}
    for _ in range(runs):
        for bin_ in (a, b):
            t = time.perf_counter()
            subprocess.run([bin_, "exec", f], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            ts[bin_].append(time.perf_counter() - t)
    print("%-14s A min=%.3f med=%.3f | B min=%.3f med=%.3f  (B/A min = %.2f)" % (
        os.path.basename(f)[:-5], min(ts[a]), statistics.median(ts[a]),
        min(ts[b]), statistics.median(ts[b]), min(ts[b]) / min(ts[a])))
