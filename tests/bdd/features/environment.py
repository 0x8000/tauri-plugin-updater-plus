"""Behave environment: one isolated ScenarioStack per scenario."""

from __future__ import annotations

import atexit
import signal
import sys
from pathlib import Path

BDD_ROOT = Path(__file__).resolve().parents[1]
SRC = BDD_ROOT / "src"
STEPS = BDD_ROOT / "features" / "steps"
for entry in (SRC, STEPS):
    if str(entry) not in sys.path:
        sys.path.insert(0, str(entry))

from updater_plus_bdd.drivers.artifacts import ArtifactFactory  # noqa: E402
from updater_plus_bdd.drivers.blockmap_cli import BlockmapCliDriver  # noqa: E402
from updater_plus_bdd.drivers.updater_client import (  # noqa: E402
    MinisignAuthority,
    UpdaterAppDriver,
)
from updater_plus_bdd.stack import (  # noqa: E402
    ScenarioStack,
    cleanup_registered_stacks,
    register_stack,
)


def _force_cleanup(*_args) -> None:
    cleanup_registered_stacks()


def before_all(context) -> None:
    atexit.register(_force_cleanup)
    for sig in (signal.SIGTERM, signal.SIGINT):
        previous = signal.getsignal(sig)

        def _handler(signum, frame, _prev=previous) -> None:
            cleanup_registered_stacks()
            if callable(_prev):
                _prev(signum, frame)
            raise SystemExit(128 + signum)

        signal.signal(sig, _handler)
    context.cli = BlockmapCliDriver()
    context.authority = MinisignAuthority()


def after_all(_context) -> None:
    cleanup_registered_stacks()


def before_scenario(context, scenario) -> None:
    context.scenario = scenario
    context.stack = ScenarioStack(scenario.name)
    register_stack(context.stack)
    context.artifacts = ArtifactFactory(context.stack.temp_dir / "artifacts")
    context.app = UpdaterAppDriver(context.stack.temp_dir / "app")
    context.nginx = {}
    context.cdn = {}
    context.last_gen_report = None
    context.last_manifest_path = None
    context.last_diff_report = None
    context.last_packed_zip = None


def after_scenario(context, _scenario) -> None:
    if getattr(context, "stack", None) is not None:
        context.stack.stop()
