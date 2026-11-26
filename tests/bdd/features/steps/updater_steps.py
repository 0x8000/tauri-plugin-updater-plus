"""Thin Behave step glue for the updater client application surface."""

from __future__ import annotations

from _ctx import resolve_endpoint_token
from behave import given, then, when


@given('an installed AppImage at version "{version:S}" with an empty update cache')
def step_installed_empty_cache(context, version: str) -> None:
    artifact = context.artifacts.path_for(version)
    context.app.install_initial_appimage(artifact, version, mode=0o750)


@given('an installed AppImage at version "{version:S}" with seeded cache')
def step_installed_seeded_cache(context, version: str) -> None:
    artifact = context.artifacts.path_for(version)
    context.app.install_initial_appimage(artifact, version, mode=0o750)
    context.app.seed_cache(artifact, version)


@given('the updater trusts the minisign authority with requireSignedVersion "{required:S}"')
def step_trust_minisign(context, required: str) -> None:
    context.app.pubkey = context.authority.pubkey
    context.app.require_signed_version = required.lower() == "true"


@given("the updater configures fullDownloadThreshold to {threshold:f}")
def step_configure_threshold(context, threshold: float) -> None:
    context.app.full_download_threshold = threshold


@given('the updater configures useMultipleRangeRequest to "{setting:S}"')
def step_configure_multi_range(context, setting: str) -> None:
    lowered = setting.lower()
    if lowered == "auto":
        context.app.use_multiple_range_request = None
    else:
        context.app.use_multiple_range_request = lowered == "true"


@given("the updater configures maxConcurrentRanges to {concurrency:d}")
def step_configure_concurrency(context, concurrency: int) -> None:
    context.app.max_concurrent_ranges = concurrency


@given('the updater sends Authorization "{auth_val}" and Cookie "{cookie_val}"')
def step_configure_credentials(context, auth_val: str, cookie_val: str) -> None:
    context.app.headers = [
        ("Authorization", auth_val),
        ("Cookie", cookie_val),
    ]


@given('the updater endpoints are "{endpoints_spec}"')
def step_configure_endpoints(context, endpoints_spec: str) -> None:
    context.app.endpoints = [
        resolve_endpoint_token(context, part) for part in endpoints_spec.split(",") if part.strip()
    ]


@when("local cached package bytes at offset {offset:d} are corrupted on disk")
def step_corrupt_cache(context, offset: int) -> None:
    context.app.corrupt_cached_package(offset=offset)


@when('the application executes "{action:S}"')
def step_execute_updater(context, action: str) -> None:
    context.app.execute(action)


@then('the update succeeds in "{expected_mode:S}" mode to version "{expected_version:S}"')
def step_assert_update_succeeded(context, expected_mode: str, expected_version: str) -> None:
    out = context.app.last_outcome
    assert out is not None, "updater has not run"
    assert out.exit_code == 0, f"expected exit 0, got {out.exit_code}: {out}"
    assert out.update_available, f"expected update available: {out}"
    assert out.version == expected_version, f"expected {expected_version}, got {out.version}"
    assert out.mode == expected_mode, f"expected mode {expected_mode}, got {out.mode} ({out})"
    assert out.cached_version == expected_version, f"cache state not updated: {out}"


@then("the transfer saves more than {min_saved:f} percent of package bytes")
def step_assert_saved_percent(context, min_saved: float) -> None:
    out = context.app.last_outcome
    assert out is not None
    assert out.saved_percent > min_saved, f"expected >{min_saved}%, got {out.saved_percent}%"
    assert out.downloaded_bytes < out.total_bytes


@then("the transfer emits Started Progress and Finished events")
def step_assert_lifecycle_events(context) -> None:
    out = context.app.last_outcome
    assert out is not None
    assert out.events_count >= 3, f"expected >=3 events, got {out.events_count}"
    assert out.started_differential is (out.mode == "differential"), (
        f"Started event differential={out.started_differential} did not match mode={out.mode}"
    )
    assert out.progress_bytes_total == out.downloaded_bytes, (
        f"Progress byte sum {out.progress_bytes_total} != downloaded_bytes {out.downloaded_bytes}"
    )
    assert out.has_finished_event, "missing terminal Finished event"


@then('the installed AppImage matches artifact "{version:S}" with mode "{octal_mode:S}"')
def step_assert_installed_appimage(context, version: str, octal_mode: str) -> None:
    expected = context.artifacts.path_for(version).read_bytes()
    actual = context.app.installed_bytes()
    assert actual == expected, f"installed AppImage bytes did not match {version}"
    actual_mode = context.app.installed_mode()
    expected_mode = int(octal_mode, 8)
    assert actual_mode == expected_mode, f"expected {oct(expected_mode)}, got {oct(actual_mode)}"


@then('the fallback reason contains "{expected_fragment}"')
def step_assert_fallback_reason(context, expected_fragment: str) -> None:
    out = context.app.last_outcome
    assert out is not None
    assert out.fallback_reason is not None, f"expected fallback reason, got None: {out}"
    assert expected_fragment in out.fallback_reason, (
        f"expected '{expected_fragment}' in '{out.fallback_reason}'"
    )


@then('the update fails with error kind "{expected_kind:S}"')
def step_assert_update_failed(context, expected_kind: str) -> None:
    out = context.app.last_outcome
    assert out is not None
    assert out.exit_code != 0, f"expected non-zero exit code, got 0: {out}"
    assert out.error_kind == expected_kind, f"expected {expected_kind}, got {out.error_kind}: {out}"


@then('the error message contains "{expected_fragment}" and does not contain "{secret_fragment}"')
def step_assert_error_redaction(context, expected_fragment: str, secret_fragment: str) -> None:
    out = context.app.last_outcome
    assert out is not None
    assert out.message is not None, f"expected error message, got None: {out}"
    assert expected_fragment in out.message, f"expected {expected_fragment!r} in {out.message!r}"
    assert secret_fragment not in out.message, (
        f"secret {secret_fragment!r} leaked in {out.message!r}"
    )


@then('the cached base package still matches artifact "{version:S}"')
def step_assert_cached_preserved(context, version: str) -> None:
    expected = context.artifacts.path_for(version).read_bytes()
    assert context.app.cached_bytes() == expected


@then('the update notes equal "{expected_notes}"')
def step_assert_notes(context, expected_notes: str) -> None:
    out = context.app.last_outcome
    assert out is not None
    assert out.notes == expected_notes.replace("\\n", "\n"), f"unexpected notes: {out.notes!r}"
