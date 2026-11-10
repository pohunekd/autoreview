#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import io
import os
import shutil
import stat
import subprocess
import tempfile
import tomllib
import unittest
from importlib.machinery import SourceFileLoader
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def load_module(name: str, path: Path):
    specification = importlib.util.spec_from_loader(name, SourceFileLoader(name, str(path)))
    if specification is None or specification.loader is None:
        raise RuntimeError(f"Unable to load {path}")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


wrapper = load_module("claude_adapter", ROOT / "claude-adapter")
installer = load_module("render_profile", ROOT / "render_profile.py")


class InputFrameTests(unittest.TestCase):
    def test_reads_multiline_utf8_prompt_without_losing_carriage_returns(self) -> None:
        prompt = "First line\nPříliš žluťoučký\rcontent"
        frame = (
            wrapper.BRACKETED_PASTE_START
            + prompt.encode("utf-8")
            + wrapper.BRACKETED_PASTE_END
            + wrapper.SUBMIT
        )

        self.assertEqual(wrapper.read_framed_prompt(io.BytesIO(frame), 4096), prompt)

    def test_rejects_missing_frame_marker(self) -> None:
        with self.assertRaisesRegex(wrapper.InputFrameError, "start marker"):
            wrapper.read_framed_prompt(io.BytesIO(b"plain prompt\r"), 4096)

    def test_rejects_oversized_prompt(self) -> None:
        frame = (
            wrapper.BRACKETED_PASTE_START
            + b"too-large"
            + wrapper.BRACKETED_PASTE_END
            + wrapper.SUBMIT
        )

        with self.assertRaisesRegex(wrapper.InputFrameError, "byte limit"):
            wrapper.read_framed_prompt(io.BytesIO(frame), 3)

    def test_result_envelope_preserves_process_streams_exactly(self) -> None:
        stdout = b'{"type":"result","result":"{\\"ok\\":true}"}\n'
        stderr = b"diagnostic\x1b[0m"

        framed = wrapper.result_envelope(7, stdout, stderr, "structured")
        envelope = json.loads(framed)

        self.assertTrue(framed.endswith(b"\n"))
        self.assertEqual(envelope["protocol"], wrapper.RESULT_PROTOCOL)
        self.assertEqual(envelope["mode"], "structured")
        self.assertEqual(envelope["exit_code"], 7)
        self.assertEqual(envelope["stdout_base64"], "eyJ0eXBlIjoicmVzdWx0IiwicmVzdWx0Ijoie1wib2tcIjp0cnVlfSJ9Cg==")
        self.assertEqual(envelope["stderr_base64"], "ZGlhZ25vc3RpYxtbMG0=")

    def test_parses_a_structured_request_and_canonicalizes_its_schema(self) -> None:
        request = json.dumps(
            {
                "protocol": wrapper.REQUEST_PROTOCOL,
                "prompt": "Review the pull request.",
                "json_schema": {
                    "required": ["ok"],
                    "properties": {"ok": {"type": "boolean"}},
                    "type": "object",
                },
            }
        )

        prompt, schema, mode = wrapper.parse_request(request, 4096)

        self.assertEqual(prompt, "Review the pull request.")
        self.assertEqual(mode, "structured")
        self.assertEqual(
            schema,
            '{"properties":{"ok":{"type":"boolean"}},"required":["ok"],"type":"object"}',
        )

    def test_preserves_plain_and_unrelated_json_prompts(self) -> None:
        for prompt in ("Review this code.", '{"protocol":"another-system/v1"}'):
            self.assertEqual(wrapper.parse_request(prompt, 4096), (prompt, None, "text"))

    def test_rejects_a_malformed_claimed_request_protocol(self) -> None:
        request = json.dumps({"protocol": wrapper.REQUEST_PROTOCOL, "prompt": "missing schema"})

        with self.assertRaisesRegex(wrapper.InputFrameError, "envelope is invalid"):
            wrapper.parse_request(request, 4096)

    def test_structured_mode_passes_the_canonical_schema_to_claude(self) -> None:
        arguments = wrapper.claude_arguments(
            "/usr/bin/claude",
            ["--model", "opus"],
            '{"type":"object"}',
        )

        self.assertEqual(arguments[:3], ["/usr/bin/claude", "--model", "opus"])
        self.assertEqual(arguments[-2:], ["--json-schema", '{"type":"object"}'])
        self.assertIn("--no-session-persistence", arguments)
        setting_sources_index = arguments.index("--setting-sources")
        self.assertEqual(arguments[setting_sources_index + 1], "user")


class VersionProbeTests(unittest.TestCase):
    """Pohunek probes the profile program with a bare --version."""

    def run_wrapper(
        self, root: Path, arguments: list[str], binary_variable: bool = True
    ) -> subprocess.CompletedProcess[str]:
        fake_binary = root / "fake-claude"
        fake_binary.write_text(
            '#!/bin/sh\n'
            'if [ "$#" -eq 1 ] && [ "$1" = --version ]; then\n'
            '  echo "2.1.296 (Claude Code)"\n'
            '  exit 0\n'
            'fi\n'
            'echo "unexpected arguments: $*" >&2\n'
            'exit 2\n',
            encoding="utf-8",
        )
        fake_binary.chmod(0o755)
        home = root / "home"
        home.mkdir(exist_ok=True)
        environment = {"PATH": os.environ["PATH"], "HOME": str(home), "PYTHONDONTWRITEBYTECODE": "1"}
        if binary_variable:
            environment[wrapper.CLAUDE_BINARY_VARIABLE] = str(fake_binary)
        return subprocess.run(
            [str(ROOT / "claude-adapter"), *arguments],
            env=environment,
            cwd=home,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_bare_version_prints_the_underlying_binary_version_line(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_wrapper(Path(directory), ["--version"])

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "2.1.296 (Claude Code)\n")
        self.assertEqual(result.stderr, "")

    def test_version_without_the_binary_variable_fails_fast(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_wrapper(Path(directory), ["--version"], binary_variable=False)

        self.assertEqual(result.returncode, wrapper.EXIT_USAGE)
        self.assertEqual(result.stdout, "")
        self.assertIn(wrapper.CLAUDE_BINARY_VARIABLE, result.stderr)

    def test_version_with_other_arguments_keeps_the_wrapper_protocol(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_wrapper(Path(directory), ["--version", "262144"])

        self.assertEqual(result.returncode, wrapper.EXIT_USAGE)
        self.assertEqual(result.stdout, "")


class ProfileInstallerTests(unittest.TestCase):
    def test_copies_otel_environment_and_disables_resume(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.toml"
            source.write_text(
                'base = "claude"\n'
                'args = ["--model", "opus", "--permission-mode", "plan"]\n'
                "[env]\n"
                'CLAUDE_CODE_ENABLE_TELEMETRY = "1"\n'
                'OTEL_EXPORTER_OTLP_ENDPOINT = "https://collector.invalid/otlp"\n',
                encoding="utf-8",
            )
            source.chmod(0o600)

            rendered = installer.render_profile(
                source,
                Path("/opt/autoreview/bin/claude-adapter"),
                Path("/usr/bin/claude"),
                262144,
                "sonnet",
                "medium",
                "no-detection",
            )
            profile = tomllib.loads(rendered)

            self.assertEqual(profile["base"], "claude")
            self.assertEqual(profile["program"], "/opt/autoreview/bin/claude-adapter")
            self.assertEqual(profile["args"][:2], ["/usr/bin/claude", "262144"])
            self.assertIn("--model", profile["args"])
            model_index = profile["args"].index("--model")
            self.assertEqual(profile["args"][model_index + 1], "sonnet")
            self.assertNotIn("opus", profile["args"])
            self.assertEqual(profile["env"]["CLAUDE_CODE_ENABLE_TELEMETRY"], "1")
            self.assertEqual(profile["env"][wrapper.CLAUDE_BINARY_VARIABLE], "/usr/bin/claude")
            self.assertEqual(
                profile["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"],
                "https://collector.invalid/otlp",
            )
            self.assertTrue(profile["input_rules"]["bracketed_paste"])
            self.assertFalse(profile["resume"]["resumable"])
            self.assertNotIn("fork", profile)

    def test_source_without_an_environment_table_renders_without_telemetry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.toml"
            source.write_text('base = "claude"\n', encoding="utf-8")
            source.chmod(0o600)

            rendered = installer.render_profile(
                source,
                Path("/opt/autoreview/bin/claude-adapter"),
                Path("/usr/bin/claude"),
                262144,
                "opus",
                "medium",
                "no-detection",
            )

        self.assertEqual(
            tomllib.loads(rendered)["env"],
            {wrapper.CLAUDE_BINARY_VARIABLE: "/usr/bin/claude"},
        )

    def test_atomic_write_is_idempotent_and_private(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "profile.toml"

            self.assertTrue(installer.atomic_write(target, 'base = "claude"\n'))
            self.assertFalse(installer.atomic_write(target, 'base = "claude"\n'))
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)
            self.assertEqual(target.stat().st_uid, os.geteuid())

    def test_rejects_source_arguments_that_override_wrapper_protocol(self) -> None:
        for controlled_argument in (
            "--output-format=stream-json",
            "--setting-sources=project",
            "--settings",
        ):
            with self.subTest(controlled_argument=controlled_argument):
                with tempfile.TemporaryDirectory() as directory:
                    source = Path(directory) / "source.toml"
                    source.write_text(
                        'base = "claude"\n'
                        f'args = ["{controlled_argument}"]\n'
                        "[env]\n"
                        'CLAUDE_CODE_ENABLE_TELEMETRY = "1"\n',
                        encoding="utf-8",
                    )

                    with self.assertRaisesRegex(ValueError, "wrapper-controlled"):
                        installer.render_profile(
                            source,
                            Path("/opt/autoreview/bin/claude-adapter"),
                            Path("/usr/bin/claude"),
                            262144,
                            "opus",
                            "medium",
                            "no-detection",
                        )

    def render_pinned_source(self, pin_lines: str) -> str:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.toml"
            source.write_text(
                'base = "claude"\n'
                f"{pin_lines}"
                'args = ["--permission-mode", "plan"]\n'
                "[env]\n"
                'CLAUDE_CODE_ENABLE_TELEMETRY = "1"\n',
                encoding="utf-8",
            )
            source.chmod(0o600)
            return installer.render_profile(
                source,
                Path("/opt/autoreview/bin/claude-adapter"),
                Path("/usr/bin/claude"),
                262144,
                "opus",
                "medium",
                "no-detection",
            )

    def test_carries_the_runtime_package_pin_after_the_base(self) -> None:
        digest = "sha256:" + "a" * 64
        rendered = self.render_pinned_source(
            f'package = "pohunek.runtime.claude"\ndigest = "{digest}"\n'
        )
        profile = tomllib.loads(rendered)

        self.assertEqual(profile["package"], "pohunek.runtime.claude")
        self.assertEqual(profile["digest"], digest)
        keys = [line.split(" =", 1)[0] for line in rendered.splitlines()[1:6]]
        self.assertEqual(keys, ["base", "package", "digest", "manifest", "program"])

    def test_references_the_detection_manifest_override(self) -> None:
        profile = tomllib.loads(self.render_pinned_source(""))

        self.assertEqual(profile["manifest"], "no-detection")

    def test_rejects_a_malformed_manifest_name(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.toml"
            source.write_text('base = "claude"\n', encoding="utf-8")
            source.chmod(0o600)

            with self.assertRaises(ValueError):
                installer.render_profile(
                    source,
                    Path("/opt/autoreview/bin/claude-adapter"),
                    Path("/usr/bin/claude"),
                    262144,
                    "opus",
                    "medium",
                    "../escape",
                )

    def test_unpinned_source_renders_no_pin(self) -> None:
        profile = tomllib.loads(self.render_pinned_source(""))

        self.assertNotIn("package", profile)
        self.assertNotIn("digest", profile)

    def test_rejects_a_partial_or_malformed_runtime_package_pin(self) -> None:
        digest = "sha256:" + "a" * 64
        for pin_lines, message in (
            ('package = "pohunek.runtime.claude"\n', "both package and digest"),
            (f'digest = "{digest}"\n', "both package and digest"),
            ('package = "pohunek.runtime.claude"\ndigest = "sha256:abc"\n', "digest is invalid"),
            (f'package = "Bad Package"\ndigest = "{digest}"\n', "package is invalid"),
        ):
            with self.subTest(pin_lines=pin_lines):
                with self.assertRaisesRegex(ValueError, message):
                    self.render_pinned_source(pin_lines)

    def test_rejects_a_malformed_source_model_option(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.toml"
            source.write_text(
                'base = "claude"\n'
                'args = ["--permission-mode", "plan", "--model"]\n'
                "[env]\n"
                'CLAUDE_CODE_ENABLE_TELEMETRY = "1"\n',
                encoding="utf-8",
            )

            with self.assertRaisesRegex(ValueError, "invalid model option"):
                installer.render_profile(
                    source,
                    Path("/opt/autoreview/bin/claude-adapter"),
                    Path("/usr/bin/claude"),
                    262144,
                    "sonnet",
                    "medium",
                    "no-detection",
                )


    def render_with_source_args(self, source_args: str, effort: str = "medium") -> dict:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.toml"
            source.write_text(
                'base = "claude"\n'
                f"args = {source_args}\n"
                "[env]\n"
                'CLAUDE_CODE_ENABLE_TELEMETRY = "1"\n',
                encoding="utf-8",
            )
            source.chmod(0o600)
            return tomllib.loads(
                installer.render_profile(
                    source,
                    Path("/opt/autoreview/bin/claude-adapter"),
                    Path("/usr/bin/claude"),
                    262144,
                    "opus",
                    effort,
                    "no-detection",
                )
            )

    def test_replaces_source_effort_with_the_configured_level(self) -> None:
        for source_args in (
            '["--effort", "high", "--permission-mode", "plan"]',
            '["--effort=xhigh", "--permission-mode", "plan"]',
            '["--permission-mode", "plan"]',
        ):
            with self.subTest(source_args=source_args):
                arguments = self.render_with_source_args(source_args)["args"]

                self.assertEqual(arguments.count("--effort"), 1)
                self.assertFalse(any(argument.startswith("--effort=") for argument in arguments))
                self.assertEqual(arguments[-2:], ["--effort", "medium"])
                self.assertIn("--permission-mode", arguments)

    def test_rejects_an_unknown_effort_level(self) -> None:
        for effort in ("", "auto", "ultracode", "Medium", "medium --model haiku"):
            with self.subTest(effort=effort):
                with self.assertRaisesRegex(ValueError, "effort level is invalid"):
                    self.render_with_source_args('["--permission-mode", "plan"]', effort)

    def test_rejects_a_malformed_source_effort_option(self) -> None:
        for source_args in ('["--permission-mode", "plan", "--effort"]', '["--effort", "--verbose"]', '["--effort="]'):
            with self.subTest(source_args=source_args):
                with self.assertRaisesRegex(ValueError, "invalid effort option"):
                    self.render_with_source_args(source_args)


if __name__ == "__main__":
    unittest.main()
