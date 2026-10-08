#!/usr/bin/env python3
"""分层契约检查：验证 src/crewchat 的依赖方向。

契约见 AGENTS/architecture.md。违反时以非零码退出，便于在提交前与 CI 中执行。

用法:
    uv run python scripts/check_layers.py
"""

from __future__ import annotations

import ast
import collections
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1] / "src" / "crewchat"

LAYERS: dict[str, int] = {
    "L0-base": 0,
    "L2-executors": 2,
    "L3-orchestration": 3,
    "L4-api": 4,
}

"""层由顶层目录决定 —— 目录结构就是契约本身。"""
PACKAGE_LAYER: dict[str, str] = {
    "core": "L0-base",
    "executors": "L2-executors",
    "orchestration": "L3-orchestration",
    "api": "L4-api",
}

"""同层内允许的依赖方向（左可依赖右）。未登记的同层边不判违规，仅提示。"""
SAME_LAYER_ALLOWED: set[tuple[str, str]] = set()


def module_name(path: pathlib.Path) -> str:
    rel = path.relative_to(ROOT).with_suffix("")
    parts = list(rel.parts)
    if parts[-1] == "__init__":
        parts = parts[:-1]
    return ".".join(parts)


def discover() -> dict[str, pathlib.Path]:
    found: dict[str, pathlib.Path] = {}
    for path in sorted(ROOT.rglob("*.py")):
        if "__pycache__" in path.parts:
            continue
        name = module_name(path)
        if name:
            found[name] = path
    return found


def resolve(name: str, path: pathlib.Path, node: ast.ImportFrom, known: set[str]) -> str | None:
    if not node.level:
        return None
    parts = name.split(".") if name else []
    if path.name != "__init__.py":
        parts = parts[:-1]
    if node.level > 1:
        parts = parts[: max(0, len(parts) - (node.level - 1))]
    target = ".".join(parts + (node.module.split(".") if node.module else []))
    for candidate in sorted(known, key=len, reverse=True):
        if candidate == target or target.startswith(candidate + "."):
            return candidate
    return None


def layer_of(module: str) -> str | None:
    top = module.split(".")[0]
    return PACKAGE_LAYER.get(top)


def main() -> int:
    modules = discover()
    known = set(modules)
    edges: dict[str, set[str]] = collections.defaultdict(set)

    for name, path in modules.items():
        tree = ast.parse(path.read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            if isinstance(node, ast.ImportFrom):
                target = resolve(name, path, node, known)
                if target and target != name:
                    edges[name].add(target)

    problems: list[str] = []
    unregistered: set[str] = set()

    for src in sorted(edges):
        src_layer = layer_of(src)
        if src_layer is None and src != "":
            unregistered.add(src)
            continue
        for dst in sorted(edges[src]):
            dst_layer = layer_of(dst)
            if dst_layer is None:
                unregistered.add(dst)
                continue
            if src_layer == dst_layer:
                if (src, dst) in SAME_LAYER_ALLOWED:
                    continue
                continue
            if LAYERS[src_layer] < LAYERS[dst_layer]:
                problems.append(f"向上依赖  {src} ({src_layer}) → {dst} ({dst_layer})")

    print(f"模块 {len(modules)} · 依赖边 {sum(len(v) for v in edges.values())}")

    if unregistered:
        print("\n未归入任何层的模块（顶层目录必须是 core / executors / orchestration / api）:")
        for name in sorted(unregistered):
            print(f"  - {name}")

    if problems:
        print(f"\n违反分层契约 {len(problems)} 处:")
        for line in problems:
            print(f"  ✗ {line}")
        return 1

    if unregistered:
        return 1

    print("✓ 分层契约通过：无向上依赖，所有模块已登记")
    return 0


if __name__ == "__main__":
    sys.exit(main())
