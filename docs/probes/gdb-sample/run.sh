#!/bin/bash
# A poor man's sampling profiler, for where `perf` is not allowed (`perf_event_paranoid` 4).
#
#   WAIT_S=1.3 SAMPLES=200 INTERVAL=0.004 docs/probes/gdb-sample/run.sh out.txt -- \
#       target/profiling/duscape --benchmark --bench-stage tree-only /data
#
# Runs the command with PR_SET_PTRACER set to anyone, so that gdb may attach to it from outside
# despite Yama's ptrace_scope 1; waits WAIT_S (to skip a warm-up phase, such as tree-only's
# untimed walk); then gdb attaches and, SAMPLES times, lets it run INTERVAL seconds, stops it and
# records the top frames of every thread that is in the model or the benchmark. out.txt gets the
# stacks by count. Build with `cargo build --profile profiling` first: release is stripped.
out=$1; shift; [ "$1" = -- ] && shift
python3 - "$@" <<'PY' &
import ctypes, os, sys
libc = ctypes.CDLL(None, use_errno=True)
PR_SET_PTRACER = 0x59616d61; PR_SET_PTRACER_ANY = ctypes.c_ulong(-1).value
libc.prctl(PR_SET_PTRACER, ctypes.c_ulong(PR_SET_PTRACER_ANY), 0, 0, 0)
os.execvp(sys.argv[1], sys.argv[1:])
PY
child=$!
sleep "${WAIT_S:-1}"
TARGET_PID=$child OUT=$out gdb -q -p $child -batch -x "$(dirname "$0")/sample.py" >/dev/null 2>&1
wait $child
