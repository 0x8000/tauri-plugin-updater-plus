"""Container wrappers attached to an isolated per-scenario Docker network."""

from __future__ import annotations

import time
from pathlib import Path

import requests
from testcontainers.core.container import DockerContainer
from testcontainers.core.network import Network

from .config import CDN_ASSET_PORT, CDN_ORIGIN_PORT, NGINX_CONTAINER_PORT, image_name


def probe_http_ready(url: str, timeout_sec: float = 20.0) -> None:
    """Wait until a real HTTP endpoint answers 200 OK."""
    deadline = time.monotonic() + timeout_sec
    last_err: Exception | None = None
    while time.monotonic() < deadline:
        try:
            resp = requests.get(url, timeout=1.0)
            if resp.status_code == 200:
                return
        except requests.RequestException as exc:
            last_err = exc
        time.sleep(0.1)
    raise RuntimeError(f"HTTP readiness probe timed out for {url}: {last_err}")


def resolve_container_ip(container: DockerContainer, network: Network) -> str:
    """Read the container's IPv4 address on the scenario bridge network (no host port)."""
    wrapped = container.get_wrapped_container()
    wrapped.reload()
    networks = wrapped.attrs.get("NetworkSettings", {}).get("Networks", {})
    if network.name in networks and networks[network.name].get("IPAddress"):
        return str(networks[network.name]["IPAddress"])
    for details in networks.values():
        ip = details.get("IPAddress")
        if ip:
            return str(ip)
    raise RuntimeError(f"container {wrapped.name} has no IPv4 address on network {network.name}")


class NginxReleaseContainer:
    """Nginx static release server supporting RFC 9110 multipart/byteranges."""

    def __init__(
        self,
        *,
        network: Network,
        releases_dir: Path,
        logs_dir: Path,
        labels: dict[str, str],
        alias: str = "nginx-release",
    ) -> None:
        self._network = network
        self.releases_dir = releases_dir
        self.logs_dir = logs_dir
        self.releases_dir.mkdir(parents=True, exist_ok=True)
        self.logs_dir.mkdir(parents=True, exist_ok=True)
        self._container = (
            DockerContainer(image_name("nginx-release"))
            .with_network(network)
            .with_network_aliases(alias)
            .with_volume_mapping(str(self.releases_dir), "/srv/releases", mode="ro")
            .with_volume_mapping(str(self.logs_dir), "/var/log/bdd", mode="rw")
            .with_kwargs(labels=dict(labels))
        )
        self.ip: str = ""

    def start(self) -> NginxReleaseContainer:
        self._container.start()
        try:
            self.ip = resolve_container_ip(self._container, self._network)
            probe_http_ready(f"http://{self.ip}:{NGINX_CONTAINER_PORT}/healthz")
        except BaseException:
            self._container.stop()
            raise
        return self

    @property
    def base_url(self) -> str:
        return f"http://{self.ip}/"

    def stop(self) -> None:
        self._container.stop()


class CdnGatewayContainer:
    """Dual-port Origin (:8080) + Cross-Origin CDN (:8081) release server."""

    def __init__(
        self,
        *,
        network: Network,
        releases_dir: Path,
        labels: dict[str, str],
        alias: str = "cdn-gateway",
    ) -> None:
        self._network = network
        self.releases_dir = releases_dir
        self.releases_dir.mkdir(parents=True, exist_ok=True)
        self._container = (
            DockerContainer(image_name("cdn-gateway"))
            .with_network(network)
            .with_network_aliases(alias)
            .with_volume_mapping(str(self.releases_dir), "/srv/releases", mode="ro")
            .with_kwargs(labels=dict(labels))
        )
        self.ip: str = ""

    def start(self) -> CdnGatewayContainer:
        self._container.start()
        try:
            self.ip = resolve_container_ip(self._container, self._network)
            probe_http_ready(f"http://{self.ip}:{CDN_ORIGIN_PORT}/healthz")
            probe_http_ready(f"http://{self.ip}:{CDN_ASSET_PORT}/healthz")
            requests.post(
                f"http://{self.ip}:{CDN_ORIGIN_PORT}/admin/config",
                json={"cdn_host": self.ip},
                timeout=5.0,
            ).raise_for_status()
        except BaseException:
            self._container.stop()
            raise
        return self

    @property
    def origin_base_url(self) -> str:
        return f"http://{self.ip}:{CDN_ORIGIN_PORT}/releases/"

    @property
    def admin_base_url(self) -> str:
        return f"http://{self.ip}:{CDN_ORIGIN_PORT}/admin"

    def stop(self) -> None:
        self._container.stop()
