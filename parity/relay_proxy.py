#!/usr/bin/env python3
"""Relay proxy lokal TANPA autentikasi -> egress proxy VM (yang URL-nya
membawa kredensial dari env). Dipakai hanya untuk verifikasi browser
Chromium di VM ini: Chromium tidak bisa memakai kredensial dari URL
--proxy-server, jadi Rust server diberi HTTPS_PROXY=http://127.0.0.1:18080.
Mendukung CONNECT (HTTPS) dan GET/POST http biasa."""
import os
import select
import socket
import sys
import threading
from urllib.parse import urlparse

UPSTREAM = os.environ.get("HTTPS_PROXY") or os.environ.get("https_proxy") or ""
LISTEN_PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18080


def upstream_addr():
    parsed = urlparse(UPSTREAM)
    return parsed.hostname, parsed.port or 3128


def proxy_auth_header():
    parsed = urlparse(UPSTREAM)
    if parsed.username:
        import base64

        creds = f"{parsed.username}:{parsed.password or ''}".encode()
        return "Proxy-Authorization: Basic " + base64.b64encode(creds).decode() + "\r\n"
    return ""


def pipe(a, b):
    try:
        while True:
            ready, _, _ = select.select([a], [], [], 30)
            if not ready:
                return
            data = a.recv(65536)
            if not data:
                return
            b.sendall(data)
    except Exception:
        return
    finally:
        for s in (a, b):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except Exception:
                pass


def handle(client):
    try:
        buf = b""
        while b"\r\n\r\n" not in buf:
            chunk = client.recv(4096)
            if not chunk:
                client.close()
                return
            buf += chunk
        head, _, rest = buf.partition(b"\r\n\r\n")
        lines = head.decode("latin-1").split("\r\n")
        method, target, _ = lines[0].split(" ", 2)
        host, port = upstream_addr()
        server = socket.create_connection((host, port), timeout=15)
        auth = proxy_auth_header()
        if method == "CONNECT":
            server.sendall(
                f"CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{auth}\r\n".encode()
            )
            resp = b""
            while b"\r\n\r\n" not in resp:
                chunk = server.recv(4096)
                if not chunk:
                    raise ConnectionError("upstream tutup")
                resp += chunk
            status = resp.split(b"\r\n", 1)[0]
            if b" 200 " not in status:
                client.sendall(status + b"\r\n\r\n")
                client.close()
                server.close()
                return
            client.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            if rest:
                server.sendall(rest)
            t = threading.Thread(target=pipe, args=(client, server), daemon=True)
            t.start()
            pipe(server, client)
        else:
            # HTTP biasa: teruskan request apa adanya + header auth.
            new_head = lines[0] + "\r\n" + auth
            others = [ln for ln in lines[1:] if not ln.lower().startswith("proxy-authorization")]
            new_head += "\r\n".join(others) + "\r\n\r\n"
            server.sendall(new_head.encode("latin-1") + rest)
            t = threading.Thread(target=pipe, args=(client, server), daemon=True)
            t.start()
            pipe(server, client)
    except Exception:
        try:
            client.close()
        except Exception:
            pass


def main():
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", LISTEN_PORT))
    srv.listen(50)
    print(f"relay di 127.0.0.1:{LISTEN_PORT}", flush=True)
    while True:
        client, _ = srv.accept()
        threading.Thread(target=handle, args=(client,), daemon=True).start()


if __name__ == "__main__":
    main()
