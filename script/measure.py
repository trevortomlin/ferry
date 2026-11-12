#!/usr/bin/env python3
"""CPU and memory of the real app, in a real window, against a real server.

    script/measure.py BINARY HOST:PORT:USER:KEY:REMOTE_DIR [--prefix FERRY] [--log-target ferry]

Runs the release binary three ways, each with a throwaway HOME and config:

  startup   time to first frame (the app's own probe), median of 5
  idle      nothing open: memory after 3 s, CPU over the next 5 s
  listing   opens REMOTE_DIR (100k entries): load time, CPU spent loading,
            memory once loaded, CPU over 5 s of idling afterwards

Memory comes from /proc/PID/smaps_rollup; CPU from /proc/PID/stat.
"""
import os, re, shutil, statistics, subprocess, sys, tempfile, time

TICK = os.sysconf("SC_CLK_TCK")


def cpu_secs(pid):
    f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    return (int(f[11]) + int(f[12])) / TICK


def mem(pid):
    out = {}
    for line in open(f"/proc/{pid}/smaps_rollup"):
        k, _, v = line.partition(":")
        if v.strip().endswith("kB"):
            out[k.strip()] = int(v.split()[0]) / 1024
    out["threads"] = len(os.listdir(f"/proc/{pid}/task"))
    return out


def sandbox(spec, prefix):
    host, port, user, key, remote = spec.split(":")
    root = tempfile.mkdtemp(prefix="measure-")
    home = os.path.join(root, "home")
    cfg = os.path.join(root, "config")
    os.makedirs(os.path.join(home, ".ssh"))
    name = prefix.lower() if prefix != "FERRY" else "ferry"
    os.makedirs(os.path.join(cfg, name))
    with open(os.path.join(cfg, name, "connections.toml"), "w") as f:
        f.write(f"""[[connection]]
id = 1234
name = "bench"
protocol = "sftp"
host = "{host}"
port = {port}
username = "{user}"
initial_path = "{remote}"
[connection.auth]
kind = "key_file"
path = "{key}"
""")
    # The real cache stays, so fontconfig does not rebuild its cache on
    # every launch; HOME and config are throwaway (known_hosts, bookmarks).
    cache = os.environ.get("XDG_CACHE_HOME", os.path.expanduser("~/.cache"))
    env = dict(os.environ, HOME=home, XDG_CONFIG_HOME=cfg, XDG_CACHE_HOME=cache)
    return root, env


def run(binary, env, wait_for=None, timeout=90):
    log = tempfile.NamedTemporaryFile("w+", delete=False)
    p = subprocess.Popen([binary], env=env, stderr=log, stdout=subprocess.DEVNULL)
    found = None
    if wait_for:
        start = time.time()
        while time.time() - start < timeout and found is None:
            # Strip ANSI colour codes before matching.
            text = re.sub(r"\x1b\[[0-9;]*m", "", open(log.name).read())
            found = re.search(wait_for, text)
            time.sleep(0.01)
    return p, found


def main():
    binary, spec = sys.argv[1], sys.argv[2]
    prefix = "FERRY"
    target = "ferry"
    if "--prefix" in sys.argv:
        prefix = sys.argv[sys.argv.index("--prefix") + 1]
    if "--log-target" in sys.argv:
        target = sys.argv[sys.argv.index("--log-target") + 1]
    root, env = sandbox(spec, prefix)
    res = {}

    # Startup.
    times = []
    for _ in range(6):
        out = subprocess.run([binary], env=dict(env, **{f"{prefix}_STARTUP_PROBE": "1"}),
                             capture_output=True, text=True, timeout=30).stdout
        m = re.search(r"startup_ms=(\d+)", out)
        if m:
            times.append(int(m.group(1)))
    res["startup_ms"] = statistics.median(times[1:])  # first run warms caches

    # Idle, nothing open.
    p, _ = run(binary, env)
    time.sleep(3)
    m = mem(p.pid)
    c0 = cpu_secs(p.pid)
    time.sleep(5)
    res["idle_cpu_pct"] = (cpu_secs(p.pid) - c0) / 5 * 100
    res["idle_rss_mb"], res["idle_dirty_mb"] = m["Rss"], m["Private_Dirty"]
    res["idle_anon_mb"], res["idle_threads"] = m["Anonymous"], m["threads"]
    p.kill(); p.wait()

    # A 100k-entry listing.
    env2 = dict(env, **{f"{prefix}_OPEN": "bench", f"{prefix}_LOG": f"{target}=debug"})
    t0 = time.time()
    p, found = run(binary, env2, wait_for=r"listing complete.*?entries=(\d+).*?millis=([\d.]+)")
    if not found:
        print("listing never completed"); p.kill(); sys.exit(1)
    res["entries"] = int(found.group(1))
    res["load_ms"] = float(found.group(2))
    res["cpu_to_loaded_s"] = cpu_secs(p.pid)
    time.sleep(2)
    m = mem(p.pid)
    res["list_rss_mb"], res["list_dirty_mb"] = m["Rss"], m["Private_Dirty"]
    res["list_anon_mb"], res["list_threads"] = m["Anonymous"], m["threads"]
    c0 = cpu_secs(p.pid)
    time.sleep(5)
    res["list_idle_cpu_pct"] = (cpu_secs(p.pid) - c0) / 5 * 100
    p.kill(); p.wait()

    # Scrolling through it: UI-thread CPU per frame, measured in-app.
    for _ in range(2):
        out = subprocess.run([binary], env=dict(env2, **{f"{prefix}_SCROLL_PROBE": "600"}),
                             capture_output=True, text=True, timeout=90).stdout
        m2 = re.search(r"scroll_frame_us_mean=(\d+) scroll_frame_us_p95=(\d+)", out)
        if m2:
            res["scroll_frame_us"] = int(m2.group(1))
            res["scroll_frame_p95_us"] = int(m2.group(2))
    shutil.rmtree(root, ignore_errors=True)

    res["binary_mb"] = os.path.getsize(binary) / 1048576
    for k, v in res.items():
        print(f"{k:20} {v:.2f}" if isinstance(v, float) else f"{k:20} {v}")


main()
