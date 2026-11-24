# Updater-Plus BDD Suite

Self-contained end-to-end Behave BDD suite for `tauri-plugin-updater-plus`.
Every scenario boots its own **isolated Docker network** and `testcontainers`
service stack (`nginx-release` for RFC 9110 `multipart/byteranges` static
hosting, and `cdn-gateway` for GitHub/S3 `302` signed-token redirects and
single-range degradation). **No service ports are published to the host** — the
runner probes readiness over real `/healthz` HTTP endpoints and addresses
containers directly by their scenario bridge IPv4 addresses.

## Architecture

- `src/updater_plus_bdd/` — image builder, isolated `ScenarioStack`, and
  object-oriented product drivers (`BlockmapCliDriver`, `NginxReleaseDriver`,
  `CdnOriginDriver`, `MinisignAuthority`, `UpdaterAppDriver`, `ArtifactFactory`)
- `features/` — Gherkin feature specifications and thin Behave step glue
- `docker/` — service Dockerfiles (`nginx-release`, `cdn-gateway`) rebuilt
  automatically when build-input hashes change (`updater-plus.bdd.buildhash`)
- `scripts/` — image builder, scenario runner with `SIGINT`/`SIGTERM` teardown,
  scenario lister, and labeled Docker resource cleanup (`updater-plus.bdd=1`)
- `tests/` — fast unit tests covering harness hashing, labels, and fixtures

## Commands

```bash
# From the repository root:
just bdd venv          # create tests/bdd/.venv and install behave + ruff + testcontainers
just test-bdd          # build Rust binaries + Docker images and run all BDD scenarios
just test-one features/nginx_differential_update.feature
just bdd list-scenarios
just bdd lint          # ruff check + ruff format --check + harness unit tests
just bdd cleanup       # remove only updater-plus.bdd=1 containers and networks
```
