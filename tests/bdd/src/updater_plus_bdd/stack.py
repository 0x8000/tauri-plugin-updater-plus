"""Per-scenario isolated stack built on testcontainers-python."""

from __future__ import annotations

import re
import shutil
import subprocess
import tempfile
from pathlib import Path

from testcontainers.core.network import Network

from .config import BDD_LABEL_KEY, BDD_SCENARIO_LABEL_KEY, image_name
from .containers import CdnGatewayContainer, MqttBrokerContainer, NginxReleaseContainer
from .images import ImageBuilder

_ACTIVE_STACKS: list[ScenarioStack] = []


def bdd_labels(scenario_name: str) -> dict[str, str]:
    """Docker labels applied to every BDD-owned container and network."""
    slug = re.sub(r"[^a-zA-Z0-9]+", "-", scenario_name).strip("-").lower()[:40] or "scenario"
    return {
        BDD_LABEL_KEY: "1",
        BDD_SCENARIO_LABEL_KEY: slug,
    }


def register_stack(stack: ScenarioStack) -> None:
    if stack not in _ACTIVE_STACKS:
        _ACTIVE_STACKS.append(stack)


def unregister_stack(stack: ScenarioStack) -> None:
    if stack in _ACTIVE_STACKS:
        _ACTIVE_STACKS.remove(stack)


def cleanup_registered_stacks() -> None:
    """Best-effort teardown of every live scenario stack."""
    while _ACTIVE_STACKS:
        stack = _ACTIVE_STACKS.pop()
        try:
            stack.stop()
        except Exception:
            pass


class ScenarioStack:
    """Owns the isolated Docker network, containers, and scratch directories for one scenario."""

    def __init__(
        self,
        scenario_name: str,
        *,
        image_builder: ImageBuilder | None = None,
    ) -> None:
        slug = re.sub(r"[^a-zA-Z0-9]+", "-", scenario_name).strip("-").lower()[:40] or "scenario"
        self.scenario_name = scenario_name
        self.slug = slug
        self.labels = bdd_labels(scenario_name)
        self.image_builder = image_builder or ImageBuilder()
        self.temp_dir = Path(tempfile.mkdtemp(prefix=f"up-bdd-{self.slug}-"))
        self.temp_dir.chmod(0o755)
        self._network = Network(docker_network_kw={"labels": dict(self.labels)})
        self._network_started = False
        self._nginx_servers: dict[str, NginxReleaseContainer] = {}
        self._cdn_gateways: dict[str, CdnGatewayContainer] = {}
        self._mqtt_brokers: dict[str, MqttBrokerContainer] = {}

    def _ensure_network(self) -> Network:
        if not self._network_started:
            self._network.__enter__()
            self._network_started = True
        return self._network

    def ensure_nginx(self, name: str = "primary") -> NginxReleaseContainer:
        if name not in self._nginx_servers:
            self.image_builder.ensure("nginx-release")
            net = self._ensure_network()
            releases_dir = self.temp_dir / f"nginx-{name}-releases"
            logs_dir = self.temp_dir / f"nginx-{name}-logs"
            releases_dir.mkdir(parents=True, exist_ok=True)
            logs_dir.mkdir(parents=True, exist_ok=True)
            releases_dir.chmod(0o755)
            logs_dir.chmod(0o777)
            container = NginxReleaseContainer(
                network=net,
                releases_dir=releases_dir,
                logs_dir=logs_dir,
                labels=self.labels,
                alias=f"nginx-{name}",
            ).start()
            self._nginx_servers[name] = container
        return self._nginx_servers[name]

    def ensure_cdn_gateway(self, name: str = "origin") -> CdnGatewayContainer:
        if name not in self._cdn_gateways:
            self.image_builder.ensure("cdn-gateway")
            net = self._ensure_network()
            releases_dir = self.temp_dir / f"cdn-{name}-releases"
            releases_dir.mkdir(parents=True, exist_ok=True)
            releases_dir.chmod(0o755)
            container = CdnGatewayContainer(
                network=net,
                releases_dir=releases_dir,
                labels=self.labels,
                alias=f"cdn-{name}",
            ).start()
            self._cdn_gateways[name] = container
        return self._cdn_gateways[name]

    def ensure_mqtt_broker(self, name: str = "primary") -> MqttBrokerContainer:
        if name not in self._mqtt_brokers:
            self.image_builder.ensure("mqtt-broker")
            net = self._ensure_network()
            releases_dir = self.temp_dir / f"mqtt-{name}-releases"
            releases_dir.mkdir(parents=True, exist_ok=True)
            releases_dir.chmod(0o755)
            container = MqttBrokerContainer(
                network=net,
                releases_dir=releases_dir,
                labels=self.labels,
                alias=f"mqtt-{name}",
            ).start()
            self._mqtt_brokers[name] = container
        return self._mqtt_brokers[name]

    def stop(self) -> None:
        for broker in list(self._mqtt_brokers.values()):
            try:
                broker.stop()
            except Exception:
                pass
        self._mqtt_brokers.clear()

        for container in list(self._nginx_servers.values()):
            try:
                container.stop()
            except Exception:
                pass
        self._nginx_servers.clear()

        for gateway in list(self._cdn_gateways.values()):
            try:
                gateway.stop()
            except Exception:
                pass
        self._cdn_gateways.clear()

        if self._network_started:
            try:
                self._network.__exit__(None, None, None)
            except Exception:
                pass
            self._network_started = False

        if self.temp_dir.exists():
            try:
                shutil.rmtree(self.temp_dir)
            except PermissionError:
                subprocess.run(
                    [
                        "docker",
                        "run",
                        "--rm",
                        "--label",
                        f"{BDD_LABEL_KEY}=1",
                        "-v",
                        f"{self.temp_dir}:/cleanup",
                        image_name("nginx-release"),
                        "sh",
                        "-c",
                        "rm -rf /cleanup/* /cleanup/.[!.]*",
                    ],
                    check=False,
                )
                shutil.rmtree(self.temp_dir, ignore_errors=True)
        unregister_stack(self)
