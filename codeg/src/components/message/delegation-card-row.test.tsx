import { type ReactElement } from "react"
import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it } from "vitest"

import { DelegationCardRow } from "./delegation-card-row"
import enMessages from "@/i18n/messages/en.json"

function renderWithIntl(ui: ReactElement) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      {ui}
    </NextIntlClientProvider>
  )
}

// Phase 4.1.1: the row leads with the MEMBER's roster name when the delegation
// resolved one, and keeps the CLI label as the secondary marker; without a
// member it renders exactly what it always did. (`AgentIcon` renders the CLI
// name in its own <title> too, so CLI-text queries are `getAllBy*`.)
describe("DelegationCardRow member label", () => {
  it("leads with the member name and keeps the CLI as a secondary label", () => {
    renderWithIntl(
      <DelegationCardRow
        agentType="claude_code"
        memberName="后端工程师"
        taskId="77f43264-aaaa"
        status="running"
        task="只读验证任务"
      />
    )

    expect(screen.getByText("后端工程师")).toBeTruthy()
    // The CLI survives as a secondary marker rather than disappearing.
    expect(screen.getAllByText("Claude Code").length).toBeGreaterThan(1)
  })

  it("falls back to the CLI label alone when no member was resolved", () => {
    renderWithIntl(
      <DelegationCardRow
        agentType="claude_code"
        memberName={null}
        taskId={null}
        status="ok"
        task={null}
      />
    )

    expect(screen.getAllByText("Claude Code").length).toBeGreaterThan(0)
  })
})
