# Vendored: Textual

| 项 | 值 |
| --- | --- |
| 上游 | https://github.com/gonzalezreal/textual |
| 版本 | 0.5.0（2026-06-15，`main` 快照） |
| 许可 | MIT（见同目录 `LICENSE`） |
| 引入方式 | 源码直接放入应用 target（不用 SPM），`Vendor/Textual/**.swift` 随文件系统同步组编译 |

## 引入原因

聊天消息需要块级 Markdown 渲染（标题、列表、代码块、表格、引用）。SwiftUI 自带的
`AttributedString(markdown:)` 只做行内样式，不排块级结构。

## 对上游源码做的改动

| 文件 | 改动 | 原因 |
| --- | --- | --- |
| `MathProperties.swift`、`Internal/StructuredText/MathBlock.swift`、`MathCodeBlock.swift`、`Internal/Attachment/MathAttachment.swift` | **删除** | 不需要数学公式渲染；避免再引入 7.5MB 的 `swiftui-math` 及其字体包 |
| `TextEnvironmentValues.swift`、`BlockContent.swift`、`AttributedString.swift`、`MarkdownParser/SyntaxExtension.swift`、`View+Textual.swift`、`Deprecations.swift` | 移除 math 相关引用 | 同上 |
| `Internal/Font/AnyFontProvider+Font.swift` | `LockIsolated` → 本地 `Locked` | 只用到 `LockIsolated` 一个类型，不为它引入 `swift-concurrency-extras` |
| `Internal/Helpers/Bundle.swift` | 资源查找改为 `Bundle.main` | SPM 的 `textual_Textual.bundle` 不存在；`prism-bundle.js` 直接放进 app 的 Resources |
| `Internal/Image/Image.swift` 等 6 个文件 | 内部 `Image` → `TextualImage` | 顶层 `Image` 会遮蔽 `SwiftUI.Image`，导致应用代码里的 `Image(systemName:)` 解析失败 |
| `Attachment/AttachmentLoader.swift` | `Textual.Attachment` → `AgentOS.Attachment` | 模块限定名随 target 变化 |
| `Internal/Attachment/WithAttachments.swift` | 显式 `as? any Attachment` | Swift 5 语言模式下存在类型擦除差异 |

资源：`Internal/Highlighter/Prism/prism-bundle.js`（Prism，MIT）随 target 复制到 `Contents/Resources`。

## 升级方式

重新拉上游 → 按上表重放改动 → 构建。升级后检查 `Contents/Resources/prism-bundle.js` 仍存在，
否则代码高亮会静默失效（`CodeTokenizer` 会 log "Prism JavaScript bundle is missing"）。
