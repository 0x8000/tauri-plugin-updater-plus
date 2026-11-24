"""Thin Behave step glue for MQTT, GitHub, S3 SigV4, HTTP middleware, and edge-case scenarios."""

from __future__ import annotations

from _ctx import cdn_driver, mqtt_driver, nginx_driver, resolve_endpoint_token
from behave import given, then, when


@given('a running MQTT release broker "{broker_name:S}" in "{mode:S}" mode')
def step_start_mqtt_broker(context, broker_name: str, mode: str) -> None:
    driver = mqtt_driver(context, broker_name)
    driver.set_mode(mode)


@given('MQTT broker "{broker_name:S}" publishes artifact "{version:S}" with notes "{notes}"')
def step_mqtt_publish(context, broker_name: str, version: str, notes: str) -> None:
    driver = mqtt_driver(context, broker_name)
    artifact = context.artifacts.path_for(version)
    driver.publish_release(artifact_path=artifact, version=version, notes=notes)


@given('the updater uses MQTT pull provider connected to broker "{broker_name:S}"')
def step_use_mqtt_pull(context, broker_name: str) -> None:
    driver = mqtt_driver(context, broker_name)
    context.app.provider = {
        "kind": "mqtt_pull",
        "brokerAddr": driver.broker_addr,
    }


@given('the updater uses MQTT push DiffSession connected to broker "{broker_name:S}"')
def step_use_mqtt_push(context, broker_name: str) -> None:
    driver = mqtt_driver(context, broker_name)
    context.app.provider = {
        "kind": "mqtt_push",
        "brokerAddr": driver.broker_addr,
    }


@given(
    'the updater uses cross-protocol FailoverProvider from MQTT "{broker_name:S}" to "{endpoints_spec}"'
)
def step_use_failover_mqtt_http(context, broker_name: str, endpoints_spec: str) -> None:
    driver = mqtt_driver(context, broker_name)
    context.app.provider = {
        "kind": "failover_mqtt_to_http",
        "brokerAddr": driver.broker_addr,
    }
    context.app.endpoints = [
        resolve_endpoint_token(context, part) for part in endpoints_spec.split(",") if part.strip()
    ]


@given(
    'CDN gateway "{gateway_name:S}" publishes private GitHub release "{version:S}" with token "{token:S}"'
)
def step_cdn_publish_github(context, gateway_name: str, version: str, token: str) -> None:
    driver = cdn_driver(context, gateway_name)
    driver.set_mode("github_private", expected_token=token)
    artifact = context.artifacts.path_for(version)
    driver.publish_github_private_release(
        artifact_path=artifact,
        version=version,
        notes="Private GitHub release",
    )


@given('the updater uses GitHubProvider against "{gateway_name:S}" with token "{token:S}"')
def step_use_github_provider(context, gateway_name: str, token: str) -> None:
    driver = cdn_driver(context, gateway_name)
    context.app.provider = {
        "kind": "github",
        "owner": "acme",
        "repo": "myapp",
        "token": token,
        "apiBaseUrl": driver.api_base_url,
        "manifestName": "latest.json",
    }


@given(
    'CDN gateway "{gateway_name:S}" enforces S3 SigV4 in region "{region:S}" with STS token "{sts_token:S}"'
)
def step_cdn_configure_s3(context, gateway_name: str, region: str, sts_token: str) -> None:
    driver = cdn_driver(context, gateway_name)
    driver.set_mode(
        "s3_sigv4",
        sigv4_region=region,
        sigv4_access_key="AKIA_BDD_KEY",
        sigv4_secret_key="BDD_SECRET_ACCESS_KEY",
        sigv4_session_token=sts_token,
    )


@given(
    'the updater uses S3Provider in region "{region:S}" with STS token "{sts_token:S}" for "{endpoints_spec}"'
)
def step_use_s3_provider(context, region: str, sts_token: str, endpoints_spec: str) -> None:
    context.app.provider = {
        "kind": "s3",
        "region": region,
        "accessKeyId": "AKIA_BDD_KEY",
        "secretAccessKey": "BDD_SECRET_ACCESS_KEY",
        "sessionToken": sts_token,
    }
    context.app.endpoints = [
        resolve_endpoint_token(context, part) for part in endpoints_spec.split(",") if part.strip()
    ]


@given(
    'CDN gateway "{gateway_name:S}" requires X-API-Key "{api_key:S}" and fails first {fail_count:d} requests with 503'
)
def step_cdn_retry_and_api_key(context, gateway_name: str, api_key: str, fail_count: int) -> None:
    driver = cdn_driver(context, gateway_name)
    driver.set_mode(
        "retry_503_and_api_key",
        expected_api_key=api_key,
        fail_503_count=fail_count,
    )


@given(
    'the updater uses HTTP retry middleware with maxRetries {max_retries:d} and X-API-Key "{api_key:S}" for "{endpoints_spec}"'
)
def step_use_http_retry_middleware(
    context, max_retries: int, api_key: str, endpoints_spec: str
) -> None:
    context.app.provider = {
        "kind": "http_middleware_retry",
        "apiKey": api_key,
        "maxRetries": max_retries,
    }
    context.app.endpoints = [
        resolve_endpoint_token(context, part) for part in endpoints_spec.split(",") if part.strip()
    ]


@given('Nginx server "{server_name:S}" removes the blockmap for "{version:S}"')
def step_nginx_remove_blockmap(context, server_name: str, version: str) -> None:
    driver = nginx_driver(context, server_name)
    artifact = context.artifacts.path_for(version)
    driver.remove_blockmap(artifact.name)


@given(
    'Nginx server "{server_name:S}" publishes signed artifact "{version:S}" without a version comment'
)
def step_nginx_publish_signed_no_version(context, server_name: str, version: str) -> None:
    driver = nginx_driver(context, server_name)
    artifact = context.artifacts.path_for(version)
    sig = context.authority.sign_artifact(artifact, signed_version=None)
    driver.publish_release(
        artifact_path=artifact,
        version=version,
        manifest_name="latest.json",
        manifest_format="tauri",
        signature=sig,
    )


def _parse_bool(setting: str) -> bool:
    lowered = setting.strip().lower()
    if lowered == "true":
        return True
    if lowered == "false":
        return False
    raise ValueError(f"expected 'true' or 'false', got {setting!r}")


@given('the updater configures differential to "{setting:S}"')
def step_configure_differential(context, setting: str) -> None:
    context.app.differential = _parse_bool(setting)


@given('the updater configures allowDowngrades to "{setting:S}"')
@when('the updater configures allowDowngrades to "{setting:S}"')
def step_configure_allow_downgrades(context, setting: str) -> None:
    context.app.allow_downgrades = _parse_bool(setting)


@given("the updater configures maxRangesPerRequest to {limit:d}")
def step_configure_max_ranges(context, limit: int) -> None:
    context.app.max_ranges_per_request = limit


@given("the updater configures maxRangeGap to {gap:d}")
def step_configure_max_range_gap(context, gap: int) -> None:
    context.app.max_range_gap = gap


@then("the update check reports no update available")
def step_assert_no_update(context) -> None:
    out = context.app.last_outcome
    assert out is not None, "updater has not run"
    assert out.exit_code == 0, f"expected exit 0, got {out.exit_code}: {out}"
    assert not out.update_available, f"expected no update available, got {out}"


@then(
    'MQTT broker "{broker_name:S}" served at least {min_ranges:d} pull range request and {expected_full:d} full requests'
)
def step_assert_mqtt_pull_telemetry(
    context, broker_name: str, min_ranges: int, expected_full: int
) -> None:
    stats = mqtt_driver(context, broker_name).telemetry()
    assert stats["pull_range_reqs"] >= min_ranges, f"unexpected MQTT pull stats: {stats}"
    assert stats["pull_full_reqs"] == expected_full, f"expected {expected_full} full reqs: {stats}"


@then(
    'MQTT broker "{broker_name:S}" pushed at least {min_chunks:d} chunks including {min_dups:d} duplicate'
)
def step_assert_mqtt_push_telemetry(
    context, broker_name: str, min_chunks: int, min_dups: int
) -> None:
    stats = mqtt_driver(context, broker_name).telemetry()
    assert stats["push_chunks_sent"] >= min_chunks, f"unexpected MQTT push stats: {stats}"
    assert stats["push_duplicates_sent"] >= min_dups, f"expected duplicate QoS-1 delivery: {stats}"


@then(
    'CDN gateway "{gateway_name:S}" verified at least {min_verified:d} SigV4 requests and rejected {expected_rejected:d}'
)
def step_assert_sigv4_telemetry(
    context, gateway_name: str, min_verified: int, expected_rejected: int
) -> None:
    stats = cdn_driver(context, gateway_name).telemetry()
    assert stats["sigv4_verified_requests"] >= min_verified, f"unexpected SigV4 stats: {stats}"
    assert stats["sigv4_rejected_requests"] == expected_rejected, f"rejected SigV4 reqs: {stats}"
    assert stats["cdn_multi_200"] == 0, f"S3Provider must avoid multi-range probes: {stats}"
    assert stats["cdn_single_206"] >= 4, f"expected >=4 single-range 206s: {stats}"


@then(
    'CDN gateway "{gateway_name:S}" recorded {expected_503:d} transient 503 responses and {expected_leaks:d} leaked credentials'
)
def step_assert_retry_telemetry(
    context, gateway_name: str, expected_503: int, expected_leaks: int
) -> None:
    stats = cdn_driver(context, gateway_name).telemetry()
    assert stats["origin_503_hits"] == expected_503, f"unexpected 503 count: {stats}"
    assert stats["cdn_leaked_credentials"] == expected_leaks, f"leaked credentials: {stats}"


@then("the differential transfer completed in exact {expected_requests:d} HTTP requests")
def step_assert_exact_http_requests(context, expected_requests: int) -> None:
    out = context.app.last_outcome
    assert out is not None
    assert out.http_requests == expected_requests, (
        f"expected exact {expected_requests} HTTP requests, got {out.http_requests}: {out}"
    )


@then('MQTT broker "{broker_name:S}" recorded at least {min_connects:d} connection attempt')
def step_assert_mqtt_connects(context, broker_name: str, min_connects: int) -> None:
    stats = mqtt_driver(context, broker_name).telemetry()
    assert stats["mqtt_connects"] >= min_connects, (
        f"expected >={min_connects} MQTT connects: {stats}"
    )


@then("the push progress bytes equal the deduplicated missing range bytes")
def step_assert_push_deduplicated_progress(context) -> None:
    out = context.app.last_outcome
    assert out is not None
    assert out.downloaded_bytes > 0
    assert out.progress_bytes_total == out.downloaded_bytes, (
        f"progress bytes {out.progress_bytes_total} != deduplicated missing bytes {out.downloaded_bytes}"
    )


@then(
    'Nginx server "{server_name:S}" recorded {expected_hits:d} blockmap requests for "{version:S}"'
)
def step_assert_nginx_blockmap_hits(
    context, server_name: str, expected_hits: int, version: str
) -> None:
    driver = nginx_driver(context, server_name)
    artifact = context.artifacts.path_for(version)
    entries = driver.access_entries(f"{artifact.name}.blockmap")
    assert len(entries) == expected_hits, (
        f"expected {expected_hits} blockmap requests, got {len(entries)}: {entries}"
    )
