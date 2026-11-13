#!/usr/bin/env python3
"""M2.1 runtime smoke test for the lan-mdns single binary.

Pure standard library. Starts the release binary on an *isolated* multicast
group/port so no real mDNS traffic ever leaves the host, then asserts the
shared API envelope, the mDNS record set, Prometheus metrics and a genuine
query/answer/probe loopback. Optionally verifies Admin Token auth on a second
instance.

Environment:
  LAN_MDNS_BIN      path to binary   (default target/release/lan-mdns)
  LAN_STUDIO_PORT   HTTP port        (default 18080)
  LAN_MDNS_PORT     mDNS UDP port    (default 15353)
  LAN_MDNS_GROUP    multicast group  (default 239.1.2.3)
  LAN_STUDIO_CONFIG config file      (default components/lan-mdns/config.template.yaml)
"""

import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, os.getenv("LAN_MDNS_BIN", "target/release/lan-mdns"))
CONFIG = os.getenv(
    "LAN_STUDIO_CONFIG", "components/lan-mdns/config.template.yaml"
)
HTTP_PORT = int(os.getenv("LAN_STUDIO_PORT", "18080"))
MDNS_PORT = int(os.getenv("LAN_MDNS_PORT", "15353"))
MDNS_GROUP = os.getenv("LAN_MDNS_GROUP", "239.1.2.3")
BASE = "http://127.0.0.1:{}".format(HTTP_PORT)

OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))

RESULTS = []


def check(name, condition, detail=""):
    RESULTS.append((name, bool(condition), detail))
    flag = "PASS" if condition else "FAIL"
    print("{}  {}{}".format(flag, name, ("  | " + detail) if detail else ""))


def http(method, path, body=None, token=None, base=BASE, timeout=6):
    url = base + path
    data = None
    headers = {}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if token is not None:
        headers["X-Admin-Token"] = token
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with OPENER.open(req, timeout=timeout) as resp:
            raw = resp.read().decode()
            return resp.status, dict(resp.headers), raw
    except urllib.error.HTTPError as exc:
        return exc.code, dict(exc.headers), exc.read().decode()
    except (urllib.error.URLError, TimeoutError, OSError):
        # Server not up yet (or transient); wait_ready retries.
        return 0, {}, ""


def as_json(raw):
    try:
        return json.loads(raw)
    except ValueError:
        return {}


def wait_ready(base, seconds=15):
    deadline = time.time() + seconds
    while time.time() < deadline:
        status, _, raw = http("GET", "/healthz", base=base)
        if status == 200 and as_json(raw).get("ok") is True:
            return True
        time.sleep(0.25)
    return False


def start_instance(config_path, http_port, mdns_port):
    env = dict(os.environ)
    env.update(
        {
            "LAN_STUDIO_CONFIG": config_path,
            "LAN_STUDIO_PORT": str(http_port),
            "LAN_MDNS_PORT": str(mdns_port),
            "LAN_MDNS_GROUP": MDNS_GROUP,
        }
    )
    log = tempfile.NamedTemporaryFile(
        mode="w+", prefix="lanmdns-smoke-", suffix=".log", delete=False
    )
    proc = subprocess.Popen([BIN], cwd=ROOT, env=env, stdout=log, stderr=log)
    return proc, log


def stop_instance(proc, log):
    if proc.poll() is None:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
    log.flush()
    with open(log.name, "r", errors="replace") as handle:
        text = handle.read()
    return text


def main():
    if not os.path.isfile(BIN):
        print("FAIL  binary missing: {}".format(BIN))
        return 1

    proc, log = start_instance(CONFIG, HTTP_PORT, MDNS_PORT)
    try:
        ready = wait_ready(BASE)
        check("00 server becomes ready", ready, BASE)
        if not ready:
            print(stop_instance(proc, log))
            return 1

        # --- liveness / readiness -----------------------------------------
        status, _, raw = http("GET", "/healthz")
        check("01 /healthz 200", status == 200)
        check("02 /healthz ok:true", as_json(raw).get("ok") is True)

        status, _, raw = http("GET", "/readyz")
        body = as_json(raw)
        check("03 /readyz 200", status == 200)
        check("04 /readyz ok:true", body.get("ok") is True)
        check("05 /readyz ready:true", body.get("data", {}).get("ready") is True)

        # --- status --------------------------------------------------------
        status, _, raw = http("GET", "/api/status")
        body = as_json(raw)
        data = body.get("data", {})
        check("06 /api/status 200", status == 200)
        check("07 ok envelope", body.get("ok") is True)
        check("08 request_id present", bool(body.get("request_id")))
        check("09 protocol mdns/1", data.get("protocol") == "mdns/1")
        check("10 component lan-mdns", data.get("component") == "lan-mdns")
        check("11 records published", data.get("records") == 8,
              "records={}".format(data.get("records")))
        check("12 isolated group", data.get("group") == MDNS_GROUP,
              "group={}".format(data.get("group")))
        check("13 isolated port", data.get("mdns_port") == MDNS_PORT,
              "port={}".format(data.get("mdns_port")))

        # --- records -------------------------------------------------------
        status, _, raw = http("GET", "/api/records")
        body = as_json(raw)
        records = body.get("data", {}).get("records", [])
        types = {r.get("type") for r in records}
        check("14 /api/records 200", status == 200)
        check("15 records total 8", body.get("data", {}).get("total") == 8)
        check("16 A/AAAA/PTR/SRV/TXT all present",
              {"A", "AAAA", "PTR", "SRV", "TXT"}.issubset(types), str(sorted(types)))
        check("17 IPv4 reverse PTR published",
              any(r.get("name") == "10.5.168.192.in-addr.arpa" for r in records))
        check("18 service type enumeration published",
              any(r.get("name") == "_services._dns-sd._udp.local" for r in records))
        check("19 cache_flush bit on authoritative A",
              any(r.get("name") == "nas.local" and r.get("type") == "A"
                  and r.get("cache_flush") is True for r in records))

        # --- interfaces ----------------------------------------------------
        status, _, raw = http("GET", "/api/interfaces")
        body = as_json(raw)
        ifaces = body.get("data", {}).get("interfaces", [])
        check("20 /api/interfaces 200", status == 200)
        check("21 at least one interface", len(ifaces) >= 1)
        check("22 interface shape",
              all("name" in i and "addr" in i for i in ifaces))

        # --- metrics -------------------------------------------------------
        status, headers, raw = http("GET", "/api/metrics")
        check("23 /api/metrics 200", status == 200)
        check("24 Prometheus 0.0.4 content-type",
              "0.0.4" in headers.get("Content-Type", ""))
        mandatory = [
            "lanstudio_mdns_up", "lanstudio_mdns_uptime_seconds",
            "lanstudio_mdns_build_info", "lanstudio_mdns_config_reloads_total",
            "lanstudio_mdns_config_saves_total",
            "lanstudio_mdns_http_requests_total",
            "lanstudio_mdns_alerts_sent_total", "lanstudio_mdns_log_disk_bytes",
            "lanstudio_mdns_log_disk_limit_bytes",
        ]
        missing = [m for m in mandatory if m not in raw]
        check("25 nine mandatory metric families", not missing, str(missing))
        mdns_specific = [
            "lanstudio_mdns_records", "lanstudio_mdns_queries_total",
            "lanstudio_mdns_responses_total", "lanstudio_mdns_announces_total",
            "lanstudio_mdns_probe_total", "lanstudio_mdns_query_audit_entries",
            "lanstudio_mdns_browse_instances",
        ]
        missing = [m for m in mdns_specific if m not in raw]
        check("26 seven mdns metric families", not missing, str(missing))
        check("27 no double-prefixed metrics", "lanstudio_mdns_mdns_" not in raw)
        check("28 records gauge labelled by type",
              'lanstudio_mdns_records{type="A"}' in raw)

        # --- empty audit / browse -----------------------------------------
        for idx, (path, key) in enumerate(
            [("/api/queries", "queries"), ("/api/browse", "browse")]
        ):
            status, _, raw = http("GET", path)
            body = as_json(raw)
            check("{} {} 200".format(29 + idx * 2, path), status == 200)
            check("{} {} bounded array".format(30 + idx * 2, path),
                  isinstance(body.get("data", {}).get(key), list))

        # --- active probe: real multicast query/answer loopback ------------
        status, _, raw = http(
            "POST", "/api/probe", {"name": "nas.local", "type": "A"}
        )
        body = as_json(raw)
        answers = body.get("data", {}).get("answers", [])
        check("33 /api/probe A 200", status == 200)
        check("34 probe A returns our published A record",
              any(a.get("name") == "nas.local" and a.get("type") == "A"
                  and a.get("value") == "192.168.5.10" for a in answers),
              str(answers))

        status, _, raw = http(
            "POST", "/api/probe",
            {"name": "_http._tcp.local", "type": "PTR"}
        )
        answers = as_json(raw).get("data", {}).get("answers", [])
        check("35 probe PTR returns service instance",
              any(a.get("name") == "_http._tcp.local" and a.get("type") == "PTR"
                  for a in answers), str(answers))

        status, _, raw = http(
            "POST", "/api/probe",
            {"name": "nas.local", "type": "AAAA", "unicast": True}
        )
        answers = as_json(raw).get("data", {}).get("answers", [])
        check("36 QU unicast probe returns AAAA",
              any(a.get("name") == "nas.local" and a.get("type") == "AAAA"
                  and a.get("value") == "fe80::1" for a in answers),
              str(answers))

        status, _, raw = http("POST", "/api/probe", {"type": "A"})
        body = as_json(raw)
        check("37 probe without name -> 400", status == 400)
        check("38 probe without name -> invalid_config",
              body.get("error", {}).get("code") == "invalid_config")

        status, _, raw = http("POST", "/api/probe", {"name": "bad"})
        body = as_json(raw)
        check("39 invalid name -> 400", status == 400)
        check("40 invalid name -> invalid_config",
              body.get("error", {}).get("code") == "invalid_config")

        # --- engine counters advanced --------------------------------------
        status, _, raw = http("GET", "/api/status")
        data = as_json(raw).get("data", {})
        check("41 queries counted", data.get("queries", 0) >= 3,
              "queries={}".format(data.get("queries")))
        check("42 responses counted", data.get("responses", 0) >= 3,
              "responses={}".format(data.get("responses")))

        status, _, raw = http("GET", "/api/queries?limit=1000")
        body = as_json(raw)
        check("43 query audit populated",
              body.get("data", {}).get("total", 0) >= 3,
              "total={}".format(body.get("data", {}).get("total")))
        check("44 query audit entries shaped",
              all("name" in e and "qtype" in e and "source" in e
                  for e in body.get("data", {}).get("queries", [])))

        # --- webui / config / auth -----------------------------------------
        status, _, raw = http("GET", "/")
        check("45 WebUI 200", status == 200)
        check("46 WebUI contains Lan Studio", "Lan Studio" in raw)

        status, _, raw = http("GET", "/api/config")
        check("47 /api/config 200", status == 200)
        check("48 config raw is the YAML",
              "component: lan-mdns" in as_json(raw).get("data", {}).get("raw", ""))

        status, _, raw = http("GET", "/api/auth")
        check("49 /api/auth 200", status == 200)
        check("50 token_required false by default",
              as_json(raw).get("data", {}).get("token_required") is False)

        status, _, _ = http("GET", "/api/does-not-exist")
        check("51 unknown API 404", status == 404)

        # --- cli ------------------------------------------------------------
        cli = subprocess.run([BIN, "--cli"], cwd=ROOT, capture_output=True,
                             text=True, env=dict(os.environ, LAN_STUDIO_CONFIG=CONFIG,
                                                 LAN_MDNS_PORT=str(MDNS_PORT),
                                                 LAN_MDNS_GROUP=MDNS_GROUP))
        check("52 --cli exits 0", cli.returncode == 0)
        check("53 --cli validates config offline",
              "component=lan-mdns" in cli.stdout and "records=8" in cli.stdout,
              cli.stdout.strip().splitlines()[1] if len(cli.stdout.splitlines()) > 1 else "")

        text = stop_instance(proc, log)
        check("54 graceful shutdown sends goodbye", "goodbye sent" in text,
              text.strip().splitlines()[-1] if text.strip() else "empty log")
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5)
        try:
            os.unlink(log.name)
        except OSError:
            pass

    # --- Admin Token instance (M1 auth regression) -------------------------
    token_cfg = tempfile.NamedTemporaryFile(
        mode="w", prefix="lanmdns-token-", suffix=".yaml", delete=False
    )
    with open(os.path.join(ROOT, CONFIG), "r") as handle:
        token_cfg.write(handle.read().replace('admin_token: ""',
                                              'admin_token: "smoke-token"'))
    token_cfg.close()
    proc2, log2 = start_instance(token_cfg.name, HTTP_PORT + 1, MDNS_PORT + 1)
    try:
        base2 = "http://127.0.0.1:{}".format(HTTP_PORT + 1)
        check("55 token instance ready", wait_ready(base2))
        status, _, raw = http("GET", "/api/status", base=base2)
        check("56 no token -> 401", status == 401)
        check("57 no token -> auth_required",
              as_json(raw).get("error", {}).get("code") == "auth_required")
        status, _, raw = http("GET", "/api/status", token="wrong", base=base2)
        check("58 wrong token -> 403", status == 403)
        check("59 wrong token -> auth_failed",
              as_json(raw).get("error", {}).get("code") == "auth_failed")
        status, _, _ = http("GET", "/api/status", token="smoke-token", base=base2)
        check("60 correct token -> 200", status == 200)
        status, _, _ = http("GET", "/healthz", token="wrong", base=base2)
        check("61 healthz bypasses auth", status == 200)
    finally:
        stop_instance(proc2, log2)
        if proc2.poll() is None:
            proc2.kill()
            proc2.wait(timeout=5)
        try:
            os.unlink(token_cfg.name)
            os.unlink(log2.name)
        except OSError:
            pass

    passed = sum(1 for _, ok, _ in RESULTS if ok)
    failed = [name for name, ok, _ in RESULTS if not ok]
    print("\n{} passed / {} failed".format(passed, len(failed)))
    if failed:
        print("FAILED: " + ", ".join(failed))
        return 1
    print("SMOKE OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
