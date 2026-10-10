"""A group turn: several agents collaborate within one conversation turn.

Stage 1 is shared context: each agent's prompt carries what the agents before it
produced, so a later agent continues their work instead of answering the same
question a second time.

Stage 2 is an explicit handoff. An agent requests one by writing
``handoff(to="名字", task="子任务")``. This module only recognizes the request
and decides whether the loop should run it. The agent does not start a process,
and the executors never see the queue.

The rule lives here (L3) rather than in the API layer because it is orchestration
logic, not request shaping: `api/chat.py` drives the loop and broadcasts, this
module decides what each speaker is told.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from ..core.models import AgentSpec, CollaborationTask

#: 每个上游产出注入下游 prompt 的字符上限。协作靠「接续」不靠「搬运全文」：
#: 全文会让上下文随人数线性膨胀，而下游真正需要的是结论与已改动的东西，不是逐字复述。
PER_OUTPUT_LIMIT = 4000

_HANDOFF_HINT = (
    '若要把子任务交给在场的另一个 agent，单独一行写 handoff(to="对方名字", task="子任务")。'
    "这只是请求，由会话循环决定是否执行，你不能自己启动进程。"
)

_PLAN_HINT = (
    "若你负责拆分任务，按依赖顺序每个子任务一行："
    'plan(owner="对方名字", task="子任务", done="完成标准", after="上一步负责人")。'
    "没有依赖就省略 after。循环会按这个顺序执行，你不能自己启动进程。"
)

_PLAN_CALL = re.compile(
    r"""plan\(\s*owner\s*=\s*(?P<q1>["'])(?P<owner>.*?)\1\s*,\s*task\s*=\s*(?P<q2>["'])(?P<task>.*?)\3\s*,\s*done\s*=\s*(?P<q3>["'])(?P<done>.*?)\5(?:\s*,\s*after\s*=\s*(?P<q4>["'])(?P<after>.*?)\7)?\s*\)"""
)

# One call, arguments in this order, quotes around each value. Anything looser
# (prose that merely mentions the word) must not enqueue work.
_HANDOFF_CALL = re.compile(
    r"""handoff\(\s*to\s*=\s*(?P<q1>["'])(?P<to>.*?)\1\s*,\s*task\s*=\s*(?P<q2>["'])(?P<task>.*?)\3\s*\)"""
)


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
    parts.append(_HANDOFF_HINT)
    parts.append(_PLAN_HINT)

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


@dataclass
class TurnStep:
    """One speaker the loop still has to run.

    `roster` is the default pipeline (the user's task, plus earlier output).
    `handoff` replaces that task with a subtask another agent requested.
    """

    spec: AgentSpec
    kind: str
    task: str
    requester: str = ""


def parse_handoff(text: str) -> tuple[str, str] | None:
    """The first ``handoff(to, task)`` in a reply, or None when it isn't a real request."""
    match = _HANDOFF_CALL.search(text)
    if match is None:
        return None
    target = match.group("to").strip()
    task = match.group("task").strip()
    if not target or not task:
        return None
    return target, task


def resolve_handoff(to: str, peers: list[AgentSpec], requester_id: str) -> AgentSpec | None:
    """Who `to` names, among the agents actually in this turn.

    Matches id, display, or name. An unknown name or a request to run oneself
    is refused: the loop only delegates to someone present.
    """
    needle = to.strip()
    if not needle:
        return None
    others = [spec for spec in peers if spec.id != requester_id]
    for spec in others:
        if needle in (spec.id, spec.display, spec.name):
            return spec
    folded = needle.casefold()
    matches = [spec for spec in others if folded in {spec.id.casefold(), spec.name.casefold()}]
    if len(matches) == 1:
        return matches[0]
    return None


def plan_steps(specs: list[AgentSpec], content: str) -> list[TurnStep]:
    """The default pipeline: every present agent, in order, on the user's task."""
    return [TurnStep(spec=spec, kind="roster", task=content) for spec in specs]


def accept_handoff(
    steps: list[TurnStep],
    index: int,
    target: AgentSpec,
    task: str,
    requester: str,
    *,
    max_steps: int,
) -> bool:
    """Queue `target` to run `task`, or refuse.

    A teammate who has not spoken yet is retargeted, so they do the subtask
    instead of answering the user again. Someone who already spoke is appended,
    until the step cap — that cap is what stops two agents handing work back
    and forth forever.
    """
    task = task.strip()
    if not task:
        return False
    for step in steps[index + 1 :]:
        if step.spec.id != target.id:
            continue
        if step.kind == "handoff":
            return False
        step.kind = "handoff"
        step.task = task
        step.requester = requester
        return True
    if len(steps) >= max_steps:
        return False
    steps.append(TurnStep(spec=target, kind="handoff", task=task, requester=requester))
    return True


def build_handoff_prompt(
    spec: AgentSpec,
    peers: list[AgentSpec],
    task: str,
    prior: list[tuple[str, str]],
    requester: str,
    *,
    limit: int = PER_OUTPUT_LIMIT,
) -> str:
    """Prompt for an agent who was handed a subtask.

    The task is the subtask, not the user's original message. Prior output stays
    so they can use what already happened.
    """
    parts: list[str] = []
    persona = spec.persona()
    if persona:
        parts.append(persona)

    others = "、".join(peer.display for peer in peers if peer.id != spec.id) or "（无）"
    parts.append(f"在场 agent（你之外）：{others}")

    if prior:
        blocks = ["以下是前面 agent 已经产出的内容："]
        for name, output in prior:
            blocks.append(f"--- {name} ---\n{_clip(output, limit)}")
        parts.append("\n\n".join(blocks))

    who = requester or "另一个 agent"
    parts.append(
        f"{who} 把一个子任务交给你。只做这件子任务，不要重答用户的原话，不要再写 handoff 或 plan。\n\n"
        f"子任务：\n{task}"
    )
    return "\n\n".join(parts)


def parse_plan(text: str) -> list[CollaborationTask]:
    """Every ``plan(owner, task, done, after)`` in a reply, in the order written."""
    found: list[CollaborationTask] = []
    for match in _PLAN_CALL.finditer(text):
        owner = match.group("owner").strip()
        description = match.group("task").strip()
        if not owner or not description:
            continue
        after = (match.group("after") or "").strip()
        found.append(
            CollaborationTask(
                description=description,
                expected_output=match.group("done").strip(),
                context=[after] if after else [],
                agent=owner,
            )
        )
    return found


def _depends_on(task: CollaborationTask, done: set[str]) -> bool:
    if not task.context:
        return True
    needle = task.context[0].strip()
    if not needle:
        return True
    return needle in done or needle.casefold() in {name.casefold() for name in done}


def _order_plan(
    items: list[tuple[AgentSpec, CollaborationTask]],
) -> list[tuple[AgentSpec, CollaborationTask]]:
    """Run a step only after the owner it names in ``context`` has a turn.

    A cycle or an unknown name does not drop the step: it runs after the ones
    that could be ordered, so the work is still visible.
    """
    pending = list(items)
    ordered: list[tuple[AgentSpec, CollaborationTask]] = []
    finished: set[str] = set()
    while pending:
        ready = [(spec, task) for spec, task in pending if _depends_on(task, finished)]
        if not ready:
            ordered.extend(pending)
            break
        ordered.extend(ready)
        for spec, _task in ready:
            finished.update({spec.id, spec.name, spec.display})
        pending = [(spec, task) for spec, task in pending if (spec, task) not in ready]
    return ordered


def apply_plan(
    steps: list[TurnStep],
    index: int,
    tasks: list[CollaborationTask],
    peers: list[AgentSpec],
    leader: AgentSpec,
    *,
    max_steps: int,
) -> list[tuple[AgentSpec, CollaborationTask]]:
    """Replace the remaining roster with the leader's steps, then a summary.

    Steps whose owner is not in the room are dropped. Returns the steps that
    were actually queued, in run order, without the summary.
    """
    resolved = [
        (spec, task)
        for task in tasks
        if (spec := resolve_handoff(task.agent, peers, "")) is not None
    ]
    if not resolved:
        return []
    ordered = _order_plan(resolved)
    room = max_steps - index - 2
    if room < 1:
        return []
    kept = ordered[:room]
    del steps[index + 1 :]
    for spec, task in kept:
        body = task.description
        if task.expected_output:
            body = f"{body}\n完成标准：{task.expected_output}"
        steps.append(TurnStep(spec=spec, kind="handoff", task=body, requester=leader.display))
    lines = ["根据下面各步的产出写汇总。不要重做这些子任务，不要再写 plan 或 handoff。"]
    for number, (spec, task) in enumerate(kept, 1):
        lines.append(f"{number}. 负责人 {spec.display}：{task.description}")
    lines.append("汇总里写明负责人和顺序，并对应上每一步的产出。")
    steps.append(TurnStep(spec=leader, kind="handoff", task="\n".join(lines), requester=leader.display))
    return kept
