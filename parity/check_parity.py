#!/usr/bin/env python3
"""Harness paritas Fase 0: bandingkan server Node (acuan) vs yuki-rs.

Pemakaian:
    python3 check_parity.py [url-node] [url-rust]

Default Node http://127.0.0.1:3101, Rust http://127.0.0.1:3102.
Membandingkan status, header penting, dan byte body untuk endpoint
Fase 0. /api/health dibandingkan bentuk JSON-nya saja karena uptime
pasti berbeda antar proses. 404 dibandingkan statusnya saja karena
body 404 bawaan Express (HTML) memang berbeda dari Rust.
"""
import json
import sys
import urllib.request
import urllib.error

NODE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:3101"
RUST = sys.argv[2] if len(sys.argv) > 2 else "http://127.0.0.1:3102"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


opener = urllib.request.build_opener(NoRedirect)


def fetch(base, path):
    req = urllib.request.Request(base + path, method="GET")
    try:
        with opener.open(req, timeout=10) as res:
            return res.status, dict(res.headers), res.read()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read()


# (path, mode) mode: bytes | status | health | redirect
CHECKS = [
    ("/api/health", "health"),
    ("/", "bytes"),
    ("/changelog", "bytes"),
    ("/docs", "bytes"),
    ("/admin", "bytes"),
    ("/privacy", "bytes"),
    ("/favicon.ico", "bytes"),
    ("/embed.html", "redirect"),
    ("/embed.html?foo=bar", "redirect"),
    ("/manifest.webmanifest", "bytes"),
    ("/backup.db", "status"),
    ("/halaman-tidak-ada", "status"),
    ("/api/endpoint-tidak-ada", "status"),
]

failures = 0
for path, mode in CHECKS:
    ns, nh, nb = fetch(NODE, path)
    rs, rh, rb = fetch(RUST, path)
    ok = ns == rs
    detail = f"node={ns} rust={rs}"
    if ok and mode == "bytes":
        ok = nb == rb
        detail += f" bytes node={len(nb)} rust={len(rb)} identical={nb == rb}"
    elif ok and mode == "health":
        try:
            nj, rj = json.loads(nb), json.loads(rb)
            ok = (
                nj.get("status") == rj.get("status") == "ok"
                and isinstance(nj.get("uptime"), int)
                and isinstance(rj.get("uptime"), int)
            )
            detail += f" json node={nj} rust={rj}"
        except Exception as e:  # noqa: BLE001
            ok = False
            detail += f" json-error {e}"
    elif ok and mode == "redirect":
        def loc(headers):
            for k, v in headers.items():
                if k.lower() == "location":
                    return v
            return None

        nl, rl = loc(nh), loc(rh)
        ok = nl == rl
        detail += f" location node={nl} rust={rl}"
    print(f"{'PASS' if ok else 'FAIL'}  {path:28s} {detail}")
    if not ok:
        failures += 1

print(f"\n{len(CHECKS) - failures}/{len(CHECKS)} PASS")
sys.exit(1 if failures else 0)
