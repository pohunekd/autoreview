#!/usr/bin/env python3

from __future__ import annotations

import base64
import contextlib
import importlib.util
import json
import io
import os
import pty
import select
import shutil
import stat
import subprocess
import tempfile
import termios
import time
import tomllib
import unittest
from importlib.machinery import SourceFileLoader
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
READ_ONLY_SOURCE = 'base = "claude"\nargs = ["--permission-mode", "plan"]\n'
COMMITTER = ["-c", "user.name=Test", "-c", "user.email=test@example.invalid"]
UNSIGNED = ["-c", "commit.gpgsign=false"]
# The structured output that the fake Claude Code prints.
CLAUDE_STDOUT = '{"type":"result","subtype":"success"}'


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

        framed = wrapper.result_envelope(7, stdout, stderr, "structured", "base")
        envelope = json.loads(framed)

        self.assertTrue(framed.endswith(b"\n"))
        self.assertEqual(envelope["protocol"], wrapper.RESULT_PROTOCOL)
        self.assertEqual(envelope["mode"], "structured")
        self.assertEqual(envelope["exit_code"], 7)
        self.assertEqual(envelope["stdout_base64"], "eyJ0eXBlIjoicmVzdWx0IiwicmVzdWx0Ijoie1wib2tcIjp0cnVlfSJ9Cg==")
        self.assertEqual(envelope["stderr_base64"], "ZGlhZ25vc3RpYxtbMG0=")
        self.assertEqual(envelope["checked_checkout"], "base")

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

    def test_claude_arguments_end_with_the_fixed_read_only_tail(self) -> None:
        tail = [
            "--setting-sources",
            "user",
            "--print",
            "--output-format",
            "json",
            "--no-session-persistence",
        ]

        structured = wrapper.claude_arguments(
            "/usr/bin/claude", ["--model", "opus"], '{"type":"object"}'
        )
        text = wrapper.claude_arguments("/usr/bin/claude", ["--model", "opus"], None)

        self.assertEqual(
            structured,
            ["/usr/bin/claude", "--model", "opus", *tail, "--json-schema", '{"type":"object"}'],
        )
        self.assertEqual(text, ["/usr/bin/claude", "--model", "opus", *tail])

    def test_claude_run_drops_session_identity_and_github_tokens(self) -> None:
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
            wrapper.claude_environment(environment),
            {"PATH": "/usr/bin", "HOME": "/tmp/home", "POHUNEK_SESSION_ID_HINT": "kept"},
        )


class BaseCheckoutTests(unittest.TestCase):
    """A base checkout starts at the base branch tip; Claude reads the head through git."""

    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / "home").mkdir()
        self.environment = {
            "PATH": "/usr/bin:/bin",
            "HOME": str(self.root / "home"),
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
        }
        source = self.root / "source"
        self.git(self.root, "init", "--quiet", "--initial-branch=main", str(source))
        start_sha = self.commit(source, "start")
        # The trusted base SHA is the base when the pull request was opened; main moved on.
        self.base_sha = self.commit(source, "base")
        self.git(source, "checkout", "--quiet", "-b", "feature")
        self.head_sha = self.commit(source, "pull request")
        self.git(source, "checkout", "--quiet", "main")
        self.tip_sha = self.commit(source, "base moves on")
        # A force push of main replaces the base with this commit.
        self.git(source, "checkout", "--quiet", "-b", "rewritten", start_sha)
        self.rewritten_sha = self.commit(source, "rewritten base")
        origin = self.root / "origin.git"
        self.git(self.root, "init", "--quiet", "--bare", str(origin))
        self.git(
            source, "push", "--quiet", str(origin), "main", "rewritten",
            "feature:refs/pull/9/head",
        )
        # Pohunek fetches only the base branch for a base checkout.
        self.checkout = self.root / "checkout"
        self.git(self.root, "init", "--quiet", str(self.checkout))
        self.git(self.checkout, "remote", "add", "origin", origin.as_uri())
        self.git(self.checkout, "fetch", "--quiet", "origin", "main")
        self.git(self.checkout, "checkout", "--quiet", "--detach", self.tip_sha)

    def fetch_head(self) -> None:
        """Puts the head in the repository, as an earlier head checkout or fetch does."""
        self.git(self.checkout, "fetch", "--quiet", "origin", "refs/pull/9/head")

    def git(self, cwd: Path, *arguments: str) -> str:
        return subprocess.run(
            ["git", *arguments],
            cwd=cwd,
            env=self.environment,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def commit(self, cwd: Path, message: str) -> str:
        self.git(cwd, *COMMITTER, *UNSIGNED, "commit", "--quiet", "--allow-empty", "-m", message)
        return self.git(cwd, "rev-parse", "HEAD")

    def prompt(self, checkout: str = "base") -> str:
        """Returns the trusted context of the prompt; `go` gives one without trusted lines."""
        context = (
            "Review the pull request.\n\nTrusted request context:\n"
            "Repository: acme/widget\nPull request: 9\n"
            f"Head SHA: {self.head_sha}\nReviewer: Opus\nReview scope: full\n"
        )
        if checkout == "go":
            return context
        return (
            f"{context}- Trusted head SHA: {self.head_sha}\n"
            f"- Trusted base SHA: {self.base_sha}\n- Trusted checkout: {checkout}\n"
        )

    def install(self, during_run: str = "") -> tuple[Path, Path]:
        """Installs the adapter with a fake Claude Code that runs `during_run` and answers."""
        fake = self.root / "fake-claude"
        fake.write_text(
            "#!/bin/sh\n"
            "cat >/dev/null\n"
            f": >{self.root / 'claude-ran'}\n"
            f"{during_run}\n"
            f"printf '%s' '{CLAUDE_STDOUT}'\n",
            encoding="utf-8",
        )
        fake.chmod(0o755)
        (self.root / "bin").mkdir(exist_ok=True)
        script = self.root / "bin" / "claude-adapter"
        shutil.copy(ROOT / "claude-adapter", script)
        script.chmod(0o755)
        sidecar = Path(str(script) + wrapper.SIDECAR_SUFFIX)
        sidecar.write_text(f"{fake}\n", encoding="utf-8")
        sidecar.chmod(0o600)
        return script, fake

    def run_adapter(self, prompt: str, during_run: str = "") -> tuple[dict, str]:
        """Runs the adapter on a structured request; returns its result line and stderr."""
        script, fake = self.install(during_run)
        request = json.dumps(
            {
                "protocol": wrapper.REQUEST_PROTOCOL,
                "prompt": prompt,
                "json_schema": {"type": "object"},
            }
        )
        primary, secondary = pty.openpty()
        process = subprocess.Popen(
            [str(script), str(fake), "262144"],
            env=self.environment,
            cwd=self.checkout,
            stdin=secondary,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        os.close(secondary)
        line = b""
        try:
            # The adapter flushes pending input when it switches the terminal to raw mode.
            deadline = time.monotonic() + 10
            while termios.tcgetattr(primary)[3] & termios.ICANON:
                self.assertLess(time.monotonic(), deadline, "the adapter never read its input")
                time.sleep(0.01)
            frame = wrapper.BRACKETED_PASTE_START + request.encode("utf-8")
            os.write(primary, frame + wrapper.BRACKETED_PASTE_END + wrapper.SUBMIT)
            # The adapter keeps running after its one result line, so read only that line.
            deadline = time.monotonic() + 30
            while not line.endswith(b"\n"):
                remaining = deadline - time.monotonic()
                self.assertGreater(remaining, 0, f"no result line, read {line!r}")
                ready, _, _ = select.select([process.stdout], [], [], remaining)
                if ready:
                    chunk = os.read(process.stdout.fileno(), 65536)
                    self.assertTrue(chunk, f"the adapter exited after {line!r}")
                    line += chunk
        finally:
            process.kill()
            _, stderr = process.communicate()
            os.close(primary)
        return json.loads(line), stderr.decode("utf-8")

    def claude_ran(self) -> bool:
        return (self.root / "claude-ran").exists()

    def assert_refused(self, result: dict, refusal: str, message: str) -> None:
        self.assertEqual(
            set(result),
            {"protocol", "mode", "exit_code", "stdout_base64", "stderr_base64", "refusal"},
        )
        self.assertEqual(result["refusal"], refusal)
        self.assertEqual(result["exit_code"], wrapper.EXIT_USAGE)
        self.assertEqual(result["stdout_base64"], "")
        self.assertIn(message, base64.b64decode(result["stderr_base64"]).decode("utf-8"))

    def test_base_checkout_with_the_head_present_runs_claude_and_leaves_the_checkout_unchanged(
        self,
    ) -> None:
        self.fetch_head()
        refs = self.git(self.checkout, "for-each-ref")

        result, stderr = self.run_adapter(self.prompt())

        self.assertEqual(result["exit_code"], 0, result)
        self.assertNotIn("refusal", result)
        self.assertEqual(result["checked_checkout"], "base")
        self.assertEqual(base64.b64decode(result["stdout_base64"]).decode(), CLAUDE_STDOUT)
        self.assertEqual(stderr, "")
        self.assertEqual(self.git(self.checkout, "rev-parse", "HEAD"), self.tip_sha)
        self.assertEqual(self.git(self.checkout, "status", "--porcelain"), "")
        self.assertEqual(self.git(self.checkout, "for-each-ref"), refs)

    def test_base_checkout_without_the_head_is_refused_before_claude_runs(self) -> None:
        result, stderr = self.run_adapter(self.prompt())

        self.assertEqual(
            result,
            {
                "protocol": wrapper.RESULT_PROTOCOL,
                "mode": "structured",
                "exit_code": wrapper.EXIT_USAGE,
                "stdout_base64": "",
                "stderr_base64": base64.b64encode(
                    b"claude-adapter: trusted pull request head is unavailable in the review "
                    b"checkout\n"
                ).decode("ascii"),
                "refusal": "head_unavailable",
            },
        )
        # The refusal is the only output: pohunek reads stderr from the same terminal.
        self.assertEqual(stderr, "")
        self.assertFalse(self.claude_ran())

    def test_base_checkout_refuses_any_other_head(self) -> None:
        moved = "git checkout --quiet --detach HEAD~1"
        for name, start, during_run, message in (
            ("pull request head", self.head_sha, "", "is the pull request head or contains it"),
            ("rewritten base", self.rewritten_sha, "", "does not descend from the trusted"),
            ("moved by the run", self.tip_sha, moved, "HEAD changed during the review"),
        ):
            with self.subTest(name=name):
                self.fetch_head()
                self.git(self.checkout, "fetch", "--quiet", "origin", "rewritten")
                self.git(self.checkout, "checkout", "--quiet", "--detach", start)

                result, stderr = self.run_adapter(self.prompt(), during_run)

                self.assert_refused(result, "checkout_invalid", message)
                self.assertEqual(stderr, "")

    def test_prompt_without_a_checkout_line_needs_the_head_checked_out(self) -> None:
        # A prompt with only `Head SHA:` gets the strict head check.
        self.fetch_head()
        result, _ = self.run_adapter(self.prompt("go"))
        self.assert_refused(result, "checkout_invalid", "does not match the trusted pull request")
        self.assertFalse(self.claude_ran())

        self.git(self.checkout, "checkout", "--quiet", "--detach", self.head_sha)
        for prompt in (self.prompt("go"), self.prompt("head")):
            with self.subTest(prompt=prompt):
                result, _ = self.run_adapter(prompt)
                self.assertEqual(result["exit_code"], 0, result)
                self.assertNotIn("refusal", result)
                self.assertEqual(result["checked_checkout"], "head")


class VersionProbeTests(unittest.TestCase):
    """Pohunek probes the profile program with a bare --version, a private HOME and only PATH."""

    def install_wrapper(self, root: Path, sidecar: str | None, mode: int = 0o600) -> Path:
        fake_binary = root / "fake-claude"
        fake_binary.write_text(
            '#!/bin/sh\n'
            'if [ "$#" -eq 1 ] && [ "$1" = --version ]; then\n'
            '  echo "2.1.296 (Claude Code)"\n'
            # Names only: one dropped token, one dropped prefix, the session identity, one kept.
            '  for name in ${GH_TOKEN+GH_TOKEN} ${AUTOREVIEW_GH_TOKEN+AUTOREVIEW_GH_TOKEN} \\\n'
            '    ${POHUNEK_SESSION_ID+POHUNEK_SESSION_ID} ${POHUNEK_HINT+POHUNEK_HINT}; do\n'
            '    echo "$name" >&2\n'
            '  done\n'
            '  exit 0\n'
            'fi\n'
            'echo "unexpected arguments: $*" >&2\n'
            'exit 2\n',
            encoding="utf-8",
        )
        fake_binary.chmod(0o755)
        installed = root / "bin"
        installed.mkdir(exist_ok=True)
        script = installed / "claude-adapter"
        shutil.copy(ROOT / "claude-adapter", script)
        script.chmod(0o755)
        if sidecar is not None:
            file = Path(str(script) + wrapper.SIDECAR_SUFFIX)
            file.write_text(sidecar.replace("{fake}", str(fake_binary)), encoding="utf-8")
            file.chmod(mode)
        return script

    def run_wrapper(
        self,
        root: Path,
        script: Path,
        arguments: list[str],
        session: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        home = root / "home"
        home.mkdir(exist_ok=True)
        return subprocess.run(
            [str(script), *arguments],
            env={"PATH": os.environ["PATH"], "HOME": str(home), **(session or {})},
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
        self.assertEqual(result.stdout, "2.1.296 (Claude Code)\n")
        self.assertEqual(result.stderr, "")

    def test_bare_version_runs_with_the_environment_of_a_review_run(self) -> None:
        session = {
            "GH_TOKEN": "t",
            "AUTOREVIEW_GH_TOKEN": "t",
            "POHUNEK_SESSION_ID": "s",
            "POHUNEK_HINT": "kept",
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = self.install_wrapper(root, "{fake}\n")
            result = self.run_wrapper(root, script, ["--version"], session)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "2.1.296 (Claude Code)\n")
        # The fake prints the names it sees; only the kept one reaches the probe.
        self.assertEqual(result.stderr, "POHUNEK_HINT\n")

    def test_missing_or_insecure_path_file_fails_fast(self) -> None:
        for sidecar, mode, message in (
            (None, 0o600, "is missing"),
            ("{fake}\n", 0o666, "writable"),
            ("relative/claude\n", 0o600, "absolute"),
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
            result = self.run_wrapper(root, script, ["/bin/true", "262144"])

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
            fake = root / "fake-claude"
            source = root / "source.toml"
            source.write_text(READ_ONLY_SOURCE, encoding="utf-8")
            source.chmod(0o600)
            rendered = subprocess.run(
                [
                    str(ROOT / "render_profile.py"),
                    "--source", str(source),
                    "--target", str(root / "agents" / "profile.toml"),
                    "--wrapper", str(script),
                    "--claude-binary", str(fake),
                    "--maximum-input-bytes", "262144",
                    "--model", "opus",
                    "--effort", "medium",
                    "--manifest-name", "no-detection",
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            result = self.run_wrapper(root, script, ["--version"])

        self.assertEqual(rendered.returncode, 0, rendered.stderr)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "2.1.296 (Claude Code)\n")


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
                'OTEL_EXPORTER_OTLP_ENDPOINT = "https://collector.invalid/otlp"\n'
                'CLAUDE_CONFIG_DIR = "/home/review/.claude-second"\n'
                'GH_TOKEN = "t"\n'
                'NODE_OPTIONS = "--require=/tmp/hook.js"\n'
                'OTELCOL_ENDPOINT = "x"\n'
                'otel_exporter = "x"\n',
                encoding="utf-8",
            )
            source.chmod(0o600)

            report = io.StringIO()
            with contextlib.redirect_stderr(report):
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
            # Only telemetry and the config home are copied; the rest is named, never shown.
            self.assertEqual(
                profile["env"],
                {
                    "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
                    "CLAUDE_CONFIG_DIR": "/home/review/.claude-second",
                    "OTEL_EXPORTER_OTLP_ENDPOINT": "https://collector.invalid/otlp",
                },
            )
            self.assertEqual(
                report.getvalue(),
                "dropped environment keys of the base profile: "
                "GH_TOKEN, NODE_OPTIONS, OTELCOL_ENDPOINT, otel_exporter\n",
            )
            self.assertTrue(profile["input_rules"]["bracketed_paste"])
            self.assertFalse(profile["resume"]["resumable"])
            self.assertNotIn("fork", profile)

    def test_source_without_an_environment_table_renders_without_telemetry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.toml"
            source.write_text(READ_ONLY_SOURCE, encoding="utf-8")
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

        self.assertEqual(tomllib.loads(rendered)["env"], {})

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

    def render_with_args(self, source_args: list[str]) -> str:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.toml"
            source.write_text(
                'base = "claude"\n' f"args = {json.dumps(source_args)}\n", encoding="utf-8"
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

    def test_rejects_every_wrapper_controlled_argument(self) -> None:
        for argument in sorted(installer.WRAPPER_CONTROLLED_ARGUMENTS):
            with self.subTest(argument=argument):
                with self.assertRaisesRegex(ValueError, "wrapper-controlled"):
                    self.render_with_args([argument])
        for argument in ("--resume", "--continue", "--print", "--json-schema"):
            self.assertIn(argument, installer.WRAPPER_CONTROLLED_ARGUMENTS)

    def test_rejects_permission_widening_arguments(self) -> None:
        plan = ["--permission-mode", "plan"]
        for widening in (
            ["--dangerously-skip-permissions"],
            ["--allow-dangerously-skip-permissions"],
            ["--add-dir", "/"],
            ["--add-dir=/"],
            ["--agents", "{}"],
            ["--allowedTools", "Bash,Edit,Write"],
            ["--allowed-tools", "Bash"],
            ["--allowed-tools=Bash"],
            ["--disallowedTools", "Bash"],
            ["--disallowed-tools", "Bash"],
            ["--mcp-config", "servers.json"],
            ["--mcp-config=servers.json"],
            ["--strict-mcp-config"],
            ["--plugin-dir", "plugin"],
            ["--plugin-url", "https://example.invalid/p.zip"],
            ["--permission-prompt-tool", "tool"],
            ["--permission-prompts", "none"],
            ["--tools", "Bash"],
        ):
            with self.subTest(widening=widening):
                with self.assertRaisesRegex(ValueError, "permission-widening"):
                    self.render_with_args([*plan, *widening])

    def test_permission_mode_is_an_allowlist(self) -> None:
        for mode in ("plan", "default"):
            for source_args in (["--permission-mode", mode], [f"--permission-mode={mode}"]):
                with self.subTest(source_args=source_args):
                    self.render_with_args(source_args)
        for source_args in (
            [],
            ["--permission-mode"],
            ["--permission-mode", "acceptEdits"],
            ["--permission-mode=acceptEdits"],
            ["--permission-mode", "auto"],
            ["--permission-mode", "dontAsk"],
            ["--permission-mode", "bypassPermissions"],
            ["--permission-mode=bypassPermissions"],
            ["--permission-mode", "Plan"],
            ["--permission-mode", "plan", "--permission-mode", "plan"],
            ["--permission-mode", "plan", "--permission-mode", "acceptEdits"],
        ):
            with self.subTest(source_args=source_args):
                with self.assertRaisesRegex(ValueError, "--permission-mode once"):
                    self.render_with_args(source_args)

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
            source.write_text(READ_ONLY_SOURCE, encoding="utf-8")
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
        for source_args in ('["--permission-mode", "plan", "--effort"]', '["--permission-mode", "plan", "--effort", "--verbose"]', '["--permission-mode", "plan", "--effort="]'):
            with self.subTest(source_args=source_args):
                with self.assertRaisesRegex(ValueError, "invalid effort option"):
                    self.render_with_source_args(source_args)

    def test_renders_the_profile_that_profiles_install_writes(self) -> None:
        # `autoreview profiles install` writes these files under its own first line, and its
        # Rust test compares them with the same golden files.
        digest = "sha256:" + "0123456789abcdef" * 4
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.toml"
            source.write_text(
                'base = "claude"\n'
                f'package = "pohunek.runtime.claude"\ndigest = "{digest}"\n'
                'args = ["--permission-mode", "plan"]\n',
                encoding="utf-8",
            )
            source.chmod(0o600)
            rendered = installer.render_profile(
                source,
                Path(
                    "/home/review/.local/share/autoreview/adapters/autoreview-sonnet/"
                    "claude-adapter"
                ),
                Path("/usr/local/bin/claude"),
                1048576,
                "claude-sonnet-4-5",
                "high",
                "autoreview-sonnet",
            )
        tests = Path(__file__).resolve().parent
        golden = (tests / "installed-profile.toml").read_text(encoding="utf-8")
        self.assertEqual(rendered.split("\n", 1)[1], golden)
        manifest = (tests / "installed-manifest.toml").read_text(encoding="utf-8")
        self.assertEqual(installer.MANIFEST_CONTENT, manifest)


if __name__ == "__main__":
    unittest.main()
