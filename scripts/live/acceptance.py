#!/usr/bin/env python3
"""Credential-gated live acceptance against an already-running Gemini Bridge."""

import argparse
import base64
import json
import os
import struct
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

ACK = "I_ACCEPT_ACCOUNT_RISK"
MINIMUMS = {"chat": 50, "image": 20, "image_references": 5, "tool": 10}


def fail(message, code=2):
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(code)


def request(base, key, method, path, payload=None, timeout=120):
    headers = {"Authorization": f"Bearer {key}"}
    data = None
    if payload is not None:
        data = json.dumps(payload).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    started = time.monotonic()
    try:
        response = urllib.request.urlopen(req, timeout=timeout)
        body = response.read()
        return response.status, response.headers, body, time.monotonic() - started
    except urllib.error.HTTPError as error:
        return error.code, error.headers, error.read(), time.monotonic() - started




def json_body(raw):
    try:
        return json.loads(raw)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return None


def image_dimensions(raw):
    if len(raw) >= 24 and raw.startswith(b"\x89PNG\r\n\x1a\n"):
        return "png", struct.unpack(">II", raw[16:24])
    if len(raw) >= 10 and raw[:6] in (b"GIF87a", b"GIF89a"):
        return "gif", struct.unpack("<HH", raw[6:10])
    if len(raw) >= 30 and raw.startswith(b"RIFF") and raw[8:12] == b"WEBP":
        if raw[12:16] == b"VP8X":
            return "webp", (1 + int.from_bytes(raw[24:27], "little"), 1 + int.from_bytes(raw[27:30], "little"))
        if raw[12:16] == b"VP8 " and raw[23:26] == b"\x9d\x01\x2a":
            return "webp", (int.from_bytes(raw[26:28], "little") & 0x3FFF, int.from_bytes(raw[28:30], "little") & 0x3FFF)
    if raw.startswith(b"\xff\xd8"):
        offset = 2
        while offset + 9 < len(raw):
            if raw[offset] != 0xFF:
                offset += 1
                continue
            marker = raw[offset + 1]
            if marker in range(0xC0, 0xC4):
                return "jpeg", (int.from_bytes(raw[offset + 7:offset + 9], "big"), int.from_bytes(raw[offset + 5:offset + 7], "big"))
            if marker in (0xD8, 0xD9):
                offset += 2
            else:
                if offset + 4 > len(raw):
                    break
                offset += 2 + int.from_bytes(raw[offset + 2:offset + 4], "big")
    return None


def run_chat(base, key, model, case, timeout):
    payload = {"model": case.get("model", model), "messages": [{"role": "user", "content": case["prompt"]}], "stream": bool(case.get("stream", True))}
    if not payload["stream"]:
        status, _, raw, elapsed = request(base, key, "POST", "/v1/chat/completions", payload, timeout)
        body = json_body(raw)
        content = body.get("choices", [{}])[0].get("message", {}).get("content") if isinstance(body, dict) else None
        return status == 200 and isinstance(content, str) and bool(content.strip()), status, elapsed, None

    headers = {"Authorization": f"Bearer {key}", "Content-Type": "application/json"}
    req = urllib.request.Request(base + "/v1/chat/completions", data=json.dumps(payload).encode(), headers=headers, method="POST")
    started = time.monotonic()
    first = None
    content = []
    done = False
    status = 0
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            status = response.status
            for raw_line in response:
                line = raw_line.decode("utf-8", "replace").strip()
                if not line.startswith("data:"):
                    continue
                value = line[5:].strip()
                if value == "[DONE]":
                    done = True
                    break
                event = json_body(value.encode())
                delta = event.get("choices", [{}])[0].get("delta", {}).get("content") if isinstance(event, dict) else None
                if isinstance(delta, str) and delta:
                    if first is None:
                        first = time.monotonic() - started
                    content.append(delta)
    except urllib.error.HTTPError as error:
        status = error.code
    elapsed = time.monotonic() - started
    return status == 200 and done and bool("".join(content).strip()), status, elapsed, first


def run_image(base, key, model, case, timeout):
    references = []
    for path in case.get("reference_files", []):
        raw = Path(path).read_bytes()
        mime = "image/png" if raw.startswith(b"\x89PNG") else "image/jpeg"
        references.append({"b64_json": base64.b64encode(raw).decode(), "mime_type": mime})
    payload = {"model": case.get("model", model), "prompt": case["prompt"], "n": 1, "response_format": "b64_json"}
    if references:
        payload["reference_images"] = references
    status, _, raw, elapsed = request(base, key, "POST", "/v1/images/generations", payload, timeout)
    body = json_body(raw)
    encoded = body.get("data", [{}])[0].get("b64_json") if isinstance(body, dict) else None
    try:
        image = base64.b64decode(encoded, validate=True) if isinstance(encoded, str) else b""
    except ValueError:
        image = b""
    dimensions = image_dimensions(image)
    valid = status == 200 and dimensions is not None and dimensions[1][0] > 0 and dimensions[1][1] > 0
    details = {"format": dimensions[0], "width": dimensions[1][0], "height": dimensions[1][1]} if dimensions else {"reason": "invalid_image"}
    return valid, status, elapsed, details


def run_tool(base, key, model, case, timeout):
    payload = {
        "model": case.get("model", model), "messages": [{"role": "user", "content": case["prompt"]}],
        "stream": False, "tools": case["tools"], "tool_choice": case.get("tool_choice", "auto"),
    }
    status, _, raw, elapsed = request(base, key, "POST", "/v1/chat/completions", payload, timeout)
    body = json_body(raw)
    calls = body.get("choices", [{}])[0].get("message", {}).get("tool_calls", []) if isinstance(body, dict) else []
    valid = status == 200 and bool(calls)
    expected = case.get("expected_tool")
    parsed = []
    for call in calls if isinstance(calls, list) else []:
        function = call.get("function", {})
        try:
            arguments = json.loads(function.get("arguments", ""))
        except (json.JSONDecodeError, TypeError):
            valid = False
            arguments = None
        name = function.get("name")
        declared = {tool.get("function", {}).get("name") for tool in case["tools"]}
        valid = valid and name in declared and (expected is None or name == expected)
        parsed.append({"name": name, "arguments_valid_json": arguments is not None})
    return valid, status, elapsed, {"calls": parsed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("config", help="absolute path to external JSON case configuration")
    parser.add_argument("output", help="machine-readable JSON result path")
    parser.add_argument("--suite", choices=("all", "chat", "image", "tool"), default="all")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    output = Path(args.output).expanduser().resolve()
    if output == repo or repo in output.parents:
        fail("result output must be retained outside the repository")
    if not output.parent.is_dir():
        fail("result output parent directory must already exist")
    if os.environ.get("GEMINI_BRIDGE_LIVE") != ACK:
        fail(f"set GEMINI_BRIDGE_LIVE={ACK} to acknowledge live-account risk")
    key = os.environ.get("LIVE_BRIDGE_API_KEY", "")
    if not key:
        fail("LIVE_BRIDGE_API_KEY is required")
    config_path = Path(args.config).expanduser().resolve()
    if not config_path.is_file() or config_path == repo or repo in config_path.parents:
        fail("config must be an existing file outside the repository")
    try:
        config = json.loads(config_path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read config: {error}")
    if "api_key" in config:
        fail("config must not contain api_key; use LIVE_BRIDGE_API_KEY")
    base = str(config.get("base_url", "")).rstrip("/")
    parsed = urllib.parse.urlparse(base)
    if parsed.scheme not in ("http", "https") or not parsed.netloc:
        fail("config.base_url must be an http(s) URL")
    model = config.get("model")
    if not isinstance(model, str) or not model:
        fail("config.model is required")
    try:
        timeout = int(config.get("timeout_seconds", 180))
    except (TypeError, ValueError):
        fail("config.timeout_seconds must be an integer")
    if timeout < 1:
        fail("config.timeout_seconds must be positive")
    selected = ("chat", "image", "tool") if args.suite == "all" else (args.suite,)
    for suite in selected:
        cases = config.get(suite)
        if not isinstance(cases, list) or len(cases) < MINIMUMS[suite]:
            fail(f"config.{suite} requires at least {MINIMUMS[suite]} cases")
        if any(not isinstance(case, dict) or not isinstance(case.get("prompt"), str) or not case["prompt"].strip() for case in cases):
            fail(f"every config.{suite} case needs a non-empty prompt")
        if suite in ("chat", "image") and len({case["prompt"] for case in cases}) != len(cases):
            fail(f"config.{suite} prompts must be distinct")
        if suite == "tool" and any(not isinstance(case.get("tools"), list) or not case["tools"] for case in cases):
            fail("every config.tool case needs at least one declared function tool")
    if "image" in selected:
        reference_count = sum(bool(case.get("reference_files")) for case in config["image"])
        if reference_count < MINIMUMS["image_references"]:
            fail(f"config.image requires at least {MINIMUMS['image_references']} cases with reference_files")
        for case in config["image"]:
            for path in case.get("reference_files", []):
                file_path = Path(path).expanduser().resolve()
                if not file_path.is_file() or file_path == repo or repo in file_path.parents:
                    fail("every reference file must exist outside the repository")
    try:
        status, _, _, _ = request(base, key, "GET", "/healthz", timeout=10)
        if status // 100 != 2:
            fail(f"target health check failed with HTTP {status}; no suite was run")
        status, _, _, _ = request(base, key, "GET", "/readyz", timeout=10)
        if status // 100 != 2:
            fail(f"target readiness check failed with HTTP {status}; no suite was run")
    except (OSError, urllib.error.URLError) as error:
        fail(f"target service unavailable; no suite was run ({type(error).__name__})")

    started = datetime.now(timezone.utc)
    results = []
    runners = {"chat": run_chat, "image": run_image, "tool": run_tool}
    for suite in selected:
        for index, case in enumerate(config[suite], 1):
            try:
                if suite == "chat":
                    passed, http_status, elapsed, first = runners[suite](base, key, model, case, timeout)
                    detail = {"first_event_ms": round(first * 1000, 3) if first is not None else None}
                else:
                    passed, http_status, elapsed, detail = runners[suite](base, key, model, case, timeout)
            except Exception as error:  # preserve all case outcomes in the retained report
                passed, http_status, elapsed, detail = False, 0, 0.0, {"error_type": type(error).__name__}
            results.append({"suite": suite, "case": index, "passed": bool(passed), "http_status": http_status, "total_ms": round(elapsed * 1000, 3), **detail})
            print(f"{suite} {index}/{len(config[suite])}: {'PASS' if passed else 'FAIL'}")

    summaries = {}
    for suite in selected:
        rows = [row for row in results if row["suite"] == suite]
        passed = sum(row["passed"] for row in rows)
        rate = passed * 100.0 / len(rows)
        threshold = 100.0 if suite == "image" else 95.0
        summaries[suite] = {"passed": passed, "total": len(rows), "pass_rate_percent": round(rate, 3), "threshold_percent": threshold, "threshold_met": rate >= threshold}
    total_passed = sum(row["passed"] for row in results)
    aggregate_rate = total_passed * 100.0 / len(results)
    passed = aggregate_rate >= 95.0 and all(item["threshold_met"] for item in summaries.values())
    report = {
        "schema_version": 1, "kind": "gemini_bridge_live_acceptance", "started_at": started.isoformat(),
        "finished_at": datetime.now(timezone.utc).isoformat(), "base_url": base, "model": model,
        "suite": args.suite, "summaries": summaries,
        "aggregate": {"passed": total_passed, "total": len(results), "pass_rate_percent": round(aggregate_rate, 3), "threshold_percent": 95.0, "threshold_met": passed},
        "results": results,
    }
    temporary = output.with_name(output.name + ".tmp")
    temporary.write_text(json.dumps(report, indent=2) + "\n")
    temporary.replace(output)
    print(f"result={output}")
    if not passed:
        fail("one or more live acceptance thresholds were not met", 1)


if __name__ == "__main__":
    main()
