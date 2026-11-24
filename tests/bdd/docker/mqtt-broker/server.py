#!/usr/bin/env python3
"""Minimal real TCP MQTT v3.1.1 broker + release publisher for BDD scenarios.

Listens on:
- TCP :1883 — MQTT v3.1.1 wire protocol (CONNECT/CONNACK, SUBSCRIBE/SUBACK, PUBLISH QoS 0/1, PUBACK, PINGREQ/PINGRESP, DISCONNECT)
- HTTP :8082 — /healthz, /admin/config, /admin/telemetry
"""

from __future__ import annotations

import asyncio
import base64
import json
import struct
import threading
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

RELEASES_ROOT = Path("/srv/releases")


class BrokerState:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.mode = "normal"  # normal | unavailable | out_of_order_and_duplicates | drop_last_chunk
        self.release: dict | None = None
        self.mqtt_connects = 0
        self.pull_manifest_reqs = 0
        self.pull_blockmap_reqs = 0
        self.pull_range_reqs = 0
        self.pull_full_reqs = 0
        self.push_chunks_sent = 0
        self.push_duplicates_sent = 0
        self.bytes_sent = 0

    def configure(self, payload: dict) -> None:
        with self.lock:
            if "mode" in payload:
                self.mode = str(payload["mode"])
            if "release" in payload:
                self.release = dict(payload["release"])

    def snapshot(self) -> dict:
        with self.lock:
            return {
                "mode": self.mode,
                "mqtt_connects": self.mqtt_connects,
                "pull_manifest_reqs": self.pull_manifest_reqs,
                "pull_blockmap_reqs": self.pull_blockmap_reqs,
                "pull_range_reqs": self.pull_range_reqs,
                "pull_full_reqs": self.pull_full_reqs,
                "push_chunks_sent": self.push_chunks_sent,
                "push_duplicates_sent": self.push_duplicates_sent,
                "bytes_sent": self.bytes_sent,
            }


STATE = BrokerState()
MQTT_READY = threading.Event()


def encode_remaining_length(length: int) -> bytes:
    out = bytearray()
    while True:
        byte = length % 128
        length //= 128
        if length > 0:
            byte |= 0x80
        out.append(byte)
        if length == 0:
            break
    return bytes(out)


async def read_remaining_length(reader: asyncio.StreamReader) -> int:
    multiplier = 1
    value = 0
    for _ in range(4):
        raw = await reader.readexactly(1)
        encoded = raw[0]
        value += (encoded & 0x7F) * multiplier
        if (encoded & 0x80) == 0:
            return value
        multiplier *= 128
    raise ValueError("malformed MQTT remaining length")


def build_publish_packet(topic: str, payload: bytes, qos: int = 1, packet_id: int = 1) -> bytes:
    topic_bytes = topic.encode("utf-8")
    body = bytearray()
    body.extend(struct.pack("!H", len(topic_bytes)))
    body.extend(topic_bytes)
    flags = 0
    if qos == 1:
        flags = 0x02
        body.extend(struct.pack("!H", packet_id & 0xFFFF))
    body.extend(payload)
    header = bytes([0x30 | flags]) + encode_remaining_length(len(body))
    return header + bytes(body)


def load_release_file(filename: str) -> bytes | None:
    target = (RELEASES_ROOT / filename).resolve()
    if not str(target).startswith(str(RELEASES_ROOT.resolve())) or not target.is_file():
        return None
    return target.read_bytes()


class MqttSession:
    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self.reader = reader
        self.writer = writer
        self.packet_seq = 1
        self.subscriptions: set[str] = set()

    def next_pid(self) -> int:
        pid = self.packet_seq
        self.packet_seq = (self.packet_seq % 65000) + 1
        return pid

    async def publish(self, topic: str, payload: bytes, qos: int = 1) -> None:
        pkt = build_publish_packet(topic, payload, qos=qos, packet_id=self.next_pid())
        self.writer.write(pkt)
        await self.writer.drain()

    async def handle_publish(self, topic: str, payload: bytes) -> None:
        with STATE.lock:
            mode = STATE.mode
            release = dict(STATE.release) if STATE.release else None

        if topic.startswith("updates/req/"):
            client_id = topic[len("updates/req/") :]
            req = json.loads(payload.decode("utf-8"))
            req_id = str(req["req_id"])
            resp_topic = f"updates/resp/{client_id}/{req_id}"
            op = req["op"]

            if mode == "unavailable" or release is None:
                if op == "manifest":
                    err_json = json.dumps(
                        {
                            "ok": False,
                            "kind": "unavailable",
                            "message": "MQTT broker release service unavailable",
                        }
                    ).encode("utf-8")
                    await self.publish(resp_topic, err_json, qos=1)
                else:
                    await self.publish(resp_topic, b"\x01broker unavailable", qos=1)
                return

            if op == "manifest":
                with STATE.lock:
                    STATE.pull_manifest_reqs += 1
                resp_json = json.dumps({"ok": True, "release": release}).encode("utf-8")
                await self.publish(resp_topic, resp_json, qos=1)
                return

            if op == "blockmap":
                with STATE.lock:
                    STATE.pull_blockmap_reqs += 1
                data = load_release_file(str(req["id"]))
                if data is None:
                    await self.publish(resp_topic, b"\x02blockmap not found", qos=1)
                else:
                    with STATE.lock:
                        STATE.bytes_sent += len(data)
                    await self.publish(resp_topic, b"\x00" + data, qos=1)
                return

            if op == "ranges":
                with STATE.lock:
                    STATE.pull_range_reqs += 1
                data = load_release_file(str(req["id"]))
                if data is None:
                    await self.publish(resp_topic, b"\x02artifact not found", qos=1)
                    return
                ranges = req["ranges"]
                buf = bytearray(b"\x00")
                buf.extend(struct.pack("!I", len(ranges)))
                sent = 0
                for start, end in ranges:
                    start_i = int(start)
                    end_i = int(end)
                    chunk = data[start_i : end_i + 1]
                    buf.extend(struct.pack("!QQI", start_i, end_i, len(chunk)))
                    buf.extend(chunk)
                    sent += len(chunk)
                with STATE.lock:
                    STATE.bytes_sent += sent
                await self.publish(resp_topic, bytes(buf), qos=1)
                return

            if op == "full":
                with STATE.lock:
                    STATE.pull_full_reqs += 1
                data = load_release_file(str(req["id"]))
                if data is None:
                    await self.publish(resp_topic, b"\x02artifact not found", qos=1)
                    return
                with STATE.lock:
                    STATE.bytes_sent += len(data)
                await self.publish(resp_topic, b"\x00" + data, qos=1)
                return

        elif topic.startswith("updates/push/hello/"):
            client_id = topic[len("updates/push/hello/") :]
            if release is None:
                return
            bm_bytes = load_release_file(str(release["blockmap"]))
            assert bm_bytes is not None
            announce = {
                "version": release["version"],
                "sha512": release["sha512"],
                "size": release["size"],
                "blockmap_b64": base64.b64encode(bm_bytes).decode("ascii"),
            }
            await self.publish(
                f"updates/push/{client_id}/announce",
                json.dumps(announce).encode("utf-8"),
                qos=1,
            )

        elif topic.startswith("updates/push/want/"):
            client_id = topic[len("updates/push/want/") :]
            if release is None:
                return
            req = json.loads(payload.decode("utf-8"))
            data = load_release_file(str(release["artifact"]))
            assert data is not None
            ranges = [(int(s), int(e)) for s, e in req["ranges"]]

            # Break each range into ~32 KiB chunks to exercise multi-message push streaming.
            chunks: list[tuple[int, bytes]] = []
            for start, end in ranges:
                cursor = start
                while cursor <= end:
                    chunk_end = min(end, cursor + 32 * 1024 - 1)
                    chunks.append((cursor, data[cursor : chunk_end + 1]))
                    cursor = chunk_end + 1

            if mode == "out_of_order_and_duplicates":
                chunks.reverse()
                # Inject both an overlapping slice and an exact QoS-1 duplicate of the first chunk.
                if chunks:
                    dup_offset, dup_data = chunks[0]
                    for offset, chunk in chunks:
                        await self.publish(
                            f"updates/push/{client_id}/chunk",
                            struct.pack("!Q", offset) + chunk,
                            qos=1,
                        )
                        with STATE.lock:
                            STATE.push_chunks_sent += 1
                            STATE.bytes_sent += len(chunk)
                    if len(dup_data) > 64:
                        half = len(dup_data) // 2
                        overlap_end = min(len(data), dup_offset + len(dup_data) + 32)
                        overlap_bytes = data[dup_offset + half : overlap_end]
                        await self.publish(
                            f"updates/push/{client_id}/chunk",
                            struct.pack("!Q", dup_offset + half) + overlap_bytes,
                            qos=1,
                        )
                        with STATE.lock:
                            STATE.push_chunks_sent += 1
                            STATE.bytes_sent += len(overlap_bytes)
                    await self.publish(
                        f"updates/push/{client_id}/chunk",
                        struct.pack("!Q", dup_offset) + dup_data,
                        qos=1,
                    )
                    with STATE.lock:
                        STATE.push_chunks_sent += 1
                        STATE.push_duplicates_sent += 1
                        STATE.bytes_sent += len(dup_data)
            elif mode == "drop_last_chunk":
                for offset, chunk in chunks[:-1]:
                    await self.publish(
                        f"updates/push/{client_id}/chunk",
                        struct.pack("!Q", offset) + chunk,
                        qos=1,
                    )
                    with STATE.lock:
                        STATE.push_chunks_sent += 1
                        STATE.bytes_sent += len(chunk)
            else:
                for offset, chunk in chunks:
                    await self.publish(
                        f"updates/push/{client_id}/chunk",
                        struct.pack("!Q", offset) + chunk,
                        qos=1,
                    )
                    with STATE.lock:
                        STATE.push_chunks_sent += 1
                        STATE.bytes_sent += len(chunk)

            await self.publish(f"updates/push/{client_id}/done", b"done", qos=1)


async def handle_mqtt_client(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    session = MqttSession(reader, writer)
    try:
        while True:
            fixed = await reader.read(1)
            if not fixed:
                break
            pkt_type = fixed[0] >> 4
            flags = fixed[0] & 0x0F
            rem_len = await read_remaining_length(reader)
            body = await reader.readexactly(rem_len) if rem_len > 0 else b""

            if pkt_type == 1:  # CONNECT
                with STATE.lock:
                    STATE.mqtt_connects += 1
                writer.write(b"\x20\x02\x00\x00")  # CONNACK accepted
                await writer.drain()
            elif pkt_type == 8:  # SUBSCRIBE
                pid = struct.unpack("!H", body[:2])[0]
                pos = 2
                return_codes = bytearray()
                while pos < len(body):
                    tlen = struct.unpack("!H", body[pos : pos + 2])[0]
                    pos += 2
                    tname = body[pos : pos + tlen].decode("utf-8")
                    pos += tlen
                    qos = body[pos]
                    pos += 1
                    session.subscriptions.add(tname)
                    return_codes.append(min(qos, 1))
                suback_body = struct.pack("!H", pid) + bytes(return_codes)
                writer.write(b"\x90" + encode_remaining_length(len(suback_body)) + suback_body)
                await writer.drain()
            elif pkt_type == 3:  # PUBLISH
                qos = (flags >> 1) & 0x03
                tlen = struct.unpack("!H", body[:2])[0]
                topic = body[2 : 2 + tlen].decode("utf-8")
                pos = 2 + tlen
                if qos == 1:
                    pid = struct.unpack("!H", body[pos : pos + 2])[0]
                    pos += 2
                    writer.write(struct.pack("!BBH", 0x40, 0x02, pid))  # PUBACK
                    await writer.drain()
                payload = body[pos:]
                await session.handle_publish(topic, payload)
            elif pkt_type == 4:  # PUBACK
                continue
            elif pkt_type == 12:  # PINGREQ
                writer.write(b"\xd0\x00")
                await writer.drain()
            elif pkt_type == 14:  # DISCONNECT
                break
    except (asyncio.IncompleteReadError, ConnectionResetError, BrokenPipeError):
        pass
    finally:
        writer.close()
        try:
            await writer.wait_closed()
        except Exception:
            pass


class AdminHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt: str, *args) -> None:
        return

    def do_GET(self) -> None:
        if self.path == "/healthz":
            if not MQTT_READY.is_set():
                self._send_json(HTTPStatus.SERVICE_UNAVAILABLE, {"ok": False})
                return
            self._send_json(HTTPStatus.OK, {"ok": True})
            return
        if self.path == "/admin/telemetry":
            self._send_json(HTTPStatus.OK, STATE.snapshot())
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


async def run_mqtt() -> None:
    server = await asyncio.start_server(handle_mqtt_client, "0.0.0.0", 1883)
    MQTT_READY.set()
    async with server:
        await server.serve_forever()


def main() -> None:
    admin = ThreadingHTTPServer(("0.0.0.0", 8082), AdminHandler)
    t_admin = threading.Thread(target=admin.serve_forever, daemon=True)
    t_admin.start()
    asyncio.run(run_mqtt())


if __name__ == "__main__":
    main()
