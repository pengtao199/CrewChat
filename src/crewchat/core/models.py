from __future__ import annotations

from pathlib import Path

from pydantic import BaseModel, ConfigDict, Field


class AgentSpec(BaseModel):
    """An agent is brain + body + capabilities.

    Field names mirror `crewai.Agent` so the same file works with `crewai run`
    and with CrewChat, and so adding a field never requires teaching two schemas.
    """

    model_config = ConfigDict(extra="ignore")

    id: str
    name: str = ""
    emoji: str = "🤖"

    role: str = ""
    goal: str = ""
    backstory: str = ""

    brain: str = ""
    body: str = "llm"

    skills: list[str] = Field(default_factory=list)
    mcps: list[str] = Field(default_factory=list)
    plugins: list[str] = Field(default_factory=list)
    memory: bool | str = False
    knowledge: list[str] = Field(default_factory=list)

    tools: list[str] = Field(default_factory=list)
    max_iter: int = 24
    allow_delegation: bool = False
    reasoning: bool = False
    inject_date: bool = True
    multimodal: bool = False

    cwd: str | None = None
    enabled: bool = True

    @property
    def display(self) -> str:
        return f"{self.emoji} {self.name or self.id}".strip()

    @property
    def is_cli_backed(self) -> bool:
        return self.body != "llm"

    def resolve_cwd(self, workspace: Path) -> Path:
        """该 Agent 的执行目录：显式 cwd 优先，否则用项目工作区。"""
        return Path(self.cwd).expanduser() if self.cwd else workspace

    def persona(self) -> str:
        parts = [p.strip() for p in (self.role, self.goal, self.backstory) if p and p.strip()]
        return "\n".join(parts)


def spec_file(agents_dir: Path) -> list[Path]:
    if not agents_dir.is_dir():
        return []
    return sorted(agents_dir.glob("*.jsonc")) + sorted(agents_dir.glob("*.json"))


def load_specs(agents_dir: Path) -> list[AgentSpec]:
    from crewai.project.json_loader import load_jsonc_file

    out: list[AgentSpec] = []
    for path in spec_file(agents_dir):
        raw = load_jsonc_file(str(path))
        raw.pop("id", None)
        raw.pop("llm", None)
        out.append(AgentSpec(id=path.stem, **raw))
    return [s for s in out if s.enabled]