"""A group turn: several agents collaborate within one conversation turn.

Stage 1 of the collaboration core — **shared context**. A turn is a pipeline, not
a broadcast: each agent's prompt carries what the agents before it produced, so a
later agent continues their work instead of answering the same question a second
time.

The rule lives here (L3) rather than in the API layer because it is orchestration
logic, not request shaping: `api/chat.py` drives the loop and broadcasts, this
module decides what each speaker is told.
"""

from __future__ import annotations

from ..core.models import AgentSpec

#: 每个上游产出注入下游 prompt 的字符上限。协作靠「接续」不靠「搬运全文」：
#: 全文会让上下文随人数线性膨胀，而下游真正需要的是结论与已改动的东西，不是逐字复述。
PER_OUTPUT_LIMIT = 4000


def _clip(text: str, limit: int) -> str:
    body = text.strip()
    if len(body) <= limit:
        return body
    return f"{body[:limit]}\n…（上文已截断，原文共 {len(body)} 字）"


def build_prompt(
    spec: AgentSpec,
    peers: list[AgentSpec],
    content: str,
    prior: list[tuple[str, str]],
    *,
    limit: int = PER_OUTPUT_LIMIT,
) -> str:
    """Prompt for one agent in a group turn.

    `prior` is the ordered `(display, output)` of the agents that already spoke.
    Empty for the first speaker, where this degrades to a plain persona + task.
    """
    parts: list[str] = []

    persona = spec.persona()
    if persona:
        parts.append(persona)

    others = "、".join(peer.display for peer in peers if peer.id != spec.id) or "（无）"
    parts.append(f"在场 agent（你之外）：{others}")

    if prior:
        # Name each block: the point is that a later agent knows who said what,
        # so it can build on a specific contribution and disagree with a specific
        # claim instead of arguing with an anonymous wall of text.
        blocks = ["以下是前面 agent 已经产出的内容，请接着它们推进，不要复述："]
        for name, output in prior:
            blocks.append(f"--- {name} ---\n{_clip(output, limit)}")
        parts.append("\n\n".join(blocks))

    parts.append(f"用户的任务：\n{content}")
    return "\n\n".join(parts)
