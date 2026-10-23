# mcp-layer vendored gateway

The runtime gateway under `vendor/gateway/` is a pinned copy of
MCPJungle (https://github.com/mcpjungle/MCPJungle), commit
`12648be5edc70391dc3f9c4aecfd37b83b3e30be`, tag `0.4.6`.

- Upstream license: Mozilla Public License 2.0. The upstream `LICENSE` file is
  kept in `vendor/gateway/LICENSE`.
- This repository vendors the source only; it is built as an independent
  sidecar binary named `crewchat-mcp-gateway`.
- No upstream Go source files were modified. `internal/dashboardui/dist/index.html`
  is a local placeholder required by the upstream `go:embed` directive; the
  dashboard is not exposed by mcp-layer.
- Do not expose the upstream product name in AgentOS/mcp-layer user-facing
  strings. User-facing API and UI call this runtime **mcp-layer**.
- If upstream Go files are modified later, those files remain under MPL-2.0 and
  the modifications must be made available with the distributed source.
