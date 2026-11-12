#!/usr/bin/env python3
"""Per-thread CPU while the app idles, then while it loads a big listing.

    script/threads.py BINARY HOST:PORT:USER:KEY:REMOTE_DIR
"""
import os, sys, time, importlib.util
spec = importlib.util.spec_from_file_location("m", os.path.join(os.path.dirname(__file__), "measure.py"))
src = open(spec.origin).read().replace("\nmain()\n", "\n")
m = {}
exec(compile(src, "measure", "exec"), m)

def threads(pid):
    out = {}
    for t in os.listdir(f"/proc/{pid}/task"):
        try:
            name = open(f"/proc/{pid}/task/{t}/comm").read().strip()
            f = open(f"/proc/{pid}/task/{t}/stat").read().rsplit(")", 1)[1].split()
            out[(t, name)] = (int(f[11]) + int(f[12])) / m["TICK"]
        except FileNotFoundError:
            pass
    return out

def diff(a, b):
    rows = {}
    for k, v in b.items():
        d = v - a.get(k, 0)
        rows[k[1]] = rows.get(k[1], 0) + d
    return sorted(((v, k) for k, v in rows.items() if v > 0), reverse=True)

binary, server = sys.argv[1], sys.argv[2]
root, env = m["sandbox"](server, "FERRY")
p, _ = m["run"](binary, env)
time.sleep(3); a = threads(p.pid); time.sleep(10); b = threads(p.pid)
print("idle 10 s:", [(n, round(v * 1000)) for v, n in diff(a, b)], "ms")
p.kill(); p.wait()
env2 = dict(env, FERRY_OPEN="bench", FERRY_LOG="ferry=debug")
p, found = m["run"](binary, env2, wait_for=r"listing complete.*?millis=([\d.]+)")
b = threads(p.pid)
print("start + load:", [(n, round(v * 1000)) for v, n in diff({}, b)], "ms", "load", found and found.group(1))
time.sleep(1); a = threads(p.pid); time.sleep(10); b = threads(p.pid)
print("idle after load 10 s:", [(n, round(v * 1000)) for v, n in diff(a, b)], "ms")
p.kill(); p.wait()
