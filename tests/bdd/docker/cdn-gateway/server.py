#!/usr/bin/env python3
"""Origin (:8080) and Cross-Origin Object-Store CDN (:8081) servers for BDD scenarios."""

from __future__ import annotations

import json
import threading
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote

RELEASES_ROOT = Path("/srv/releases")


class GatewayState:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.mode = "redirect"
        self.cdn_host = "127.0.0.1"
        self.token_max_hits = 1
        self.origin_requests = 0
        self.origin_auth_hits = 0
        self.origin_cookie_hits = 0
        self.redirect_count = 0
        self.cdn_requests = 0
        self.cdn_single_206 = 0
        self.cdn_multi_200 = 0
        self.cdn_leaked_credentials = 0
        self.v1_token_hits = 0
        self.v2_token_hits = 0

    def configure(self, payload: dict) -> None:
        with self.lock:
            if "mode" in payload:
                self.mode = str(payload["mode"])
            if "cdn_host" in payload:
                self.cdn_host = str(payload["cdn_host"])
            if "token_max_hits" in payload:
                self.token_max_hits = int(payload["token_max_hits"])

    def snapshot(self) -> dict:
        with self.lock:
            return {
                "mode": self.mode,
                "origin_requests": self.origin_requests,
                "origin_auth_hits": self.origin_auth_hits,
                "origin_cookie_hits": self.origin_cookie_hits,
                "redirect_count": self.redirect_count,
                "cdn_requests": self.cdn_requests,
                "cdn_single_206": self.cdn_single_206,
                "cdn_multi_200": self.cdn_multi_200,
                "cdn_leaked_credentials": self.cdn_leaked_credentials,
                "v1_token_hits": self.v1_token_hits,
                "v2_token_hits": self.v2_token_hits,
            }


STATE = GatewayState()


def serve_file_with_mode(handler: BaseHTTPRequestHandler, file_path: Path, mode: str) -> None:
    if not file_path.is_file():
        handler.send_error(HTTPStatus.NOT_FOUND, "Not found")
        return

    data = file_path.read_bytes()
    total = len(data)
    range_hdr = handler.headers.get("Range")

    if mode == "error_500":
        handler.send_error(HTTPStatus.INTERNAL_SERVER_ERROR, "Simulated origin failure")
        return

    if mode == "accept_ranges_none":
        handler.send_response(HTTPStatus.OK)
        handler.send_header("Accept-Ranges", "none")
        handler.send_header("Content-Length", str(total))
        handler.end_headers()
        handler.wfile.write(data)
        return

    if not range_hdr or not range_hdr.startswith("bytes="):
        handler.send_response(HTTPStatus.OK)
        handler.send_header("Accept-Ranges", "bytes")
        handler.send_header("Content-Length", str(total))
        handler.end_headers()
        handler.wfile.write(data)
        return

    spec = range_hdr[len("bytes=") :].strip()
    if "," in spec:
        # Simulate AWS S3 / Cloudflare R2 / GitHub CDN returning 200 OK on multi-range requests.
        with STATE.lock:
            STATE.cdn_multi_200 += 1
        handler.send_response(HTTPStatus.OK)
        handler.send_header("Accept-Ranges", "bytes")
        handler.send_header("Content-Length", str(total))
        handler.end_headers()
        handler.wfile.write(data)
        return

    start_s, _, end_s = spec.partition("-")
    start = int(start_s)
    end = int(end_s)
    if start > end or end >= total:
        handler.send_error(HTTPStatus.REQUESTED_RANGE_NOT_SATISFIABLE, "Out of bounds")
        return

    chunk = data[start : end + 1]
    with STATE.lock:
        STATE.cdn_single_206 += 1
    handler.send_response(HTTPStatus.PARTIAL_CONTENT)
    handler.send_header("Accept-Ranges", "bytes")
    handler.send_header("Content-Range", f"bytes {start}-{end}/{total}")
    handler.send_header("Content-Length", str(len(chunk)))
    handler.end_headers()
    handler.wfile.write(chunk)


class OriginHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt: str, *args) -> None:
        return

    def do_GET(self) -> None:
        path = unquote(self.path.split("?", 1)[0])
        if path == "/healthz":
            self._send_json(HTTPStatus.OK, {"ok": True})
            return
        if path == "/admin/telemetry":
            self._send_json(HTTPStatus.OK, STATE.snapshot())
            return

        if not path.startswith("/releases/"):
            self.send_error(HTTPStatus.NOT_FOUND)
            return

        rel = path[len("/releases/") :]
        file_path = (RELEASES_ROOT / rel).resolve()
        if not str(file_path).startswith(str(RELEASES_ROOT.resolve())):
            self.send_error(HTTPStatus.FORBIDDEN)
            return

        with STATE.lock:
            STATE.origin_requests += 1
            if self.headers.get("Authorization"):
                STATE.origin_auth_hits += 1
            if self.headers.get("Cookie"):
                STATE.origin_cookie_hits += 1
            mode = STATE.mode
            cdn_host = STATE.cdn_host

        is_metadata = rel.endswith((".json", ".yml", ".yaml", ".blockmap"))
        if is_metadata:
            if mode == "error_500":
                self.send_error(HTTPStatus.INTERNAL_SERVER_ERROR, "Origin down")
                return
            serve_file_with_mode(self, file_path, "direct")
            return

        if mode == "redirect":
            with STATE.lock:
                hit_index = STATE.redirect_count
                STATE.redirect_count += 1
            token = "signed-v1" if hit_index == 0 else "signed-v2"
            location = f"http://{cdn_host}:8081/cdn/{token}/{rel}"
            self.send_response(HTTPStatus.FOUND)
            self.send_header("Location", location)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return

        serve_file_with_mode(self, file_path, mode)

    def do_POST(self) -> None:
        if self.path != "/admin/config":
            self.send_error(HTTPStatus.NOT_FOUND)
            return
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length).decode("utf-8"))
        STATE.configure(body)
        self._send_json(HTTPStatus.OK, STATE.snapshot())

    def _send_json(self, status: HTTPStatus, data: dict) -> None:
        raw = json.dumps(data).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)


class CdnHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt: str, *args) -> None:
        return

    def do_GET(self) -> None:
        path = unquote(self.path.split("?", 1)[0])
        if path == "/healthz":
            self.send_response(HTTPStatus.OK)
            self.end_headers()
            self.wfile.write(b"ok\n")
            return

        if self.headers.get("Authorization") or self.headers.get("Cookie"):
            with STATE.lock:
                STATE.cdn_leaked_credentials += 1
            self.send_error(HTTPStatus.BAD_REQUEST, "Cross-origin credentials must be stripped")
            return

        parts = [p for p in path.split("/") if p]
        if len(parts) < 3 or parts[0] != "cdn":
            self.send_error(HTTPStatus.NOT_FOUND)
            return

        token = parts[1]
        rel = "/".join(parts[2:])
        with STATE.lock:
            STATE.cdn_requests += 1
            max_hits = STATE.token_max_hits
            if token == "signed-v1":
                current = STATE.v1_token_hits
                STATE.v1_token_hits += 1
                if current >= max_hits:
                    self.send_error(HTTPStatus.FORBIDDEN, "Signed token expired")
                    return
            elif token == "signed-v2":
                STATE.v2_token_hits += 1
            else:
                self.send_error(HTTPStatus.FORBIDDEN, "Unknown token")
                return

        file_path = (RELEASES_ROOT / rel).resolve()
        if not str(file_path).startswith(str(RELEASES_ROOT.resolve())):
            self.send_error(HTTPStatus.FORBIDDEN)
            return
        serve_file_with_mode(self, file_path, "s3_single_only_200_on_multi")


def main() -> None:
    origin = ThreadingHTTPServer(("0.0.0.0", 8080), OriginHandler)
    cdn = ThreadingHTTPServer(("0.0.0.0", 8081), CdnHandler)
    t_origin = threading.Thread(target=origin.serve_forever, daemon=True)
    t_cdn = threading.Thread(target=cdn.serve_forever, daemon=True)
    t_origin.start()
    t_cdn.start()
    t_origin.join()
    t_cdn.join()


if __name__ == "__main__":
    main()
