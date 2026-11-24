#!/usr/bin/env python3
"""Origin (:8080) and Cross-Origin Object-Store CDN (:8081) servers for BDD scenarios.

Supported modes:
- redirect: Origin 302-redirects artifacts to :8081 CDN with expiring signed tokens.
- s3_single_only_200_on_multi: Returns 200 OK on multi-range, 206 on single-range.
- accept_ranges_none: Advertises Accept-Ranges: none with 200 OK.
- error_500: Fails every request with 500.
- github_private: Mock GitHub Releases API (/repos/{o}/{r}/releases/latest + /assets/{id}) with 302 to CDN.
- s3_sigv4: Verifies real AWS4-HMAC-SHA256 SigV4 signature + STS token on every request.
- retry_503_and_api_key: Requires X-API-Key on origin, fails initial N requests with 503, 302-redirects to CDN.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import threading
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qsl, quote, unquote, urlsplit

RELEASES_ROOT = Path("/srv/releases")


class GatewayState:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.mode = "redirect"
        self.cdn_host = "127.0.0.1"
        self.token_max_hits = 1
        self.expected_token = "ghp_bdd_secret"
        self.expected_api_key = "bdd-api-key-secret"
        self.fail_503_remaining = 0
        self.sigv4_region = "us-east-1"
        self.sigv4_access_key = "AKIA_BDD_KEY"
        self.sigv4_secret_key = "BDD_SECRET_ACCESS_KEY"
        self.sigv4_session_token: str | None = "BDD_STS_SESSION_TOKEN"
        self.github_tag = "v0.2.0"
        self.github_assets: dict[int, str] = {}
        self.origin_requests = 0
        self.origin_auth_hits = 0
        self.origin_cookie_hits = 0
        self.origin_503_hits = 0
        self.redirect_count = 0
        self.cdn_requests = 0
        self.cdn_single_206 = 0
        self.cdn_multi_200 = 0
        self.cdn_leaked_credentials = 0
        self.v1_token_hits = 0
        self.v2_token_hits = 0
        self.sigv4_verified_requests = 0
        self.sigv4_rejected_requests = 0

    def configure(self, payload: dict) -> None:
        with self.lock:
            if "mode" in payload:
                self.mode = str(payload["mode"])
            if "cdn_host" in payload:
                self.cdn_host = str(payload["cdn_host"])
            if "token_max_hits" in payload:
                self.token_max_hits = int(payload["token_max_hits"])
            if "expected_token" in payload:
                self.expected_token = str(payload["expected_token"])
            if "expected_api_key" in payload:
                self.expected_api_key = str(payload["expected_api_key"])
            if "fail_503_count" in payload:
                self.fail_503_remaining = int(payload["fail_503_count"])
            if "sigv4_region" in payload:
                self.sigv4_region = str(payload["sigv4_region"])
            if "sigv4_access_key" in payload:
                self.sigv4_access_key = str(payload["sigv4_access_key"])
            if "sigv4_secret_key" in payload:
                self.sigv4_secret_key = str(payload["sigv4_secret_key"])
            if "sigv4_session_token" in payload:
                val = payload["sigv4_session_token"]
                self.sigv4_session_token = None if val is None else str(val)
            if "github_tag" in payload:
                self.github_tag = str(payload["github_tag"])
            if "github_assets" in payload:
                self.github_assets = {int(k): str(v) for k, v in payload["github_assets"].items()}

    def snapshot(self) -> dict:
        with self.lock:
            return {
                "mode": self.mode,
                "origin_requests": self.origin_requests,
                "origin_auth_hits": self.origin_auth_hits,
                "origin_cookie_hits": self.origin_cookie_hits,
                "origin_503_hits": self.origin_503_hits,
                "redirect_count": self.redirect_count,
                "cdn_requests": self.cdn_requests,
                "cdn_single_206": self.cdn_single_206,
                "cdn_multi_200": self.cdn_multi_200,
                "cdn_leaked_credentials": self.cdn_leaked_credentials,
                "v1_token_hits": self.v1_token_hits,
                "v2_token_hits": self.v2_token_hits,
                "sigv4_verified_requests": self.sigv4_verified_requests,
                "sigv4_rejected_requests": self.sigv4_rejected_requests,
            }


STATE = GatewayState()


def _hmac_sha256(key: bytes, msg: bytes) -> bytes:
    return hmac.new(key, msg, hashlib.sha256).digest()


def verify_aws_sigv4(handler: BaseHTTPRequestHandler) -> bool:
    auth = handler.headers.get("Authorization") or ""
    if not auth.startswith("AWS4-HMAC-SHA256 "):
        return False

    parts: dict[str, str] = {}
    for item in auth[len("AWS4-HMAC-SHA256 ") :].split(","):
        k, _, v = item.strip().partition("=")
        parts[k] = v

    credential = parts.get("Credential", "")
    signed_headers_str = parts.get("SignedHeaders", "")
    signature = parts.get("Signature", "")
    cred_parts = credential.split("/")
    if len(cred_parts) != 5:
        return False
    access_key, date_stamp, region, service, terminator = cred_parts

    with STATE.lock:
        expected_ak = STATE.sigv4_access_key
        expected_sk = STATE.sigv4_secret_key
        expected_region = STATE.sigv4_region
        expected_sts = STATE.sigv4_session_token

    if (
        access_key != expected_ak
        or region != expected_region
        or service != "s3"
        or terminator != "aws4_request"
    ):
        return False

    amz_date = handler.headers.get("x-amz-date") or ""
    if not amz_date.startswith(date_stamp):
        return False
    if (handler.headers.get("x-amz-content-sha256") or "") != "UNSIGNED-PAYLOAD":
        return False
    if (
        expected_sts is not None
        and (handler.headers.get("x-amz-security-token") or "") != expected_sts
    ):
        return False

    signed_names = [h.strip().lower() for h in signed_headers_str.split(";") if h.strip()]
    required_signed = {"host", "x-amz-content-sha256", "x-amz-date"}
    if expected_sts is not None:
        required_signed.add("x-amz-security-token")
    if signed_names != sorted(signed_names) or not required_signed.issubset(set(signed_names)):
        return False
    canonical_headers = ""
    for name in signed_names:
        val = (handler.headers.get(name) or "").strip()
        canonical_headers += f"{name}:{val}\n"

    split = urlsplit(handler.path)
    decoded_path = unquote(split.path) or "/"
    canonical_uri = quote(decoded_path, safe="/-_.~")
    query_pairs = sorted(
        (quote(k, safe="-_.~"), quote(v, safe="-_.~"))
        for k, v in parse_qsl(split.query, keep_blank_values=True)
    )
    canonical_query = "&".join(f"{k}={v}" for k, v in query_pairs)

    canonical_request = "\n".join(
        [
            handler.command,
            canonical_uri,
            canonical_query,
            canonical_headers,
            signed_headers_str,
            "UNSIGNED-PAYLOAD",
        ]
    )
    canonical_hash = hashlib.sha256(canonical_request.encode("utf-8")).hexdigest()
    scope = f"{date_stamp}/{region}/{service}/aws4_request"
    string_to_sign = f"AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{canonical_hash}"

    k_date = _hmac_sha256(f"AWS4{expected_sk}".encode(), date_stamp.encode("utf-8"))
    k_region = _hmac_sha256(k_date, region.encode("utf-8"))
    k_service = _hmac_sha256(k_region, service.encode("utf-8"))
    k_signing = _hmac_sha256(k_service, b"aws4_request")
    expected_sig = hmac.new(k_signing, string_to_sign.encode("utf-8"), hashlib.sha256).hexdigest()
    return hmac.compare_digest(signature, expected_sig)


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

        with STATE.lock:
            mode = STATE.mode
            cdn_host = STATE.cdn_host
            expected_token = STATE.expected_token
            expected_api_key = STATE.expected_api_key
            github_assets = dict(STATE.github_assets)

        if mode == "github_private":
            self._handle_github_private(path, cdn_host, expected_token, github_assets)
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

        if mode == "s3_sigv4":
            if not verify_aws_sigv4(self):
                with STATE.lock:
                    STATE.sigv4_rejected_requests += 1
                self.send_error(HTTPStatus.FORBIDDEN, "Invalid AWS SigV4 signature")
                return
            with STATE.lock:
                STATE.sigv4_verified_requests += 1
            serve_file_with_mode(self, file_path, "s3_single_only_200_on_multi")
            return

        if mode == "retry_503_and_api_key":
            if (self.headers.get("X-API-Key") or "") != expected_api_key:
                self.send_error(HTTPStatus.UNAUTHORIZED, "Missing or invalid X-API-Key")
                return
            with STATE.lock:
                if STATE.fail_503_remaining > 0:
                    STATE.fail_503_remaining -= 1
                    STATE.origin_503_hits += 1
                    self.send_error(HTTPStatus.SERVICE_UNAVAILABLE, "Simulated transient 503")
                    return
            is_metadata = rel.endswith((".json", ".yml", ".yaml", ".blockmap"))
            if is_metadata:
                serve_file_with_mode(self, file_path, "direct")
                return
            with STATE.lock:
                STATE.redirect_count += 1
            location = f"http://{cdn_host}:8081/cdn/signed-v2/{rel}"
            self.send_response(HTTPStatus.FOUND)
            self.send_header("Location", location)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return

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

    def _handle_github_private(
        self,
        path: str,
        cdn_host: str,
        expected_token: str,
        github_assets: dict[int, str],
    ) -> None:
        with STATE.lock:
            STATE.origin_requests += 1
        auth = self.headers.get("Authorization") or ""
        if auth != f"Bearer {expected_token}":
            self.send_error(HTTPStatus.UNAUTHORIZED, "Missing GitHub bearer token")
            return
        with STATE.lock:
            STATE.origin_auth_hits += 1

        if path.endswith("/releases/latest"):
            parts = [p for p in path.split("/") if p]
            # /repos/{owner}/{repo}/releases/latest
            owner, repo = parts[1], parts[2]
            assets = [
                {
                    "id": asset_id,
                    "name": filename,
                    "url": f"http://{cdn_host}:8080/repos/{owner}/{repo}/releases/assets/{asset_id}",
                }
                for asset_id, filename in sorted(github_assets.items())
            ]
            with STATE.lock:
                tag_name = STATE.github_tag
            self._send_json(
                HTTPStatus.OK,
                {"tag_name": tag_name, "draft": False, "prerelease": False, "assets": assets},
            )
            return

        if "/releases/assets/" in path:
            if (self.headers.get("Accept") or "") != "application/octet-stream":
                self.send_error(
                    HTTPStatus.BAD_REQUEST,
                    "GitHub asset API requires Accept: application/octet-stream",
                )
                return
            asset_id = int(path.rsplit("/", 1)[-1])
            filename = github_assets.get(asset_id)
            if not filename:
                self.send_error(HTTPStatus.NOT_FOUND, "Unknown GitHub asset ID")
                return
            file_path = (RELEASES_ROOT / filename).resolve()
            if filename.endswith((".json", ".yml", ".yaml")):
                serve_file_with_mode(self, file_path, "direct")
                return
            with STATE.lock:
                STATE.redirect_count += 1
            location = f"http://{cdn_host}:8081/cdn/signed-v2/{filename}"
            self.send_response(HTTPStatus.FOUND)
            self.send_header("Location", location)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return

        self.send_error(HTTPStatus.NOT_FOUND)

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

        if (
            self.headers.get("Authorization")
            or self.headers.get("Cookie")
            or self.headers.get("X-API-Key")
            or self.headers.get("X-GitHub-Api-Version")
            or self.headers.get("x-amz-security-token")
            or (self.headers.get("Accept") == "application/octet-stream")
        ):
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
