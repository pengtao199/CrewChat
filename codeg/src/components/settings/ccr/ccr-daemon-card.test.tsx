import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import { CcrDaemonCard } from "./ccr-daemon-card"

const call = vi.fn()

vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ call }),
  isDesktop: () => false,
  isRemoteDesktopMode: () => false,
  getActiveRemoteConnectionId: () => null,
}))

const running = {
  enabled: true,
  webUrl: "http://127.0.0.1:3458",
  tokenConfigured: true,
  bin: "/opt/ccr/bin/ccr",
  state: "running" as const,
  managed: false,
  pid: 4242,
  lastCheckMs: 1,
  consecutiveFailures: 0,
  lastError: null,
  blockedReason: null,
}

const stopped = {
  ...running,
  state: "stopped" as const,
  managed: false,
  pid: null,
}

function renderCard() {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <CcrDaemonCard />
    </NextIntlClientProvider>
  )
}

describe("CcrDaemonCard", () => {
  beforeEach(() => {
    call.mockReset()
  })

  it("adopts a running instance: reports the state and offers no start", async () => {
    call.mockResolvedValue(running)
    renderCard()

    await waitFor(() => {
      expect(screen.getByText("Running")).toBeTruthy()
    })
    expect(call).toHaveBeenCalledWith("ccr_supervisor_state")
    // Who started it is visible, and Codeg offers nothing to click that would
    // touch a healthy instance.
    expect(screen.getByText("Started outside Codeg")).toBeTruthy()
    expect(screen.getByText("4242")).toBeTruthy()
    expect(screen.queryByText("Start CCR")).toBeNull()
  })

  it("says which instance Codeg started itself", async () => {
    call.mockResolvedValue({ ...running, managed: true })
    renderCard()

    await waitFor(() => {
      expect(screen.getByText("Started by Codeg")).toBeTruthy()
    })
  })

  it("starts a stopped management process and shows the new state", async () => {
    call.mockResolvedValueOnce(stopped).mockResolvedValueOnce(running)
    renderCard()

    const start = await screen.findByText("Start CCR")
    fireEvent.click(start)

    await waitFor(() => {
      expect(call).toHaveBeenCalledWith("ccr_supervisor_start")
    })
    await waitFor(() => {
      expect(screen.getByText("Running")).toBeTruthy()
    })
  })

  it("retries after a failed start", async () => {
    call
      .mockResolvedValueOnce({
        ...stopped,
        state: "failed",
        lastError: "`ccr start` exited 1: port in use",
      })
      .mockResolvedValueOnce(running)
    renderCard()

    // The reason a retry is worth offering is on screen.
    expect(
      await screen.findByText(/port in use/, { exact: false })
    ).toBeTruthy()
    const retry = await screen.findByText("Retry")
    fireEvent.click(retry)
    await waitFor(() => {
      expect(call).toHaveBeenCalledWith("ccr_supervisor_start")
    })
  })

  it("explains why it cannot start one at all", async () => {
    call.mockResolvedValue({
      ...stopped,
      bin: null,
      tokenConfigured: false,
      blockedReason: "CCR_WEB_AUTH_TOKEN is not set",
    })
    renderCard()

    // Standing reason shown, and no button that would fail the same way.
    expect(
      await screen.findByText(/CCR_WEB_AUTH_TOKEN is not set/, { exact: false })
    ).toBeTruthy()
    expect(screen.queryByText("Start CCR")).toBeNull()
  })
})
