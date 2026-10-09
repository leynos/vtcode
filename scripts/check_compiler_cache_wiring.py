#!/usr/bin/env python3
"""Check that the Rust CI jobs get their compiler cache from setup-rust.

Four jobs compile Rust with a shared compiler cache. They used to start sccache
inline, with `RUSTC_WRAPPER` set for the whole job before any server existed,
so a slow or unreachable cache failed the build. They now run the shared
`setup-rust` action, which picks the backend for the runner, exports the
wrapper, starts the server with a 60 s timeout and, if the server still cannot
start, clears the wrapper so the build runs uncached with a `sccache-fallback`
warning. This check keeps the jobs that way:

* each listed job has a `setup-rust` step pinned to a full commit SHA;
* no listed job, and no workflow, sets `RUSTC_WRAPPER` or `SCCACHE_*` itself,
  because the action exports them and clears the wrapper on a failed start;
* no listed job also installs the toolchain, the Cargo cache or sccache by
  hand, because the action does all three;
* the `setup-rust` step passes no `toolchain`, so `rust-toolchain.toml` decides
  (an explicit `toolchain` sets a rustup override that beats the file).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import Any

import yaml

SETUP_RUST = "leynos/shared-actions/.github/actions/setup-rust"
PINNED = re.compile(re.escape(SETUP_RUST) + r"@[0-9a-f]{40}$")
HAND_ROLLED = (
    "mozilla-actions/sccache-action",
    "dtolnay/rust-toolchain",
    "Swatinem/rust-cache",
)
#: Workflow file name to the jobs in it that must use the shared action.
REQUIRED_JOBS = {
    "ci.yml": ("lint-clippy", "check-cross-platform", "test"),
    "tool-eval.yml": ("eval",),
}


def _env_violations(where: str, env: Any) -> list[str]:
    if not isinstance(env, dict):
        return []
    return [
        f"{where}: sets {name}, which setup-rust exports itself"
        for name in env
        if name == "RUSTC_WRAPPER" or str(name).startswith("SCCACHE_")
    ]


def violations(name: str, workflow: dict[str, Any]) -> list[str]:
    """Return the ways one workflow breaks the compiler-cache wiring."""
    found = _env_violations(f"{name}: workflow env", workflow.get("env"))
    jobs = workflow.get("jobs") or {}
    for job_id in REQUIRED_JOBS.get(name, ()):
        where = f"{name}:{job_id}"
        job = jobs.get(job_id)
        if job is None:
            found.append(f"{where}: job is missing")
            continue
        found += _env_violations(f"{where}: job env", job.get("env"))
        steps = job.get("steps") or []
        uses = [str(step.get("uses", "")) for step in steps]
        setup = [step for step in steps if str(step.get("uses", "")).startswith(SETUP_RUST)]
        if not setup:
            found.append(f"{where}: has no setup-rust step")
        for step in setup:
            if not PINNED.fullmatch(str(step["uses"])):
                found.append(f"{where}: setup-rust must be pinned to a 40-character SHA")
            if "toolchain" in (step.get("with") or {}):
                found.append(
                    f"{where}: setup-rust must not pass toolchain; "
                    "rust-toolchain.toml decides"
                )
        for action in HAND_ROLLED:
            if any(u.startswith(action) for u in uses):
                found.append(f"{where}: installs {action} by hand; setup-rust does it")
    return found


def main(root: Path) -> int:
    failed: list[str] = []
    for name in REQUIRED_JOBS:
        path = root / ".github" / "workflows" / name
        failed += violations(name, yaml.safe_load(path.read_text(encoding="utf-8")))
    for line in failed:
        print(f"[compiler-cache] {line}", file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main(Path(__file__).resolve().parents[1]))
