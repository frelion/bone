"""Run BONE as an installed agent inside Harbor task containers."""

import json
import shlex
from pathlib import PurePosixPath
from typing import override

from harbor.agents.installed.base import BaseInstalledAgent, with_prompt_template
from harbor.agents.model_connection import ModelConnectionSpec
from harbor.agents.options import InstalledAgentOptions
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext
from harbor.models.trial.paths import EnvironmentPaths


class BoneOptions(InstalledAgentOptions):
    """BONE currently uses only Harbor's standard installed-agent options."""


class BoneAgent(BaseInstalledAgent):
    """Official release-binary adapter for reproducible BONE evaluations."""

    MODEL_CONNECTION = ModelConnectionSpec(default_provider="openai")
    options_model = BoneOptions
    _REPOSITORY = "frelion/bone"

    @staticmethod
    @override
    def name() -> str:
        return "bone"

    @override
    def get_version_command(self) -> str | None:
        return "bone --version"

    @override
    def parse_version(self, stdout: str) -> str:
        return stdout.strip().removeprefix("bone ")

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        await self.ensure_system_dependencies(
            environment, ("curl", "ca-certificates", "git")
        )
        version = self._version
        release = f"v{version.removeprefix('v')}" if version else "latest"
        release_path = f"download/{release}" if version else "latest/download"
        base = f"https://github.com/{self._REPOSITORY}/releases/{release_path}"
        command = f"""
set -euo pipefail
case "$(uname -m)" in
  x86_64|amd64) asset=bone-linux-x86_64 ;;
  aarch64|arm64) asset=bone-linux-aarch64 ;;
  *) echo "unsupported BONE architecture: $(uname -m)" >&2; exit 1 ;;
esac
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
curl --fail --location --retry 3 --output "$tmp_dir/$asset" {shlex.quote(base)}/$asset
curl --fail --location --retry 3 --output "$tmp_dir/SHA256SUMS" {shlex.quote(base)}/SHA256SUMS
(cd "$tmp_dir" && grep "  $asset$" SHA256SUMS | sha256sum --check --status)
install -m 0755 "$tmp_dir/$asset" /usr/local/bin/bone
bone --version
"""
        await self.exec_as_root(environment, command=command)

    @with_prompt_template
    @override
    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        if not self.model_name:
            raise ValueError("BONE requires a Harbor model name")
        access = self.model_connection
        if not access.api_key:
            raise ValueError(
                "BONE Harbor evaluations require a provider API key; "
                "a host ChatGPT subscription login is not available inside task containers"
            )
        model = self.model_name.split("/")[-1]
        provider_prefix = self.model_name.split("/", 1)[0].lower()
        if provider_prefix == "anthropic":
            provider = "anthropic"
        else:
            provider = "openai-responses"

        agent_dir = PurePosixPath(EnvironmentPaths.agent_dir)
        prompt_path = agent_dir / "bone-prompt.txt"
        result_path = agent_dir / "bone-result.json"
        trajectory_path = agent_dir / "bone-trajectory.json"
        base_url = access.configured_base_url
        base_url_arg = f" --base-url {shlex.quote(base_url)}" if base_url else ""
        env = {"BONE_BENCHMARK_API_KEY": access.api_key or ""}

        await self.exec_as_agent(
            environment,
            command=(
                f"mkdir -p {shlex.quote(agent_dir.as_posix())} /tmp/bone-data && "
                "chmod 700 /tmp/bone-data && "
                f"printf %s {shlex.quote(instruction)} > {shlex.quote(prompt_path.as_posix())}"
            ),
        )
        # A task-level agent failure must still allow Harbor's verifier to score
        # any useful workspace changes. The native exit code is retained in the
        # result JSON and copied to a separate file for diagnosis.
        await self.exec_as_agent(
            environment,
            command=(
                "set +e; "
                "bone run --workspace /app --data-dir /tmp/bone-data "
                f"--prompt-file {shlex.quote(prompt_path.as_posix())} "
                f"--model {shlex.quote(model)} --provider {provider} "
                "--api-key-env BONE_BENCHMARK_API_KEY "
                f"--timeout-seconds 1800{base_url_arg} "
                f"--trajectory {shlex.quote(trajectory_path.as_posix())} "
                f"--result {shlex.quote(result_path.as_posix())}; "
                "code=$?; "
                f"printf '%s\\n' \"$code\" > {shlex.quote((agent_dir / 'bone-exit-code.txt').as_posix())}; "
                "exit 0"
            ),
            env=env,
        )

    @override
    def populate_context_post_run(self, context: AgentContext) -> None:
        result_path = self.logs_dir / "bone-result.json"
        if not result_path.is_file():
            return
        try:
            result = json.loads(result_path.read_text())
        except (OSError, json.JSONDecodeError):
            self.logger.exception("Could not read BONE's headless result")
            return
        context.metadata = context.metadata or {}
        context.metadata["bone"] = {
            "status": result.get("status"),
            "exit_code": result.get("exit_code"),
            "session_id": result.get("session_id"),
            "changed_files": result.get("changed_files", []),
            "duration_ms": result.get("duration_ms"),
            "message": result.get("message"),
        }
