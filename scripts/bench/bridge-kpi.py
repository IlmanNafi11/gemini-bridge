#!/usr/bin/env python3
"""Measure a real Gemini Bridge process: cold start, RSS, and concurrent SSE."""

import argparse
import json
import os
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path


def fail(message, code=2):
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(code)


def rss_kb(pid):
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except (FileNotFoundError, PermissionError, ValueError):
        pass
    fail(f"cannot read RSS for bridge PID {pid}; Linux /proc access is required")


def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    return ordered[int((len(ordered) - 1) * fraction)]


def request(url, key, payload=None, timeout=10):
    headers = {"Authorization": f"Bearer {key}"} if key else {}
    data = None
    if payload is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(payload).encode()
    req = urllib.request.Request(url, data=data, headers=headers, method="POST" if payload is not None else "GET")
    return urllib.request.urlopen(req, timeout=timeout)


def wait_for_health(base, proc, timeout):
    started = time.monotonic_ns()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            fail(f"bridge exited before health became available (status {proc.returncode})")
        try:
            with request(base + "/healthz", "", timeout=0.2) as response:
                if response.status // 100 == 2:
                    return (time.monotonic_ns() - started) / 1_000_000
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(0.005)
    fail(f"bridge did not serve /healthz within {timeout:g}s")


def terminate(proc):
    if proc.poll() is None:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, help="release gemini-bridge executable")
    parser.add_argument("--config", required=True, help="external TOML config with usable credentials")
    parser.add_argument("--base-url", required=True, help="URL matching config bind/port")
    parser.add_argument("--output", required=True, help="retained JSON result")
    parser.add_argument("--api-key", default="", help=argparse.SUPPRESS)
    parser.add_argument("--model", default="gemini-web-flash")
    parser.add_argument("--streams", type=int, default=10)
    parser.add_argument("--cold-runs", type=int, default=5)
    parser.add_argument("--idle-seconds", type=float, default=2.0)
    parser.add_argument("--timeout-seconds", type=float, default=180.0)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    output = Path(args.output).expanduser().resolve()
    binary = Path(args.binary).expanduser().resolve()
    config = Path(args.config).expanduser().resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        fail("--binary must be an executable file")
    if not config.is_file() or config == repo or repo in config.parents:
        fail("--config must be an existing file outside the repository")
    if output == repo or repo in output.parents or not output.parent.is_dir():
        fail("--output must have an existing parent outside the repository")
    if args.cold_runs < 1 or args.streams != 10 or args.idle_seconds < 0 or args.timeout_seconds <= 0:
        fail("--cold-runs must be positive, --streams exactly 10, --idle-seconds non-negative, and timeout positive")
    try:
        request(args.base_url.rstrip("/") + "/healthz", "", timeout=1).close()
        fail("target port is already serving; cold-start measurement requires an unused target")
    except urllib.error.HTTPError:
        fail("target URL already responds; cold-start measurement requires an unused target")
    except (OSError, urllib.error.URLError):
        pass

    import ipaddress
    from urllib.parse import urlparse
    parsed_base = urlparse(args.base_url)
    if parsed_base.scheme not in ("http", "https") or not parsed_base.hostname:
        fail("--base-url must be an http(s) URL")
    try:
        loopback = ipaddress.ip_address(parsed_base.hostname).is_loopback
    except ValueError:
        loopback = parsed_base.hostname == "localhost"
    if not loopback:
        fail("cold-start runs require a loopback target address")
    if args.api_key:
        fail("BRIDGE_BENCH_API_KEY must be unset; use an isolated loopback config without API-key auth")
    started_at = datetime.now(timezone.utc)
    cold_ms = []
    for _ in range(args.cold_runs):
        with tempfile.NamedTemporaryFile(prefix="gemini-bridge-kpi-", suffix=".log") as log:
            proc = subprocess.Popen([str(binary), "--config", str(config)], stdout=log, stderr=log)
            try:
                cold_ms.append(wait_for_health(args.base_url.rstrip("/"), proc, min(args.timeout_seconds, 30)))
            finally:
                terminate(proc)
        time.sleep(0.1)

    log = tempfile.NamedTemporaryFile(prefix="gemini-bridge-kpi-", suffix=".log", delete=False)
    proc = subprocess.Popen([str(binary), "--config", str(config)], stdout=log, stderr=log)
    try:
        wait_for_health(args.base_url.rstrip("/"), proc, min(args.timeout_seconds, 30))
        ready_url = args.base_url.rstrip("/") + "/readyz"
        try:
            with request(ready_url, args.api_key, timeout=5) as response:
                if response.status // 100 != 2:
                    fail(f"bridge is not ready (HTTP {response.status})")
        except urllib.error.HTTPError as error:
            fail(f"bridge is not ready (HTTP {error.code})")
        time.sleep(args.idle_seconds)
        idle = rss_kb(proc.pid)

        barrier = threading.Barrier(args.streams + 1)
        release = threading.Event()
        outcomes = [None] * args.streams
        payload = {"model": args.model, "messages": [{"role": "user", "content": "Write a detailed numbered explanation of the water cycle in at least 800 words."}], "stream": True}

        def stream(index):
            try:
                barrier.wait(timeout=10)
                started = time.monotonic()
                with request(args.base_url.rstrip("/") + "/v1/chat/completions", args.api_key, payload, args.timeout_seconds) as response:
                    first = None
                    done = False
                    release.wait(timeout=10)
                    for raw in response:
                        line = raw.decode("utf-8", "replace").strip()
                        if line.startswith("data:") and line[5:].strip() != "[DONE]" and first is None:
                            first = time.monotonic() - started
                        if line == "data: [DONE]":
                            done = True
                    outcomes[index] = {"ok": response.status == 200 and done and first is not None, "status": response.status, "first_event_ms": first * 1000 if first is not None else None, "total_ms": (time.monotonic() - started) * 1000}
            except Exception as error:
                outcomes[index] = {"ok": False, "status": 0, "error_type": type(error).__name__}

        threads = [threading.Thread(target=stream, args=(index,)) for index in range(args.streams)]
        for thread in threads:
            thread.start()
        barrier.wait(timeout=10)
        peak = idle
        deadline = time.monotonic() + args.timeout_seconds
        release_at = time.monotonic() + min(2.0, args.timeout_seconds / 2)
        released = False
        while any(thread.is_alive() for thread in threads):
            peak = max(peak, rss_kb(proc.pid))
            if not released and time.monotonic() >= release_at:
                release.set()
                released = True
            if time.monotonic() >= deadline:
                fail("concurrent stream measurement timed out")
            time.sleep(0.01)
        for thread in threads:
            thread.join()
        peak = max(peak, rss_kb(proc.pid))
    finally:
        terminate(proc)
        log.close()
        Path(log.name).unlink(missing_ok=True)

    successful = [row for row in outcomes if row and row.get("ok")]
    first_values = [row["first_event_ms"] for row in successful]
    total_values = [row["total_ms"] for row in successful]
    report = {
        "schema_version": 1, "kind": "gemini_bridge_kpi", "started_at": started_at.isoformat(),
        "finished_at": datetime.now(timezone.utc).isoformat(), "binary": str(binary), "base_url": args.base_url,
        "thresholds": {"cold_start_ms_max": 150, "idle_rss_kb_max": 25600, "ten_stream_rss_kb_max": 122880},
        "cold_start": {"runs_ms": [round(value, 3) for value in cold_ms], "median_ms": round(statistics.median(cold_ms), 3), "max_ms": round(max(cold_ms), 3), "threshold_met": max(cold_ms) <= 150},
        "memory": {"bridge_pid": proc.pid, "idle_rss_kb": idle, "ten_stream_peak_rss_kb": peak, "idle_threshold_met": idle <= 25600, "ten_stream_threshold_met": peak <= 122880},
        "streaming": {"requested": args.streams, "successful": len(successful), "first_event_p50_ms": round(percentile(first_values, .5), 3) if first_values else None, "first_event_p95_ms": round(percentile(first_values, .95), 3) if first_values else None, "total_p50_ms": round(percentile(total_values, .5), 3) if total_values else None, "total_p95_ms": round(percentile(total_values, .95), 3) if total_values else None, "results": outcomes},
        "caveat": "Streaming values include upstream network/model latency; bridge-only overhead requires internal upstream-chunk timestamps.",
    }
    report["thresholds_met"] = report["cold_start"]["threshold_met"] and report["memory"]["idle_threshold_met"] and report["memory"]["ten_stream_threshold_met"] and len(successful) == args.streams
    temp = output.with_name(output.name + ".tmp")
    temp.write_text(json.dumps(report, indent=2) + "\n")
    temp.replace(output)
    print(json.dumps({"result": str(output), "thresholds_met": report["thresholds_met"]}))
    if not report["thresholds_met"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
