#!/bin/sh
# Per-operation micro benchmarks (1M iterations each). Usage: bench/micro.sh [spar-binary]
BIN=${1:-$(dirname "$0")/../target/release/spar}
DIR=$(dirname "$0")/spar/micro
for f in "$DIR"/*.spar; do
  python3 - "$BIN" "$f" <<'PY'
import subprocess,sys,time,os
b,f=sys.argv[1:3]
ts=[]
for _ in range(3):
    t=time.perf_counter(); subprocess.run([b,"exec",f],capture_output=True); ts.append(time.perf_counter()-t)
print("%-12s min=%.3fs"%(os.path.basename(f)[:-5],min(ts)))
PY
done
