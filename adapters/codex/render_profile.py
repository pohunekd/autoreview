#!/usr/bin/env -S python3 -I
"""Generate a pohunek Codex review profile from a base Codex profile."""

from __future__ import annotations

import argparse
import json
import os
import re
import stat
import tempfile
import tomllib
from pathlib import Path


ENVIRONMENT_KEY = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
MODEL_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
PACKAGE_NAME = re.compile(r"^[a-z0-9][a-z0-9._-]*$")
PACKAGE_DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
REASONING_EFFORTS = {"low", "medium", "high", "xhigh"}
# The wrapper reads the Codex path from this file next to it; it must match the
# suffix in the wrapper.
SIDECAR_SUFFIX = ".codex-binary"
# The wrapper passes `--sandbox read-only` and `approval_policy="never"` itself,
# so a base profile may repeat exactly these settings and no other argument.
# Each form is the exact token sequence that is accepted and then dropped.
READ_ONLY_SANDBOX_FORMS = (
    ("--sandbox", "read-only"),
    ("--sandbox=read-only",),
    ("-s", "read-only"),
    ("-s=read-only",),
)
NEVER_APPROVAL_FORMS = tuple(
    form
    for option in ("--config", "-c")
    for value in ('approval_policy="never"', "approval_policy=never")
    for form in ((option, value), (f"{option}={value}",))
)


def secure_regular_file(path: Path) -> None:
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode):
        raise ValueError(f"{path} must be a regular file")
    if metadata.st_uid != os.geteuid():
        raise ValueError(f"{path} must be owned by the current user")
    if metadata.st_mode & 0o022:
        raise ValueError(f"{path} must not be group or world writable")


def sidecar_path(wrapper: Path) -> Path:
    """The file next to the resolved wrapper that holds the Codex path."""
    return Path(os.path.realpath(wrapper) + SIDECAR_SUFFIX)


def quoted(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def check_source_arguments(source_args: list[str]) -> None:
    """Accepts only the read-only sandbox and the never-ask approval policy, once each.

    Every other argument is refused, so nothing that widens what the run may read,
    write or reach (`--sandbox workspace-write`, `--add-dir`, `--config
    sandbox_mode=...`, `--dangerously-bypass-approvals-and-sandbox`, ...) gets through
    in any spelling.
    """
    seen = {"sandbox": 0, "approval": 0}
    index = 0
    while index < len(source_args):
        for kind, forms in (
            ("sandbox", READ_ONLY_SANDBOX_FORMS),
            ("approval", NEVER_APPROVAL_FORMS),
        ):
            form = next(
                (
                    candidate
                    for candidate in forms
                    if tuple(source_args[index : index + len(candidate)]) == candidate
                ),
                None,
            )
            if form is not None:
                seen[kind] += 1
                index += len(form)
                break
        else:
            raise ValueError(
                "source profile args contain an argument other than the read-only "
                "sandbox and the never-ask approval policy"
            )
    if max(seen.values()) > 1:
        raise ValueError("source profile args repeat the sandbox or the approval policy")


def render_profile(
    source: Path,
    wrapper: Path,
    codex_binary: Path,
    maximum_input_bytes: int,
    maximum_result_bytes: int,
    maximum_retention_seconds: int,
    model: str,
    reasoning_effort: str,
) -> str:
    secure_regular_file(source)
    data = tomllib.loads(source.read_text(encoding="utf-8"))
    if data.get("base") != "codex":
        raise ValueError("source profile must use the Codex base runtime")

    # Pohunek refuses a Codex profile without the runtime package pin that its
    # migration writes into the source profile, so the pin is carried over.
    package = data.get("package")
    digest = data.get("digest")
    if (package is None) != (digest is None):
        raise ValueError("source profile must pin both package and digest, or neither")
    if package is not None and (
        not isinstance(package, str) or PACKAGE_NAME.fullmatch(package) is None
    ):
        raise ValueError("source profile package is invalid")
    if digest is not None and (
        not isinstance(digest, str) or PACKAGE_DIGEST.fullmatch(digest) is None
    ):
        raise ValueError("source profile digest is invalid")

    source_args = data.get("args", [])
    if not isinstance(source_args, list) or not all(isinstance(value, str) for value in source_args):
        raise ValueError("source profile args must be an array of strings")
    check_source_arguments(source_args)
    # The environment table is optional: telemetry settings, when present, are
    # carried over, and a profile without them runs without telemetry.
    environment = data.get("env", {})
    if not isinstance(environment, dict):
        raise ValueError("source profile environment must be a table")
    if not all(
        isinstance(key, str)
        and ENVIRONMENT_KEY.fullmatch(key)
        and isinstance(value, str)
        for key, value in environment.items()
    ):
        raise ValueError("source profile environment must contain valid string entries")
    if not wrapper.is_absolute():
        raise ValueError("wrapper must be an absolute path")
    if not codex_binary.is_absolute():
        raise ValueError("Codex executable must be an absolute path")
    if "\n" in str(codex_binary):
        raise ValueError("Codex executable path must not contain a newline")
    try:
        executable = codex_binary.stat()
    except OSError as error:
        raise ValueError("Codex executable is unavailable") from error
    if not stat.S_ISREG(executable.st_mode) or not os.access(codex_binary, os.X_OK):
        raise ValueError("Codex executable must be an executable regular file")
    if (
        maximum_input_bytes <= 0
        or maximum_result_bytes <= 0
        or maximum_retention_seconds <= 0
    ):
        raise ValueError("maximum sizes and retention time must be positive")
    if MODEL_NAME.fullmatch(model) is None:
        raise ValueError("model name is invalid")
    if reasoning_effort not in REASONING_EFFORTS:
        raise ValueError("reasoning effort is invalid")

    arguments = [
        str(codex_binary),
        str(maximum_input_bytes),
        str(maximum_result_bytes),
        str(maximum_retention_seconds),
        model,
        reasoning_effort,
    ]
    lines = [
        "# Generated by autoreview from a base Codex profile. Do not edit manually.",
        'base = "codex"',
    ]
    if package is not None and digest is not None:
        lines.extend([f"package = {quoted(package)}", f"digest = {quoted(digest)}"])
    lines.extend(
        [
            f"program = {quoted(str(wrapper))}",
            f"args = [{', '.join(quoted(value) for value in arguments)}]",
            "",
            "[input_rules]",
            "bracketed_paste = true",
            "submit_delay_ms = 150",
            "",
            "[resume]",
            "resumable = false",
            "",
            "[env]",
        ]
    )
    lines.extend(f"{key} = {quoted(environment[key])}" for key in sorted(environment))
    return "\n".join(lines) + "\n"


def atomic_write(path: Path, content: str) -> bool:
    if path.exists():
        secure_regular_file(path)
        if path.read_text(encoding="utf-8") == content:
            return False

    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)
    return True


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--target", type=Path, required=True)
    parser.add_argument("--wrapper", type=Path, required=True)
    parser.add_argument("--codex-binary", type=Path, required=True)
    parser.add_argument("--maximum-input-bytes", type=int, required=True)
    parser.add_argument("--maximum-result-bytes", type=int, required=True)
    parser.add_argument("--maximum-retention-seconds", type=int, required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--reasoning-effort", required=True)
    return parser.parse_args()


def main() -> None:
    arguments = parse_args()
    content = render_profile(
        arguments.source,
        arguments.wrapper,
        arguments.codex_binary,
        arguments.maximum_input_bytes,
        arguments.maximum_result_bytes,
        arguments.maximum_retention_seconds,
        arguments.model,
        arguments.reasoning_effort,
    )
    sidecar_changed = atomic_write(
        sidecar_path(arguments.wrapper), f"{arguments.codex_binary}\n"
    )
    profile_changed = atomic_write(arguments.target, content)
    print("changed" if sidecar_changed or profile_changed else "unchanged")


if __name__ == "__main__":
    main()
