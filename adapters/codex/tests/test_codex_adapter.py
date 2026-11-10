#!/usr/bin/env python3

from __future__ import annotations

import base64
import importlib.util
import json
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
READ_ONLY_SOURCE = 'base = "codex"\n'
HEAD = "a" * 40


def load_module(name: str, path: Path):
    specification = importlib.util.spec_from_loader(name, SourceFileLoader(name, str(path)))
    if specification is None or specification.loader is None:
        raise RuntimeError(f"Unable to load {path}")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


wrapper = load_module("codex_adapter", ROOT / "codex-adapter")
renderer = load_module("render_profile", ROOT / "render_profile.py")


def review_result(**overrides: object) -> dict[str, object]:
    result: dict[str, object] = {
        "findings": [],
        "summary": "No findings.",
        "tests": [],
        "status": "completed",
        "reviewed_head_sha": HEAD,
        "prior_findings": [],
    }
    result.update(overrides)
    return result


def finding(**overrides: object) -> dict[str, object]:
    value: dict[str, object] = {
        "severity": "high",
        "confidence": "high",
        "blocking": True,
        "path": "src/lib.rs",
        "line": 7,
        "title": "Title",
        "body": "Body",
    }
    value.update(overrides)
    return value


def prior(**overrides: object) -> dict[str, object]:
    value: dict[str, object] = {
        "key": "abc123",
        "status": "fixed",
        "note": "The check is in place.",
        "relies_on": "",
    }
    value.update(overrides)
    return value


class WrapperTests(unittest.TestCase):
    def test_codex_arguments_are_the_fixed_read_only_command(self) -> None:
        arguments = wrapper.codex_arguments(
            "/usr/bin/codex",
            "model-x",
            "high",
            Path("/tmp/schema.json"),
            Path("/tmp/result.json"),
        )

        self.assertEqual(
            arguments,
            [
                "/usr/bin/codex",
                "exec",
                "--model",
                "model-x",
                "--config",
                'model_reasoning_effort="high"',
                "--config",
                'approval_policy="never"',
                "--sandbox",
                "read-only",
                "--ignore-user-config",
                "--ignore-rules",
                "--ephemeral",
                "--color",
                "never",
                "--output-schema",
                "/tmp/schema.json",
                "--output-last-message",
                "/tmp/result.json",
                "-",
            ],
        )

    def test_codex_run_drops_session_identity_and_github_tokens(self) -> None:
        environment = {
            "PATH": "/usr/bin",
            "HOME": "/tmp/home",
            "POHUNEK_SESSION_ID": "s",
            "POHUNEK_DAEMON_ID": "d",
            "GH_TOKEN": "t",
            "GITHUB_TOKEN": "t",
            "GH_ENTERPRISE_TOKEN": "t",
            "GITHUB_ENTERPRISE_TOKEN": "t",
            "MATRIX_ACCESS_TOKEN": "t",
            "AUTOREVIEW_GH_TOKEN": "t",
            "POHUNEK_SESSION_ID_HINT": "kept",
        }

        self.assertEqual(
            wrapper.codex_environment(environment),
            {"PATH": "/usr/bin", "HOME": "/tmp/home", "POHUNEK_SESSION_ID_HINT": "kept"},
        )

    def write_result(self, directory: str, value: object) -> Path:
        result = Path(directory) / "result.json"
        result.write_text(json.dumps(value), encoding="utf-8")
        return result

    def test_result_is_compact_bounded_base64_json(self) -> None:
        value = review_result(
            findings=[finding()],
            prior_findings=[prior(status="answered", relies_on="reviewer-login")],
        )
        with tempfile.TemporaryDirectory() as directory:
            encoded = wrapper.encoded_result(self.write_result(directory, value), 4096, HEAD)

        self.assertEqual(json.loads(base64.b64decode(encoded, validate=True)), value)
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(wrapper.InputFrameError, "invalid size"):
                wrapper.encoded_result(self.write_result(directory, value), 10, HEAD)

    def test_result_rejects_interrupted_or_wrong_head(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            for status, reviewed_head in (("interrupted", HEAD), ("completed", "b" * 40)):
                result = self.write_result(
                    directory, review_result(status=status, reviewed_head_sha=reviewed_head)
                )
                with self.assertRaisesRegex(wrapper.InputFrameError, "did not complete"):
                    wrapper.encoded_result(result, 1024, HEAD)

    def test_result_requires_the_blocking_flag_of_every_finding(self) -> None:
        for bad in (
            [finding(blocking="yes")],
            [finding(blocking=None)],
            [finding(), {key: value for key, value in finding().items() if key != "blocking"}],
        ):
            with self.subTest(findings=bad):
                with tempfile.TemporaryDirectory() as directory:
                    result = self.write_result(directory, review_result(findings=bad))
                    with self.assertRaisesRegex(wrapper.InputFrameError, "blocking flag"):
                        wrapper.encoded_result(result, 4096, HEAD)

    def test_result_requires_a_findings_list(self) -> None:
        missing = review_result()
        del missing["findings"]
        for bad in (missing, review_result(findings=None), review_result(findings={})):
            with self.subTest(result=bad):
                with tempfile.TemporaryDirectory() as directory:
                    result = self.write_result(directory, bad)
                    with self.assertRaisesRegex(wrapper.InputFrameError, "findings list"):
                        wrapper.encoded_result(result, 4096, HEAD)

    def test_result_requires_well_formed_prior_findings(self) -> None:
        missing = review_result()
        del missing["prior_findings"]
        for bad in (
            missing,
            review_result(prior_findings=None),
            review_result(prior_findings=[prior(status="resolved")]),
            review_result(prior_findings=[prior(key="")]),
            review_result(prior_findings=[prior(note="")]),
            review_result(prior_findings=[prior(relies_on=None)]),
            review_result(prior_findings=[{"key": "k", "status": "fixed", "note": "n"}]),
            review_result(prior_findings=[{**prior(), "extra": 1}]),
        ):
            with self.subTest(result=bad):
                with tempfile.TemporaryDirectory() as directory:
                    result = self.write_result(directory, bad)
                    with self.assertRaisesRegex(wrapper.InputFrameError, "prior findings"):
                        wrapper.encoded_result(result, 4096, HEAD)

    def test_schema_requires_the_fields_that_the_claude_result_carries(self) -> None:
        schema = wrapper.RESULT_SCHEMA
        item = schema["properties"]["findings"]["items"]
        verdict = schema["properties"]["prior_findings"]["items"]

        self.assertIn("prior_findings", schema["required"])
        self.assertIn("blocking", item["required"])
        self.assertEqual(item["properties"]["blocking"], {"type": "boolean"})
        self.assertEqual(verdict["required"], ["key", "status", "note", "relies_on"])
        self.assertEqual(verdict["properties"]["status"]["enum"], ["fixed", "still_open", "answered"])
        self.assertFalse(verdict["additionalProperties"])

    def test_trusted_head_must_be_unambiguous(self) -> None:
        self.assertEqual(wrapper.trusted_head_sha("- Trusted head SHA: " + HEAD), HEAD)
        with self.assertRaisesRegex(wrapper.InputFrameError, "missing or ambiguous"):
            wrapper.trusted_head_sha(
                "- Trusted head SHA: " + HEAD + "\n- Trusted head SHA: " + HEAD
            )

    def test_checkout_head_must_match_trusted_pull_request_head(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            checkout = Path(directory)
            git_environment = {
                **os.environ,
                "HOME": directory,
                "GIT_CONFIG_GLOBAL": "/dev/null",
                "GIT_CONFIG_NOSYSTEM": "1",
            }
            subprocess.run(["git", "init", "--quiet", str(checkout)], check=True, env=git_environment)
            tracked_file = checkout / "tracked.txt"
            tracked_file.write_text("committed content\n", encoding="utf-8")
            subprocess.run(
                [
                    "git", "-C", str(checkout), "-c", "user.name=Test",
                    "-c", "user.email=test@example.invalid", "-c",
                    "commit.gpgsign=false", "add", "tracked.txt",
                ],
                check=True,
                env=git_environment,
            )
            subprocess.run(
                [
                    "git", "-C", str(checkout), "-c", "user.name=Test",
                    "-c", "user.email=test@example.invalid", "-c",
                    "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture",
                ],
                check=True,
                env=git_environment,
            )
            actual_head = subprocess.run(
                ["git", "-C", str(checkout), "rev-parse", "HEAD"],
                check=True,
                capture_output=True,
                text=True,
                env=git_environment,
            ).stdout.strip()
            prompt = "- Trusted head SHA: " + actual_head + "\n- Trusted base SHA: " + actual_head
            self.assertEqual(wrapper.verify_checkout_head(prompt, checkout), actual_head)
            tracked_file.write_text("uncommitted content\n", encoding="utf-8")
            with self.assertRaisesRegex(wrapper.InputFrameError, "checkout is not clean"):
                wrapper.verify_checkout_head(prompt, checkout)
            subprocess.run(
                ["git", "-C", str(checkout), "checkout", "--", "tracked.txt"],
                check=True,
                env=git_environment,
            )
            (checkout / "untracked.txt").write_text("untracked content\n", encoding="utf-8")
            with self.assertRaisesRegex(wrapper.InputFrameError, "checkout is not clean"):
                wrapper.verify_checkout_head(prompt, checkout)
            (checkout / "untracked.txt").unlink()
            with self.assertRaisesRegex(wrapper.InputFrameError, "does not match"):
                wrapper.verify_checkout_head(
                    "- Trusted head SHA: " + "a" * 40 + "\n- Trusted base SHA: " + actual_head,
                    checkout,
                )
            with self.assertRaisesRegex(wrapper.InputFrameError, "base is unavailable"):
                wrapper.verify_checkout_head(
                    "- Trusted head SHA: " + actual_head + "\n- Trusted base SHA: " + "b" * 40,
                    checkout,
                )


class VersionProbeTests(unittest.TestCase):
    """Pohunek probes the profile program with a bare --version, a private HOME and only PATH."""

    def install_wrapper(self, root: Path, sidecar: str | None, mode: int = 0o600) -> Path:
        fake_binary = root / "fake-codex"
        fake_binary.write_text(
            '#!/bin/sh\n'
            'if [ "$#" -eq 1 ] && [ "$1" = --version ]; then\n'
            '  echo "codex-cli 1.2.3"\n'
            '  exit 0\n'
            'fi\n'
            'echo "unexpected arguments: $*" >&2\n'
            'exit 2\n',
            encoding="utf-8",
        )
        fake_binary.chmod(0o755)
        installed = root / "bin"
        installed.mkdir(exist_ok=True)
        script = installed / "codex-adapter"
        shutil.copy(ROOT / "codex-adapter", script)
        script.chmod(0o755)
        if sidecar is not None:
            file = Path(str(script) + wrapper.SIDECAR_SUFFIX)
            file.write_text(sidecar.replace("{fake}", str(fake_binary)), encoding="utf-8")
            file.chmod(mode)
        return script

    def run_wrapper(self, root: Path, script: Path, arguments: list[str]) -> subprocess.CompletedProcess[str]:
        home = root / "home"
        home.mkdir(exist_ok=True)
        return subprocess.run(
            [str(script), *arguments],
            env={"PATH": "/usr/bin:/bin", "HOME": str(home)},
            cwd=home,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_bare_version_prints_the_underlying_binary_version_line(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = self.run_wrapper(root, self.install_wrapper(root, "{fake}\n"), ["--version"])

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "codex-cli 1.2.3\n")
        self.assertEqual(result.stderr, "")

    def test_missing_or_insecure_path_file_fails_fast(self) -> None:
        for sidecar, mode, message in (
            (None, 0o600, "is missing"),
            ("{fake}\n", 0o666, "writable"),
            ("relative/codex\n", 0o600, "absolute"),
            ("\n", 0o600, "one path"),
        ):
            with self.subTest(sidecar=sidecar, mode=mode):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    script = self.install_wrapper(root, sidecar, mode)
                    result = self.run_wrapper(root, script, ["--version"])

                self.assertEqual(result.returncode, wrapper.EXIT_USAGE)
                self.assertEqual(result.stdout, "")
                self.assertIn(message, result.stderr)

    def test_launch_argument_must_equal_the_path_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = self.install_wrapper(root, "{fake}\n")
            result = self.run_wrapper(
                root, script, ["/bin/true", "262144", "262144", "60", "model-x", "high"]
            )

        self.assertEqual(result.returncode, wrapper.EXIT_USAGE)
        self.assertIn("differs", result.stderr)

    def test_version_with_other_arguments_keeps_the_wrapper_protocol(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = self.run_wrapper(root, self.install_wrapper(root, "{fake}\n"), ["--version", "262144"])

        self.assertEqual(result.returncode, wrapper.EXIT_USAGE)
        self.assertEqual(result.stdout, "")

    def test_rendered_path_file_answers_the_probe(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = self.install_wrapper(root, None)
            source = root / "source.toml"
            source.write_text(READ_ONLY_SOURCE, encoding="utf-8")
            source.chmod(0o600)
            rendered = subprocess.run(
                [
                    str(ROOT / "render_profile.py"),
                    "--source", str(source),
                    "--target", str(root / "agents" / "profile.toml"),
                    "--wrapper", str(script),
                    "--codex-binary", str(root / "fake-codex"),
                    "--maximum-input-bytes", "262144",
                    "--maximum-result-bytes", "262144",
                    "--maximum-retention-seconds", "60",
                    "--model", "model-x",
                    "--reasoning-effort", "high",
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            result = self.run_wrapper(root, script, ["--version"])

        self.assertEqual(rendered.returncode, 0, rendered.stderr)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "codex-cli 1.2.3\n")


class ProfileRendererTests(unittest.TestCase):
    def render(self, source_text: str, **overrides: object) -> str:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.toml"
            source.write_text(source_text, encoding="utf-8")
            source.chmod(0o600)
            arguments: dict[str, object] = {
                "wrapper": Path("/opt/autoreview/bin/codex-adapter"),
                "codex_binary": Path("/bin/sh"),
                "maximum_input_bytes": 262144,
                "maximum_result_bytes": 262144,
                "maximum_retention_seconds": 3900,
                "model": "model-x",
                "reasoning_effort": "high",
            }
            arguments.update(overrides)
            return renderer.render_profile(source, **arguments)

    def render_with_args(self, source_args: list[str]) -> str:
        return self.render(f"base = \"codex\"\nargs = {json.dumps(source_args)}\n")

    def test_profile_pins_model_reasoning_and_disables_resume(self) -> None:
        profile = tomllib.loads(self.render(READ_ONLY_SOURCE))

        self.assertEqual(profile["base"], "codex")
        self.assertEqual(profile["program"], "/opt/autoreview/bin/codex-adapter")
        self.assertEqual(
            profile["args"],
            ["/bin/sh", "262144", "262144", "3900", "model-x", "high"],
        )
        self.assertTrue(profile["input_rules"]["bracketed_paste"])
        self.assertFalse(profile["resume"]["resumable"])
        self.assertEqual(profile["env"], {})

    def test_copies_the_environment_table_and_the_package_pin(self) -> None:
        digest = "sha256:" + "a" * 64
        rendered = self.render(
            'base = "codex"\n'
            f'package = "pohunek.runtime.codex"\ndigest = "{digest}"\n'
            '[env]\nOTEL_EXPORTER_OTLP_ENDPOINT = "http://127.0.0.1:4318"\n'
        )
        profile = tomllib.loads(rendered)

        self.assertEqual(profile["package"], "pohunek.runtime.codex")
        self.assertEqual(profile["digest"], digest)
        self.assertEqual(
            profile["env"], {"OTEL_EXPORTER_OTLP_ENDPOINT": "http://127.0.0.1:4318"}
        )
        self.assertLess(rendered.index("package ="), rendered.index("program ="))

    def test_rejects_a_partial_or_malformed_package_pin(self) -> None:
        digest = "sha256:" + "a" * 64
        for pin in (
            'package = "pohunek.runtime.codex"\n',
            f'digest = "{digest}"\n',
            f'package = "Bad Name"\ndigest = "{digest}"\n',
            'package = "pohunek.runtime.codex"\ndigest = "sha256:short"\n',
        ):
            with self.subTest(pin=pin):
                with self.assertRaisesRegex(ValueError, "package|digest"):
                    self.render(f'base = "codex"\n{pin}')

    def test_source_must_use_the_codex_base(self) -> None:
        with self.assertRaisesRegex(ValueError, "Codex base runtime"):
            self.render('base = "claude"\n')

    def test_accepts_only_the_read_only_sandbox_and_never_ask_approval(self) -> None:
        for source_args in (
            [],
            ["--sandbox", "read-only"],
            ["--sandbox=read-only"],
            ["-s", "read-only"],
            ["-s=read-only"],
            ["--config", 'approval_policy="never"'],
            ["--config=approval_policy=\"never\""],
            ["-c", "approval_policy=never"],
            ["-c=approval_policy=never"],
            ["--sandbox", "read-only", "--config", 'approval_policy="never"'],
        ):
            with self.subTest(source_args=source_args):
                profile = tomllib.loads(self.render_with_args(source_args))
                self.assertEqual(len(profile["args"]), 6)

    def test_rejects_every_argument_that_widens_access(self) -> None:
        for widening in (
            ["--sandbox", "workspace-write"],
            ["--sandbox=workspace-write"],
            ["--sandbox", "danger-full-access"],
            ["--sandbox=danger-full-access"],
            ["-s", "workspace-write"],
            ["-s=danger-full-access"],
            ["-sworkspace-write"],
            ["--sandbox"],
            ["--dangerously-bypass-approvals-and-sandbox"],
            ["--dangerously-bypass-hook-trust"],
            ["--approve-for-me"],
            ["--full-auto"],
            ["--add-dir", "/"],
            ["--add-dir=/"],
            ["--cd", "/"],
            ["-C", "/"],
            ["--worktree"],
            ["--profile", "wide"],
            ["-p", "wide"],
            ["--enable", "unified_exec"],
            ["--disable", "sandbox"],
            ["--oss"],
            ["--model", "model-y"],
            ["-m", "model-y"],
            ["--config", 'approval_policy="on-request"'],
            ["--config", 'approval_policy="untrusted"'],
            ["--config=approval_policy=\"on-failure\""],
            ["-c", 'sandbox_mode="danger-full-access"'],
            ["-c=sandbox_mode=\"workspace-write\""],
            ["--config", "sandbox_permissions=[\"disk-full-write-access\"]"],
            ["--config", "shell_environment_policy.inherit=all"],
            ["--config", 'mcp_servers.x.command="x"'],
            ["--config", 'model_reasoning_effort="low"'],
            ["--ignore-rules=false"],
            ["--output-schema", "schema.json"],
            ["--output-last-message", "result.json"],
        ):
            with self.subTest(widening=widening):
                with self.assertRaisesRegex(ValueError, "other than the read-only"):
                    self.render_with_args(widening)
                with self.assertRaisesRegex(ValueError, "other than the read-only"):
                    self.render_with_args(["--sandbox", "read-only", *widening])

    def test_rejects_a_repeated_sandbox_or_approval_setting(self) -> None:
        for source_args in (
            ["--sandbox", "read-only", "--sandbox", "read-only"],
            ["--sandbox", "read-only", "-s=read-only"],
            ["--sandbox", "read-only", "--sandbox", "workspace-write"],
            ["-c", "approval_policy=never", "--config", 'approval_policy="never"'],
            ["--config", 'approval_policy="never"', "--config", 'approval_policy="on-request"'],
        ):
            with self.subTest(source_args=source_args):
                with self.assertRaisesRegex(ValueError, "repeat|other than the read-only"):
                    self.render_with_args(source_args)

    def test_rejects_invalid_limits_model_and_paths(self) -> None:
        for overrides, message in (
            ({"reasoning_effort": "extreme"}, "reasoning effort"),
            ({"model": "-bad"}, "model name"),
            ({"maximum_input_bytes": 0}, "positive"),
            ({"maximum_result_bytes": 0}, "positive"),
            ({"maximum_retention_seconds": 0}, "positive"),
            ({"wrapper": Path("codex-adapter")}, "wrapper must be an absolute"),
            ({"codex_binary": Path("codex")}, "Codex executable must be an absolute"),
        ):
            with self.subTest(overrides=overrides):
                with self.assertRaisesRegex(ValueError, message):
                    self.render(READ_ONLY_SOURCE, **overrides)

    def test_refuses_a_codex_binary_that_is_not_an_executable_regular_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plain = root / "plain"
            plain.write_text("x", encoding="utf-8")
            plain.chmod(0o644)
            newline = root / "with\nnewline"
            newline.write_text("x", encoding="utf-8")
            newline.chmod(0o755)
            for binary, message in (
                (root / "missing", "unavailable"),
                (root, "executable regular file"),
                (plain, "executable regular file"),
                (newline, "newline"),
            ):
                with self.subTest(binary=binary.name):
                    with self.assertRaisesRegex(ValueError, message):
                        self.render(READ_ONLY_SOURCE, codex_binary=binary)

    def test_atomic_write_is_idempotent_and_private(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "profile.toml"

            self.assertTrue(renderer.atomic_write(target, 'base = "codex"\n'))
            self.assertFalse(renderer.atomic_write(target, 'base = "codex"\n'))
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)
            self.assertEqual(target.stat().st_uid, os.geteuid())


if __name__ == "__main__":
    unittest.main()
