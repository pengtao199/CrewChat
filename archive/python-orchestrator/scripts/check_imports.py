"""导入可解析性检查：所有相对导入必须指向真实模块。

`archive/` 下的文件是历史存档，不参与运行，跳过检查。

用法:
    python3 scripts/check_imports.py
"""

import ast
import pathlib
import sys

DEFERRED = {"archive"}


def deferred(p: pathlib.Path) -> bool:
    return bool(DEFERRED.intersection(p.parts)) or "__pycache__" in p.parts


root = pathlib.Path("src/crewchat")
mods = set()
for p in root.rglob("*.py"):
    if deferred(p): continue
    rel = p.relative_to(root).with_suffix("")
    parts = list(rel.parts)
    if parts[-1] == "__init__": parts = parts[:-1]
    if parts: mods.add(".".join(parts))

bad = []
for p in root.rglob("*.py"):
    if deferred(p): continue
    rel = p.relative_to(root).with_suffix("")
    parts = list(rel.parts)
    pkg = parts[:-1]
    name = ".".join(parts)
    tree = ast.parse(p.read_text())
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.level:
            base = pkg[: len(pkg) - (node.level - 1)] if node.level > 1 else pkg
            target = ".".join(base + (node.module.split(".") if node.module else []))
            if target not in mods:
                # 可能是子模块（from ..core import x）
                bad.append(f"{name}:{node.lineno}  from {'.'*node.level}{node.module or ''}  → {target} 不存在")

for b in bad:
    print("  ✗", b)
if bad:
    print(f"\n失效导入: {len(bad)}")
    sys.exit(1)
print(f"✓ 全部 {len(mods)} 个模块的导入可解析")
