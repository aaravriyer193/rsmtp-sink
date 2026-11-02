#!/usr/bin/env python3
"""Benchmark rsmtp-sink against Postfix smtp-sink with Postfix smtp-source.

Servers run in alternating order on a fresh port each round, so one server
never benefits from warm state or inherits the other's TIME_WAIT sockets.
"""
import argparse
import json
import os
import platform
import signal
import socket
import statistics
import subprocess
import time

POSTFIX = "/usr/libexec/postfix"
RUST = os.path.join(os.path.dirname(__file__), "..", "target", "release", "rsmtp-sink")

SERVERS = {
    "postfix": lambda addr: [f"{POSTFIX}/smtp-sink", "-m", "1024", addr, "1024"],
    "rust": lambda addr: [RUST, addr, "1024"],
    "rust-4t": lambda addr: [RUST, "--threads", "4", addr, "1024"],
}

# clients: smtp-source processes. sessions: concurrent sessions per process.
# reuse: keep the connection open between messages (smtp-source -d).
SCENARIOS = [
    dict(name="1KB, new connection each message", clients=1, sessions=20, msgs=5000, kb=1, reuse=False),
    dict(name="1KB, reused connections", clients=1, sessions=20, msgs=50000, kb=1, reuse=True),
    dict(name="1MB, reused connections", clients=1, sessions=4, msgs=2000, kb=1024, reuse=True),
    dict(name="1KB, reused, 6 client processes", clients=6, sessions=20, msgs=20000, kb=1, reuse=True),
]


def wait_listening(port, deadline=5.0):
    start = time.monotonic()
    while time.monotonic() - start < deadline:
        with socket.socket() as s:
            if s.connect_ex(("127.0.0.1", port)) == 0:
                return
        time.sleep(0.01)
    raise RuntimeError(f"server never listened on {port}")


def run_once(server, scenario, port):
    addr = f"127.0.0.1:{port}"
    proc = subprocess.Popen(
        SERVERS[server](addr), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True
    )
    try:
        wait_listening(port)
        # The readiness probe above costs one connection on every server equally.
        cmd = [f"{POSTFIX}/smtp-source", "-s", str(scenario["sessions"]),
               "-m", str(scenario["msgs"]), "-l", str(scenario["kb"] * 1024)]
        if scenario["reuse"]:
            cmd.append("-d")
        cmd.append(addr)

        start = time.monotonic()
        clients = [subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
                   for _ in range(scenario["clients"])]
        failures = []
        for client in clients:
            _, err = client.communicate()
            if client.returncode != 0 or err.strip():
                failures.append(err.strip()[:300] or f"exit {client.returncode}")
        wall = time.monotonic() - start
    finally:
        proc.send_signal(signal.SIGINT)
    stderr = proc.stderr.read()
    _, _, usage = os.wait4(proc.pid, 0)
    proc.returncode = 0

    total = scenario["clients"] * scenario["msgs"]
    counted = None
    if server != "postfix":
        counted = int(stderr.rsplit("messages=", 1)[1].strip())
        if counted != total:
            failures.append(f"server counted {counted}, expected {total}")
    return dict(
        wall=wall,
        msgs_per_sec=total / wall,
        server_cpu_us_per_msg=(usage.ru_utime + usage.ru_stime) / total * 1e6,
        peak_rss_mb=usage.ru_maxrss / (1024 * 1024),  # macOS reports bytes
        failures=failures,
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--rounds", type=int, default=7)
    parser.add_argument("--base-port", type=int, default=21000)
    parser.add_argument("--out", default="results/results.json")
    args = parser.parse_args()

    port = args.base_port
    results = []
    for scenario in SCENARIOS:
        runs = {name: [] for name in SERVERS}
        for round_no in range(args.rounds):
            order = list(SERVERS)
            order = order[round_no % len(order):] + order[:round_no % len(order)]
            for name in order:
                port += 1
                run = run_once(name, scenario, port)
                runs[name].append(run)
                note = f" FAIL {run['failures']}" if run["failures"] else ""
                print(f"{scenario['name']:<36} {name:<8} {run['msgs_per_sec']:>10.0f} msg/s{note}", flush=True)
                time.sleep(0.2)
        summary = {}
        for name, rs in runs.items():
            rate = [r["msgs_per_sec"] for r in rs]
            summary[name] = dict(
                msgs_per_sec_median=statistics.median(rate),
                msgs_per_sec_min=min(rate),
                msgs_per_sec_max=max(rate),
                server_cpu_us_per_msg_median=statistics.median(r["server_cpu_us_per_msg"] for r in rs),
                peak_rss_mb_median=statistics.median(r["peak_rss_mb"] for r in rs),
                failures=sum(len(r["failures"]) for r in rs),
            )
        results.append(dict(scenario=scenario, summary=summary, runs=runs))

    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as f:
        json.dump(dict(machine=platform.platform(), rounds=args.rounds, results=results), f, indent=1)

    print()
    for result in results:
        print(result["scenario"]["name"])
        for name, s in result["summary"].items():
            print(f"  {name:<8} {s['msgs_per_sec_median']:>9.0f} msg/s "
                  f"[{s['msgs_per_sec_min']:.0f}-{s['msgs_per_sec_max']:.0f}]  "
                  f"cpu {s['server_cpu_us_per_msg_median']:>8.1f} us/msg  "
                  f"rss {s['peak_rss_mb_median']:>6.1f} MB  failures {s['failures']}")


if __name__ == "__main__":
    main()
