"""Shared machinery for the final desired-state conformance bar."""

from __future__ import annotations

from contextlib import contextmanager
from dataclasses import dataclass, field
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import re
from typing import Any, Callable, Iterator, Sequence


SUITE_ROOT = Path(__file__).resolve().parent
REPOSITORY_ROOT = SUITE_ROOT.parents[1]

# The bar is itself commonly launched as a Tally job.  None of that outer
# executor identity belongs to the disposable daemons below: presenting the
# outer capability to an inner daemon is both invalid and a false parentage
# claim.  Keep product/fixture overrides such as TALLY_NIX_STORE_PROGRAM, but
# clear every environment name installed by the executor before spawning a
# probe.  Individual cases can still add an intentional value explicitly.
EXECUTOR_ENVIRONMENT = {
    "TALLY_ATTEMPT",
    "TALLY_BRIEF",
    "TALLY_BRIEF_HASH",
    "TALLY_CLASS",
    "TALLY_CREDENTIALS",
    "TALLY_GATE_MANIFEST",
    "TALLY_GH_COMMENT_ID",
    "TALLY_GH_CONTEXT",
    "TALLY_GH_EVENT_ID",
    "TALLY_GH_HEAD_SHA",
    "TALLY_GH_NODE_ID",
    "TALLY_GH_NUMBER",
    "TALLY_GH_REPO",
    "TALLY_GH_TRIGGER_ACTOR",
    "TALLY_GH_TRIGGER_KIND",
    "TALLY_GH_TYPE",
    "TALLY_GH_URL",
    "TALLY_JOB_ID",
    "TALLY_JOB_TOKEN",
    "TALLY_LEASE_EPOCH",
    "TALLY_NO_ENQUEUE",
    "TALLY_PARENT",
    "TALLY_POOL",
    "TALLY_SOCKET",
    "TALLY_TASK_REF",
    "TALLY_TASK_UUID",
    "TALLY_WORKSPACE_BASE_REV",
    "TALLY_WORKSPACE_BRANCH",
    "TALLY_WORKSPACE_PATH",
    "TALLY_WORKSPACE_REPO",
    "TALLY_YIELD_HOOK",
}


class ConformanceFailure(AssertionError):
    """The target ran, but observable behavior disagreed with the spec."""


class HarnessError(RuntimeError):
    """The probe could not establish a product verdict."""


@dataclass(frozen=True)
class Completed:
    argv: tuple[str, ...]
    returncode: int
    stdout: str
    stderr: str
    elapsed: float

    def json(self, context: str = "command output") -> Any:
        try:
            return json.loads(self.stdout)
        except json.JSONDecodeError as error:
            raise ConformanceFailure(
                f"{context} was not JSON: {error}; stdout={self.stdout[-2000:]!r}; "
                f"stderr={self.stderr[-2000:]!r}"
            ) from error


def run_process(
    argv: Sequence[os.PathLike[str] | str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float = 120,
    input_text: str | None = None,
) -> Completed:
    rendered = tuple(os.fspath(value) for value in argv)
    started = time.monotonic()
    try:
        result = subprocess.run(
            rendered,
            cwd=cwd,
            env=env,
            input=input_text,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
    except FileNotFoundError as error:
        raise HarnessError(f"cannot execute {rendered[0]!r}: {error}") from error
    except PermissionError as error:
        raise HarnessError(f"cannot execute {rendered[0]!r}: {error}") from error
    except subprocess.TimeoutExpired as error:
        raise HarnessError(
            f"command exceeded {timeout:.0f}s: {rendered!r}; "
            f"stdout={str(error.stdout)[-1000:]!r}; stderr={str(error.stderr)[-1000:]!r}"
        ) from error
    return Completed(
        argv=rendered,
        returncode=result.returncode,
        stdout=result.stdout,
        stderr=result.stderr,
        elapsed=time.monotonic() - started,
    )


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ConformanceFailure(message)


def require_equal(actual: Any, expected: Any, context: str) -> None:
    if actual != expected:
        expected_text = json.dumps(expected, sort_keys=True, ensure_ascii=False, indent=2)
        actual_text = json.dumps(actual, sort_keys=True, ensure_ascii=False, indent=2)
        raise ConformanceFailure(
            f"{context} differs\nexpected:\n{expected_text}\nactual:\n{actual_text}"
        )


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")


@dataclass(frozen=True)
class Case:
    case_id: str
    issues: tuple[int, ...]
    description: str
    function: Callable[["Context"], None]
    long: bool = False


CASES: list[Case] = []


def case(
    case_id: str,
    issues: Sequence[int],
    description: str,
    *,
    long: bool = False,
) -> Callable[[Callable[["Context"], None]], Callable[["Context"], None]]:
    def register(function: Callable[["Context"], None]) -> Callable[["Context"], None]:
        if any(existing.case_id == case_id for existing in CASES):
            raise RuntimeError(f"duplicate conformance case ID: {case_id}")
        CASES.append(Case(case_id, tuple(issues), description, function, long))
        return function

    return register


@dataclass
class Context:
    target: Path
    work: Path
    tally_override: Path | None = None
    driver_override: Path | None = None
    presets_override: Path | None = None
    core_test_binary_override: Path | None = None
    n_minus_one_tally_override: Path | None = None
    _tally: Path | None = field(default=None, init=False)
    _driver: Path | None = field(default=None, init=False)
    _driver_script: Path | None = field(default=None, init=False)
    _presets: dict[str, Any] | None = field(default=None, init=False)
    _core_test_binary: Path | None = field(default=None, init=False)
    _n_minus_one_tally: Path | None = field(default=None, init=False)

    @property
    def flake(self) -> str:
        return f"path:{self.target}"

    def command(
        self,
        *argv: os.PathLike[str] | str,
        cwd: Path | None = None,
        env: dict[str, str] | None = None,
        timeout: float = 120,
        input_text: str | None = None,
    ) -> Completed:
        return run_process(
            argv,
            cwd=cwd,
            env=self.environment() if env is None else env,
            timeout=timeout,
            input_text=input_text,
        )

    def environment(self, **values: os.PathLike[str] | str) -> dict[str, str]:
        env = os.environ.copy()
        for name in EXECUTOR_ENVIRONMENT:
            env.pop(name, None)
        env.update({key: os.fspath(value) for key, value in values.items()})
        return env

    def nix_build(self, attribute: str, timeout: float = 1800) -> Path:
        result = self.command(
            "nix",
            "build",
            "--no-link",
            "--print-out-paths",
            f"{self.flake}#{attribute}",
            timeout=timeout,
        )
        if result.returncode != 0:
            raise HarnessError(
                f"could not build target flake attribute {attribute!r}:\n"
                f"{result.stderr[-6000:]}"
            )
        paths = [Path(line) for line in result.stdout.splitlines() if line.strip()]
        if len(paths) != 1 or not paths[0].exists():
            raise HarnessError(
                f"nix build {attribute!r} returned unexpected paths: {result.stdout!r}"
            )
        return paths[0]

    @property
    def tally(self) -> Path:
        if self._tally is None:
            package = self.tally_override or self.nix_build("tally")
            candidate = package if package.is_file() else package / "bin/tally"
            if not candidate.is_file() or not os.access(candidate, os.X_OK):
                raise HarnessError(f"target tally executable is missing: {candidate}")
            self._tally = candidate
        return self._tally

    @property
    def driver(self) -> Path:
        if self._driver is None:
            package = self.driver_override or self.nix_build("spec-build-driver")
            candidate = package if package.is_file() else package / "bin/spec-build-driver"
            if not candidate.is_file() or not os.access(candidate, os.X_OK):
                raise HarnessError(f"packaged spec-build driver is missing: {candidate}")
            self._driver = candidate
        return self._driver

    @property
    def driver_script(self) -> Path:
        """The immutable Python payload behind the packaged driver wrapper."""
        if self._driver_script is not None:
            return self._driver_script
        driver = self.driver
        try:
            text = driver.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as error:
            raise HarnessError(f"cannot inspect packaged driver launcher {driver}: {error}") from error
        if driver.name.endswith(".py"):
            candidate = driver
        else:
            matches = re.findall(r"(/nix/store/[^\s\"']+/spec_build_driver\.py)", text)
            if len(matches) != 1:
                raise HarnessError(
                    "packaged driver wrapper did not identify exactly one immutable Python payload"
                )
            candidate = Path(matches[0])
        if not candidate.is_file():
            raise HarnessError(f"packaged driver payload is missing: {candidate}")
        self._driver_script = candidate
        return candidate

    @property
    def presets(self) -> dict[str, Any]:
        if self._presets is None:
            if self.presets_override is not None:
                try:
                    value = json.loads(self.presets_override.read_text(encoding="utf-8"))
                except (OSError, json.JSONDecodeError) as error:
                    raise HarnessError(
                        f"cannot read evaluated preset JSON {self.presets_override}: {error}"
                    ) from error
            else:
                result = self.command(
                    "nix",
                    "eval",
                    "--json",
                    f"{self.flake}#lib.adapters.presets",
                    timeout=300,
                )
                if result.returncode != 0:
                    raise HarnessError(
                        "cannot evaluate target stock adapter presets:\n"
                        + result.stderr[-6000:]
                    )
                try:
                    value = json.loads(result.stdout)
                except json.JSONDecodeError as error:
                    raise HarnessError(f"evaluated adapter presets are not JSON: {error}") from error
            if not isinstance(value, dict):
                raise HarnessError("evaluated adapter presets must be an object")
            self._presets = value
        return self._presets

    def adapter_config(self, directory: Path) -> Path:
        path = directory / "adapter-config.json"
        path.write_text(
            json.dumps({"pools": {}, "adapters": self.presets}, ensure_ascii=False),
            encoding="utf-8",
        )
        checked = self.command(self.tally, "--mode", "check-config", "--config", path)
        if checked.returncode != 0:
            raise ConformanceFailure(
                "the evaluated Nix presets are rejected by the target Rust config reader: "
                + (checked.stderr or checked.stdout)[-4000:]
            )
        return path

    @property
    def core_test_binary(self) -> Path:
        if self._core_test_binary is not None:
            return self._core_test_binary
        if self.core_test_binary_override is not None:
            candidate = self.core_test_binary_override
        else:
            cargo_target = self.work / "cargo-target"
            env = self.environment(CARGO_TARGET_DIR=cargo_target)
            result = self.command(
                "nix",
                "develop",
                self.flake,
                "-c",
                "cargo",
                "test",
                "--manifest-path",
                self.target / "Cargo.toml",
                "-p",
                "tally-core",
                "--lib",
                "--no-run",
                "--message-format=json",
                env=env,
                timeout=1800,
            )
            if result.returncode != 0:
                raise HarnessError(
                    "could not prebuild the target tally_core test binary:\n"
                    + result.stderr[-8000:]
                )
            executables: list[Path] = []
            for line in result.stdout.splitlines():
                try:
                    message = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if (
                    message.get("reason") == "compiler-artifact"
                    and message.get("profile", {}).get("test") is True
                    and message.get("target", {}).get("name") == "tally_core"
                    and isinstance(message.get("executable"), str)
                ):
                    executables.append(Path(message["executable"]))
            if len(executables) != 1:
                raise HarnessError(
                    "cargo did not identify exactly one tally_core test binary; "
                    f"found {executables!r}"
                )
            candidate = executables[0]
        if not candidate.is_file() or not os.access(candidate, os.X_OK):
            raise HarnessError(f"tally_core test binary is not executable: {candidate}")
        self._core_test_binary = candidate.resolve()
        return self._core_test_binary

    def core_test_names(self) -> set[str]:
        listed = self.command(self.core_test_binary, "--list", "--format", "terse", timeout=120)
        if listed.returncode != 0:
            raise HarnessError(
                "could not list tally_core tests: " + (listed.stderr or listed.stdout)[-4000:]
            )
        return {
            line.removesuffix(": test").strip()
            for line in listed.stdout.splitlines()
            if line.endswith(": test")
        }

    @property
    def n_minus_one_tally(self) -> Path:
        if self._n_minus_one_tally is not None:
            return self._n_minus_one_tally
        if self.n_minus_one_tally_override is not None:
            candidate = self.n_minus_one_tally_override
        else:
            source = self.work / "n-minus-one-source"
            # The release immediately before self-contained local arm wrote
            # schema 3.  Current schema 4 must fail closed in that actual N-1,
            # because its repository binding gained checkout/base/remote.
            commit = "1953bb49c80bcdb106299782713aa9292f32bd16"
            cloned = self.command(
                "git", "clone", "--quiet", "--shared", "--no-checkout", self.target, source,
                timeout=300,
            )
            if cloned.returncode != 0:
                raise HarnessError(
                    "cannot materialize the pinned N-1 source: "
                    + (cloned.stderr or cloned.stdout)[-4000:]
                )
            checked = self.command("git", "-C", source, "checkout", "--quiet", commit, timeout=120)
            if checked.returncode != 0:
                raise HarnessError(
                    f"target history does not contain pinned N-1 {commit}: "
                    + (checked.stderr or checked.stdout)[-4000:]
                )
            built = self.command(
                "nix",
                "build",
                "--no-link",
                "--print-out-paths",
                f"path:{source}#tally",
                timeout=1800,
            )
            if built.returncode != 0:
                raise HarnessError("cannot build pinned N-1 tally:\n" + built.stderr[-8000:])
            paths = [Path(line) for line in built.stdout.splitlines() if line.strip()]
            if len(paths) != 1:
                raise HarnessError(f"N-1 build returned unexpected paths: {built.stdout!r}")
            candidate = paths[0] / "bin/tally"
        if candidate.is_dir():
            candidate = candidate / "bin/tally"
        if not candidate.is_file() or not os.access(candidate, os.X_OK):
            raise HarnessError(f"pinned N-1 tally is not executable: {candidate}")
        self._n_minus_one_tally = candidate.resolve()
        return self._n_minus_one_tally

    def run_core_test(self, name: str, timeout: float = 180) -> Completed:
        return self.command(
            self.core_test_binary,
            name,
            "--exact",
            "--nocapture",
            timeout=timeout,
        )

    @contextmanager
    def daemon(
        self,
        directory: Path,
        config: dict[str, Any],
        *,
        extra_env: dict[str, str] | None = None,
    ) -> Iterator["Daemon"]:
        daemon = Daemon.start(self, directory, config, extra_env=extra_env)
        try:
            yield daemon
        finally:
            daemon.stop()


@dataclass
class Daemon:
    context: Context
    root: Path
    config: Path
    socket: Path
    state: Path
    data: Path
    log: Path
    process: subprocess.Popen[str]
    log_handle: Any

    @classmethod
    def start(
        cls,
        context: Context,
        root: Path,
        config_value: dict[str, Any],
        *,
        extra_env: dict[str, str] | None = None,
    ) -> "Daemon":
        root.mkdir(parents=True, exist_ok=True)
        config = root / "config.json"
        socket = root / "tally.sock"
        state = root / "state"
        data = root / "data"
        log = root / "daemon.log"
        config.write_text(json.dumps(config_value), encoding="utf-8")
        checked = context.command(
            context.tally,
            "--mode",
            "check-config",
            "--config",
            config,
        )
        if checked.returncode != 0:
            raise ConformanceFailure(
                "test configuration was rejected by the public config boundary: "
                + (checked.stderr or checked.stdout)[-4000:]
            )
        env = context.environment()
        if extra_env:
            env.update(extra_env)
        log_handle = log.open("w+", encoding="utf-8")
        try:
            process = subprocess.Popen(
                [
                    os.fspath(context.tally),
                    "--config",
                    os.fspath(config),
                    "--socket",
                    os.fspath(socket),
                    "daemon",
                    "run",
                    "--cpu-weight",
                    "100",
                    "--memory-max-bytes",
                    "8589934592",
                    "--state-dir",
                    os.fspath(state),
                    "--data-dir",
                    os.fspath(data),
                ],
                env=env,
                text=True,
                stdout=log_handle,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        except OSError as error:
            log_handle.close()
            raise HarnessError(f"could not start target daemon: {error}") from error
        daemon = cls(context, root, config, socket, state, data, log, process, log_handle)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if socket.is_socket():
                probe = daemon.tally("query", "pools", timeout=5)
                if probe.returncode == 0:
                    return daemon
            if process.poll() is not None:
                break
            time.sleep(0.05)
        daemon.stop()
        detail = log.read_text(encoding="utf-8", errors="replace")[-8000:] if log.exists() else ""
        raise HarnessError(f"target daemon did not become ready:\n{detail}")

    def tally(
        self,
        *arguments: os.PathLike[str] | str,
        timeout: float = 120,
        env: dict[str, str] | None = None,
    ) -> Completed:
        merged = self.context.environment()
        if env:
            merged.update(env)
        return self.context.command(
            self.context.tally,
            "--config",
            self.config,
            "--socket",
            self.socket,
            *arguments,
            timeout=timeout,
            env=merged,
        )

    def stop(self) -> None:
        if self.process.poll() is None:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.process.wait(timeout=5)
        if not self.log_handle.closed:
            self.log_handle.flush()
            self.log_handle.close()


def copy_executable(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)
    destination.chmod(destination.stat().st_mode | 0o111)


def make_case_directory(context: Context, case_id: str) -> Path:
    path = context.work / "cases" / case_id
    path.mkdir(parents=True, exist_ok=True)
    return path
