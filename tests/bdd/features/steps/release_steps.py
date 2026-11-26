"""Thin Behave step glue for Nginx and CDN release servers."""

from __future__ import annotations

from _ctx import cdn_driver, nginx_driver
from behave import given, then, when


@given('a running Nginx release server "{server_name:S}"')
def step_start_nginx(context, server_name: str) -> None:
    nginx_driver(context, server_name)


@given('a running CDN release gateway "{gateway_name:S}" in "{mode:S}" mode')
def step_start_cdn_gateway(context, gateway_name: str, mode: str) -> None:
    driver = cdn_driver(context, gateway_name)
    driver.set_mode(mode, token_max_hits=1)


@given(
    'Nginx server "{server_name:S}" publishes artifact "{version:S}" as "{manifest_name:S}" in "{fmt:S}" format'
)
@when(
    'Nginx server "{server_name:S}" publishes artifact "{version:S}" as "{manifest_name:S}" in "{fmt:S}" format'
)
def step_nginx_publish(
    context,
    server_name: str,
    version: str,
    manifest_name: str,
    fmt: str,
) -> None:
    driver = nginx_driver(context, server_name)
    artifact = context.artifacts.path_for(version)
    driver.publish_release(
        artifact_path=artifact,
        version=version,
        manifest_name=manifest_name,
        manifest_format=fmt,
    )


@given(
    'Nginx server "{server_name:S}" publishes signed artifact "{version:S}" with signed version "{signed_version:S}"'
)
def step_nginx_publish_signed(
    context,
    server_name: str,
    version: str,
    signed_version: str,
) -> None:
    driver = nginx_driver(context, server_name)
    artifact = context.artifacts.path_for(version)
    sig = context.authority.sign_artifact(artifact, signed_version=signed_version)
    driver.publish_release(
        artifact_path=artifact,
        version=version,
        manifest_name="latest.json",
        manifest_format="tauri",
        signature=sig,
    )


@given('Nginx server "{server_name:S}" tampers with published artifact "{version:S}"')
def step_nginx_tamper(context, server_name: str, version: str) -> None:
    driver = nginx_driver(context, server_name)
    artifact = context.artifacts.path_for(version)
    driver.tamper_published_artifact(artifact.name)


@given('Nginx server "{server_name:S}" replaces the blockmap for "{version:S}" with a gzip bomb')
def step_nginx_gzip_bomb(context, server_name: str, version: str) -> None:
    driver = nginx_driver(context, server_name)
    artifact = context.artifacts.path_for(version)
    driver.replace_blockmap_with_gzip_bomb(artifact.name)


@given(
    'Nginx server "{server_name:S}" replaces the blockmap for "{version:S}" with a stale blockmap from "{stale_version:S}"'
)
def step_nginx_stale_blockmap(context, server_name: str, version: str, stale_version: str) -> None:
    driver = nginx_driver(context, server_name)
    artifact = context.artifacts.path_for(version)
    stale_source = context.artifacts.path_for(stale_version)
    driver.replace_blockmap_with_stale_from(artifact.name, stale_source)


@given(
    'Nginx server "{server_name:S}" publishes a presigned manifest "{manifest_name:S}" for missing artifact "{version:S}" with query "{query:S}"'
)
def step_nginx_presigned_missing_manifest(
    context, server_name: str, manifest_name: str, version: str, query: str
) -> None:
    driver = nginx_driver(context, server_name)
    driver.publish_presigned_missing_manifest(manifest_name, version, query)


@given(
    'Nginx server "{server_name:S}" publishes an unverified manifest "{manifest_name:S}" for version "{version:S}"'
)
def step_nginx_unverified_manifest(
    context,
    server_name: str,
    manifest_name: str,
    version: str,
) -> None:
    driver = nginx_driver(context, server_name)
    artifact = context.artifacts.path_for(version)
    driver.publish_raw_json(
        manifest_name,
        {
            "version": version,
            "url": f"{driver.base_url}{artifact.name}",
        },
    )


@given(
    'CDN gateway "{gateway_name:S}" publishes artifact "{version:S}" as "{manifest_name:S}" in "{fmt:S}" format with notes "{notes}"'
)
def step_cdn_publish(
    context,
    gateway_name: str,
    version: str,
    manifest_name: str,
    fmt: str,
    notes: str,
) -> None:
    driver = cdn_driver(context, gateway_name)
    artifact = context.artifacts.path_for(version)
    driver.publish_release(
        artifact_path=artifact,
        version=version,
        manifest_name=manifest_name,
        manifest_format=fmt,
        notes=notes.replace("\\n", "\n"),
    )


@then(
    'Nginx server "{server_name:S}" served at least {min_count:d} multipart 206 response for "{version:S}"'
)
def step_assert_nginx_multipart(
    context,
    server_name: str,
    min_count: int,
    version: str,
) -> None:
    driver = nginx_driver(context, server_name)
    artifact = context.artifacts.path_for(version)
    actual = driver.multipart_206_count(artifact.name)
    assert actual >= min_count, (
        f"expected >={min_count} multipart/byteranges 206 responses, got {actual}: "
        f"{driver.access_entries(artifact.name)}"
    )


@then(
    'CDN gateway "{gateway_name:S}" recorded {expected_redirects:d} redirects {expected_leaks:d} leaked credentials and at least {min_single:d} single-range 206 responses'
)
def step_assert_cdn_telemetry(
    context,
    gateway_name: str,
    expected_redirects: int,
    expected_leaks: int,
    min_single: int,
) -> None:
    stats = cdn_driver(context, gateway_name).telemetry()
    assert stats["redirect_count"] == expected_redirects, f"unexpected redirects: {stats}"
    assert stats["cdn_leaked_credentials"] == expected_leaks, f"leaked credentials: {stats}"
    assert stats["cdn_single_206"] >= min_single, f"insufficient single-range 206s: {stats}"


@then(
    'Nginx server "{server_name:S}" served at least {min_ranges:d} byte ranges across 206 responses for "{version:S}"'
)
def step_assert_nginx_range_specs(
    context,
    server_name: str,
    min_ranges: int,
    version: str,
) -> None:
    driver = nginx_driver(context, server_name)
    artifact = context.artifacts.path_for(version)
    actual = driver.requested_range_spec_count(artifact.name)
    assert actual >= min_ranges, (
        f"expected >={min_ranges} requested byte ranges across 206 responses, got {actual}: "
        f"{driver.access_entries(artifact.name)}"
    )


@then(
    'Nginx server "{server_name:S}" recorded {expected_hits:d} artifact requests for "{version:S}"'
)
def step_assert_nginx_artifact_hits(
    context,
    server_name: str,
    expected_hits: int,
    version: str,
) -> None:
    driver = nginx_driver(context, server_name)
    artifact = context.artifacts.path_for(version)
    entries = driver.access_entries(artifact.name)
    assert len(entries) == expected_hits, (
        f"expected {expected_hits} artifact requests, got {len(entries)}: {entries}"
    )


@then('CDN gateway "{gateway_name:S}" recorded {expected_origin_reqs:d} total origin requests')
def step_assert_cdn_origin_requests(
    context,
    gateway_name: str,
    expected_origin_reqs: int,
) -> None:
    stats = cdn_driver(context, gateway_name).telemetry()
    assert stats["origin_requests"] == expected_origin_reqs, (
        f"expected {expected_origin_reqs} origin requests, got {stats['origin_requests']}: {stats}"
    )
