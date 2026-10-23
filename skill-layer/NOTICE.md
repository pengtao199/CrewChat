# NOTICE

`crewchat-skill` 是 CrewChat 自有的 Rust 实现（MIT）。

## 参考实现

技能格式与安装/软链策略参考了以下项目；**本目录没有直接复制其源码**。
如果后续 vendor 具体文件，必须在这里记录 LICENSE 与源 commit。

| 项目 | 许可 | 参考内容 |
| --- | --- | --- |
| [Agent Skills 规范](https://agentskills.io) / [anthropics/skills](https://github.com/anthropics/skills) | Apache-2.0（部分 doc 技能 source-available） | `SKILL.md`、frontmatter、目录结构 |
| [vercel-labs/skills](https://github.com/vercel-labs/skills) | MIT | 安装源格式、symlink/copy、下载/解压限制、skills.sh 发现 |
| [yibie/skills-manager](https://github.com/yibie/skills-manager) | MIT | Library → Collection → Mount、稳定 ID、冲突/回滚思路 |
| [xingkongliang/skills-manager](https://github.com/xingkongliang/skills-manager) | MIT | 中央库、presets、per-agent 开关 |
| [wshobson/agents](https://github.com/wshobson/agents) | MIT | 多 harness 市场清单格式 |
| [anthropics/claude-plugins-community](https://github.com/anthropics/claude-plugins-community) | Apache-2.0 | `.claude-plugin/marketplace.json` 市场发现 |

## 第三方依赖

Rust crates 的许可见 `Cargo.lock`；主要依赖：`axum`、`tokio`、`rusqlite`、
`serde`、`sha2`、`walkdir`。均为 MIT / Apache-2.0 兼容许可。
