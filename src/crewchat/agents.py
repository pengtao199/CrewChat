from __future__ import annotations

import os
from pathlib import Path
from typing import Any

from .bodies import build_body_tool
from .config import Settings
from .executors import get_executor
from .models import AgentSpec, load_specs
from .providers import Provider, get_provider, shared_mcp_servers


class AgentFactory:
    """Turns an AgentSpec into a real `crewai.Agent`.

    Nothing about the agent shape is invented here: rules/brain/body/skills/mcps/
    memory all map onto fields crewai already has.
    """

    def __init__(self, settings: Settings, workspace: Path) -> None:
        self.settings = settings
        self.workspace = workspace

    def provider_for(self, spec: AgentSpec) -> Provider | None:
        return get_provider(self.settings, spec.brain or None)

    def skills_for(self, spec: AgentSpec) -> list[str]:
        home = Path.home() / ".agents"
        out: list[str] = []
        for name in spec.skills:
            for candidate in (
                home / "skills" / name,
                *(home / "plugins" / p / "skills" / name for p in spec.plugins),
            ):
                if (candidate / "SKILL.md").is_file():
                    out.append(str(candidate))
                    break
        return out

    def mcps_for(self, spec: AgentSpec) -> list[Any]:
        shared = shared_mcp_servers()
        from crewai.mcp.config import MCPServerStdio

        out: list[Any] = []
        for name in spec.mcps:
            entry = shared.get(name)
            if not entry:
                continue
            out.append(
                MCPServerStdio(
                    command=entry.get("command", ""),
                    args=entry.get("args") or [],
                    env=entry.get("env") or {},
                )
            )
        return out

    def tools_for(self, spec: AgentSpec) -> list[Any]:
        if not spec.is_cli_backed:
            return []
        cwd = self.cwd_for(spec)
        return [build_body_tool(spec, cwd)]

    def cwd_for(self, spec: AgentSpec) -> str:
        return str(Path(spec.cwd).expanduser() if spec.cwd else self.workspace)

    def llm_for(self, spec: AgentSpec):  # type: ignore[no-untyped-def]
        from crewai import LLM

        provider = self.provider_for(spec)
        if provider is None:
            return None
        kwargs: dict[str, Any] = {
            "model": provider.model,
            "base_url": provider.base_url,
        }
        if provider.api_key:
            kwargs["api_key"] = provider.api_key
        if provider.temperature is not None:
            kwargs["temperature"] = provider.temperature
        if provider.max_tokens is not None:
            kwargs["max_tokens"] = provider.max_tokens
        return LLM(**kwargs)

    def apply_env(self, spec: AgentSpec) -> None:
        provider = self.provider_for(spec)
        if provider and provider.api_key:
            os.environ.setdefault("OPENAI_API_KEY", provider.api_key)
            os.environ.setdefault("OPENAI_BASE_URL", provider.base_url)

    def build(self, spec: AgentSpec):  # type: ignore[no-untyped-def]
        from crewai import Agent

        self.apply_env(spec)
        kwargs: dict[str, Any] = {
            "role": spec.role or spec.display,
            "goal": spec.goal,
            "backstory": spec.backstory,
            "tools": self.tools_for(spec),
            "max_iter": spec.max_iter,
            "allow_delegation": spec.allow_delegation,
            "reasoning": spec.reasoning,
            "inject_date": spec.inject_date,
            "multimodal": spec.multimodal,
            "verbose": False,
        }
        llm = self.llm_for(spec)
        if llm is not None:
            kwargs["llm"] = llm
        skills = self.skills_for(spec)
        if skills:
            kwargs["skills"] = skills
        mcps = self.mcps_for(spec)
        if mcps:
            kwargs["mcps"] = mcps
        if spec.memory:
            kwargs["memory"] = spec.memory
        return Agent(**kwargs)

    def health(self) -> list[dict[str, Any]]:
        rows: list[dict[str, Any]] = []
        for spec in load_specs(self.settings.agents_dir):
            try:
                executor = get_executor(spec.body)
                builtin = executor.has_builtin_brain
                ok, note = True, ""
            except KeyError as exc:
                builtin, ok, note = False, False, str(exc)
            provider = self.provider_for(spec)
            rows.append(
                {
                    "id": spec.id,
                    "name": spec.name or spec.id,
                    "emoji": spec.emoji,
                    "body": spec.body,
                    "brain_model": (provider.model if provider else None),
                    "brain_provider": (provider.name if provider else None),
                    "brain_builtin": builtin,
                    "skills": spec.skills,
                    "mcps": spec.mcps,
                    "plugins": spec.plugins,
                    "memory": spec.memory,
                    "role": spec.role,
                    "ready": ok and bool(provider or builtin),
                    "note": note,
                }
            )
        return rows