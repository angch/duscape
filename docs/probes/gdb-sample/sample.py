# Runs inside gdb, attached by run.sh: stop the target every INTERVAL seconds and count the top
# frames of the threads in the model or the benchmark. Written to OUT, most frequent first.
import gdb, os, signal, threading, time, collections, sys
pid = int(os.environ["TARGET_PID"]); n = int(os.environ.get("SAMPLES", "150")); interval = float(os.environ.get("INTERVAL", "0.02"))
stacks = collections.Counter()
gdb.execute("set pagination off"); gdb.execute("set print thread-events off")
def kick():
    time.sleep(interval); os.kill(pid, signal.SIGINT)
for i in range(n):
    threading.Thread(target=kick, daemon=True).start()
    try:
        gdb.execute("continue", to_string=True)
    except gdb.error as e:
        break
    try:
        out = gdb.execute("thread apply all bt 25", to_string=True)
    except gdb.error:
        continue
    # keep only threads whose stack mentions the model or the bench
    for block in out.split("\nThread ")[1:]:
        frames = [l for l in block.splitlines() if l.startswith("#")]
        names = []
        for f in frames:
            f = f.split(" in ", 1)[-1].split(" (")[0].split(" at ")[0].strip()
            names.append(f)
        if any("libdiskonaut" in f or "bench" in f or "add_dir_entries" in f for f in names):
            key = " <- ".join(names[:12])
            stacks[key] += 1
            for f in names[:6]:
                stacks["LEAF " + f] += 0
with open(os.environ["OUT"], "w") as o:
    for k, c in stacks.most_common(60):
        if c: o.write(f"{c:5}  {k}\n")
gdb.execute("detach")
