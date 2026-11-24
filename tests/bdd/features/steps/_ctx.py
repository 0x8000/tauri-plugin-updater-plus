"""Shared context driver accessors for thin Behave step definitions."""

from __future__ import annotations

from updater_plus_bdd.drivers.release_origin import (
    CdnOriginDriver,
    MqttBrokerDriver,
    NginxReleaseDriver,
)


def nginx_driver(context, name: str) -> NginxReleaseDriver:
    if name not in context.nginx:
        container = context.stack.ensure_nginx(name)
        context.nginx[name] = NginxReleaseDriver(container, context.cli)
    return context.nginx[name]


def cdn_driver(context, name: str) -> CdnOriginDriver:
    if name not in context.cdn:
        gateway = context.stack.ensure_cdn_gateway(name)
        context.cdn[name] = CdnOriginDriver(gateway, context.cli)
    return context.cdn[name]


def mqtt_driver(context, name: str) -> MqttBrokerDriver:
    if name not in context.mqtt:
        broker = context.stack.ensure_mqtt_broker(name)
        context.mqtt[name] = MqttBrokerDriver(broker, context.cli)
    return context.mqtt[name]


def resolve_endpoint_token(context, token: str) -> str:
    item = token.strip()
    if item.startswith("nginx:"):
        rest = item[len("nginx:") :]
        name, _, path = rest.partition("/")
        return nginx_driver(context, name).manifest_url(path)
    if item.startswith("cdn:"):
        rest = item[len("cdn:") :]
        name, _, path = rest.partition("/")
        return cdn_driver(context, name).manifest_url(path)
    return item
