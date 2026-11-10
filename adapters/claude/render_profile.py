#!/usr/bin/env -S python3 -I
"""Generate a noninteractive pohunek Claude profile from a base Claude profile."""

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
MANIFEST_NAME = re.compile(r"^[A-Za-z0-9_.-]+$")
# Levels accepted by `claude --effort`. An explicit flag outranks every
# settings file, so a review runs at this level whatever /effort last saved.
EFFORT_LEVELS = {"low", "medium", "high", "xhigh", "max"}
# The Claude runtime package waits for a rendered prompt box before it writes a
# session's initial input. The wrapper reads that input silently and never
# renders one, so the profile overrides detection with an empty rule list and
# gets bracketed-paste readiness instead.
MANIFEST_CONTENT = (
    "# An empty rule list intentionally disables detection: the wrapper never renders a\n"
    "# prompt box, so the initial input must not wait for one.\n"
    "rules = []\n"
)
# The wrapper reads the Claude Code path from this file next to it; it must
# match the suffix in the wrapper.
SIDECAR_SUFFIX = ".claude-binary"
# Options that widen what a run may do. A review run is read-only, so a base
# profile cannot carry them.
PERMISSION_WIDENING_PREFIXES = ("--dangerously-", "--allow-dangerously-", "--add-dir")
WRAPPER_CONTROLLED_ARGUMENTS = {
    "--continue",
    "--json-schema",
    "--no-session-persistence",
    "--output-format",
    "--print",
    "--resume",
    "--setting-sources",
    "--settings",
    "-c",
    "-p",
    "-r",
}


def secure_regular_file(path: Path) -> None:
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode):
        raise ValueError(f"{path} must be a regular file")
    if metadata.st_uid != os.geteuid():
        raise ValueError(f"{path} must be owned by the current user")
    if metadata.st_mode & 0o022:
        raise ValueError(f"{path} must not be group or world writable")


def sidecar_path(wrapper: Path) -> Path:
    """The file next to the resolved wrapper that holds the Claude Code path."""
    return Path(os.path.realpath(wrapper) + SIDECAR_SUFFIX)


def quoted(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def render_profile(
    source: Path,
    wrapper: Path,
    claude_binary: Path,
    maximum_input_bytes: int,
    model: str,
    effort: str,
    manifest_name: str,
) -> str:
    if MANIFEST_NAME.fullmatch(manifest_name) is None:
        raise ValueError("manifest name is invalid")
    secure_regular_file(source)
    data = tomllib.loads(source.read_text(encoding="utf-8"))
    if data.get("base") != "claude":
        raise ValueError("source profile must use the Claude base runtime")

    # Pohunek refuses a Claude profile without the runtime package pin that its
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
    if any(
        argument in WRAPPER_CONTROLLED_ARGUMENTS
        or any(argument.startswith(f"{option}=") for option in WRAPPER_CONTROLLED_ARGUMENTS)
        for argument in source_args
    ):
        raise ValueError("source profile args contain a wrapper-controlled option")
    for index, argument in enumerate(source_args):
        if argument.startswith(PERMISSION_WIDENING_PREFIXES):
            raise ValueError("source profile args contain a permission-widening option")
        if argument == "--permission-mode":
            mode = source_args[index + 1] if index + 1 < len(source_args) else ""
        elif argument.startswith("--permission-mode="):
            mode = argument.removeprefix("--permission-mode=")
        else:
            continue
        if "bypass" in mode.lower():
            raise ValueError("source profile args contain a permission-widening option")
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
    if not claude_binary.is_absolute():
        raise ValueError("Claude Code executable must be an absolute path")
    if not wrapper.is_absolute():
        raise ValueError("wrapper must be an absolute path")
    if maximum_input_bytes <= 0:
        raise ValueError("maximum input size must be positive")
    if MODEL_NAME.fullmatch(model) is None:
        raise ValueError("model name is invalid")
    if effort not in EFFORT_LEVELS:
        raise ValueError("effort level is invalid")

    filtered_args: list[str] = []
    index = 0
    while index < len(source_args):
        argument = source_args[index]
        # The generated profile sets its own model and effort level.
        option = next((name for name in ("--model", "--effort") if argument == name), None)
        if option is not None:
            if index + 1 >= len(source_args) or source_args[index + 1].startswith("-"):
                raise ValueError(f"source profile contains an invalid {option[2:]} option")
            index += 2
            continue
        option = next((name for name in ("--model", "--effort") if argument.startswith(f"{name}=")), None)
        if option is not None:
            if not argument.removeprefix(f"{option}="):
                raise ValueError(f"source profile contains an invalid {option[2:]} option")
            index += 1
            continue
        filtered_args.append(argument)
        index += 1

    arguments = [
        str(claude_binary),
        str(maximum_input_bytes),
        *filtered_args,
        "--model",
        model,
        "--effort",
        effort,
    ]
    lines = [
        "# Generated by autoreview from a base Claude profile. Do not edit manually.",
        'base = "claude"',
    ]
    if package is not None and digest is not None:
        lines.extend([f"package = {quoted(package)}", f"digest = {quoted(digest)}"])
    lines.extend(
        [
            f"manifest = {quoted(manifest_name)}",
            f"program = {quoted(str(wrapper))}",
            f"args = [{', '.join(quoted(value) for value in arguments)}]",
            "",
            "[input_rules]",
            "bracketed_paste = true",
            "submit_delay_ms = 0",
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
    parser.add_argument("--claude-binary", type=Path, required=True)
    parser.add_argument("--maximum-input-bytes", type=int, required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--effort", required=True)
    parser.add_argument("--manifest-name", required=True)
    return parser.parse_args()


def main() -> None:
    arguments = parse_args()
    content = render_profile(
        arguments.source,
        arguments.wrapper,
        arguments.claude_binary,
        arguments.maximum_input_bytes,
        arguments.model,
        arguments.effort,
        arguments.manifest_name,
    )
    manifest_target = arguments.target.parent / "manifests" / f"{arguments.manifest_name}.toml"
    sidecar_changed = atomic_write(
        sidecar_path(arguments.wrapper), f"{arguments.claude_binary}\n"
    )
    manifest_changed = atomic_write(manifest_target, MANIFEST_CONTENT)
    profile_changed = atomic_write(arguments.target, content)
    print("changed" if sidecar_changed or manifest_changed or profile_changed else "unchanged")


if __name__ == "__main__":
    main()
