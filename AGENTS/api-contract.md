# UI 契约

## 适用范围

新增或修改 `/api/v1/*` 路由、WebSocket 事件、消息流式帧，或前端报 404 / 501 时。

## 目标

后端与 fork 前端之间的契约显式、可追踪：未实现要能被发现，已实现的形状必须匹配。

## 版本根

**所有 HTTP 请求必须带版本段 `/api/v1`。**

- 版本号单一真相源是 `core/version.py` 的 `api_prefix()`，其他文件禁止硬编码 `v1`。
- 路由仍注册在未版本化的 `/api` 下，`ApiVersionMiddleware`（`api/app.py`）在 ASGI
  边界强制：带 `/api/v1` 的路径去掉版本段后交给路由；不带版本的 `/api/*` 与已迁入
  版本根的旧入口（`/login`、`/logout`）返回 **404** `{"error": "api_version_required",
  "path": ..., "expected_prefix": "/api/v1"}`，不做兼容转发。
- 新增路由不需要改路径：注册在 `/api/...` 即可自动随版本根暴露。
- `/ws`（WebSocket）、`/assets`（静态资源）、前端页面路由不在版本管辖内。
- 前端把 `/api/...` 变成 `/api/v1/...` 的动作只能在传输层做一次，不要逐个调用点加前缀。

## 契约来源

上游前端通过 `ui/packages/desktop/src/common/adapter/ipcBridge.ts` 定义全部调用，这是**唯一权威**：

| 类别 | 数量 | 位置 |
| --- | --- | --- |
| REST 端点 | 112 | `ipcBridge.ts` 里的 `'/api/...'` 字符串 |
| WS 事件 | 43 | `wsEmitter('<name>')` |
| 鉴权 | 4 | `/api/v1/auth/user`、`/api/v1/login`、`/api/v1/logout`、`/api/v1/auth/refresh` |

`ipcBridge.ts` 里的字符串保持上游写法（不带版本段），后端挂载时统一加 `/api/v1`。

新增后端接口前，先在 `ipcBridge.ts` 里搜到对应定义，按其类型形状实现。

## 原则

- **形状跟随上游**：字段名、包装层（`{items, total, has_more}`）、时间戳单位（毫秒）都照抄，不自创。
- **未实现要显式**：内部 `/api/{path}` 兜底返回 **501** 并带 `{"error": "not_implemented", "path": ...}`（`path` 报的是调用方请求的带版本路径），让前端日志能定位；不要返回 404 或空对象。
- **状态码有意义**：缺版本段 404 `api_version_required`、未登录 401、不存在 404、未实现 501、参数错 400。
- **流式用累积 + replace**：文本增量帧发送完整累积文本并置 `replace: true`，否则前端会把每段追加成新行。
- **终止帧类型固定**：一轮结束发送 `type: "finish"`，这是前端判定回合结束的类型。

## 具体规则

- 新增路由后，在 `AGENTS/` 对应的规则文件或提交信息里登记它属于哪个上游契约。
- 属于我们自研的功能用独立命名空间 `/api/crewchat/*`，不污染上游路径。
- WS 帧统一走 `api/hub.py` 的信封 `{"name": <event>, "data": <payload>}`；事件名必须来自 `ipcBridge.ts`，不自造通道。
- 会话发送的返回必须是 `{msg_id, turn_id, runtime}`；`runtime` 的 `can_send_message` / `is_processing` 决定界面能否再发。
- 修改上游已有的响应形状时，同时更新 `ui/` 里对应的 mapper（如 `apiModelMapper.ts`），否则前端静默降级。

## 现有实现覆盖

| 域 | 状态 |
| --- | --- |
| 鉴权、客户端偏好、WS 连接 | 已实现 |
| 会话列表/详情/创建/删除、消息历史 | 已实现 |
| 会话发送/取消、流式帧 | 已实现 |
| 大脑池、Assistants（投影自 Agent）、技能、MCP、CLI 探测 | 已实现 |
| 用量（自研 `/api/v1/crewchat/usage`，按身体/模型/天聚合） | 已实现 |
| 项目=工作区（自研 `/api/v1/crewchat/projects` CRUD） | 已实现 |
| 文件/目录浏览（上游 `/api/v1/fs/*`：dir/list/read/write/metadata/image-base64） | 已实现（其余 fs 端点未实现） |
| 团队（多 Agent）、文件浏览、扩展、定时任务、远程连接 | 未实现（501） |

## 禁止项

- 不为了让界面能跑而返回假数据；宁可是显式的 501。
- 不在前端加"兼容分支"去迁就后端形状；改后端。
- 不用自造的事件名（例如 `crewchat.update`），除非同时在前端注册监听。

## 检查

- 这个路径/事件名能在 `ipcBridge.ts` 里找到吗？
- 响应形状与上游类型定义逐字段对得上吗（含单位与包装）？
- 失败时前端日志能否指出是哪个路径未实现？
- 流式文本是否带 `replace` 且以 `finish` 收尾？
