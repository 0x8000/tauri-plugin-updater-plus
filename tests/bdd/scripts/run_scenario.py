#!/usr/bin/env python3
"""Run Behave BDD scenarios with isolated testcontainers teardown."""

from __future__ import annotations

import os
import signal
import subprocess
import sys
from pathlib import Path

BDD_ROOT = Path(__file__).resolve().parents[1]
SRC = BDD_ROOT / "src"
if str(SRC) not in sys.path:
    sys.path.insert(0, str(SRC))

_CHILDREN: list[subprocess.Popen] = []


def _cleanup() -> None:
    try:
        from updater_plus_bdd.stack import cleanup_registered_stacks

        cleanup_registered_stacks()
    except Exception:
        pass


def _handle_signal(signum, _frame) -> None:
    for child in list(_CHILDREN):
        if child.poll() is None:
            child.terminate()
    for child in list(_CHILDREN):
        try:
            child.wait(timeout=15)
        except Exception:
            child.kill()
    _cleanup()
    raise SystemExit(128 + signum)


def main() -> int:
    signal.signal(signal.SIGTERM, _handle_signal)
    signal.signal(signal.SIGINT, _handle_signal)
    behave_bin = Path(sys.executable).parent / "behave"
    command = [str(behave_bin)] if behave_bin.exists() else [sys.executable, "-m", "behave"]
    if len(sys.argv) == 1:
        command.extend(["--tags=-skip", "--no-capture"])
    else:
        command.extend(sys.argv[1:])
    env = dict(os.environ)
    env["PYTHONPATH"] = str(SRC) + ((":" + env["PYTHONPATH"]) if env.get("PYTHONPATH") else "")
    child = subprocess.Popen(command, cwd=str(BDD_ROOT), env=env)
    _CHILDREN.append(child)
    try:
        return child.wait()
    finally:
        if child in _CHILDREN:
            _CHILDREN.remove(child)
        _cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
