"use client"

import { useSidebarContext } from "@/contexts/sidebar-context"
import { useAuxPanelContext } from "@/contexts/aux-panel-context"
import { useWorkspaceView } from "@/contexts/workspace-context"
import { useIsMobile } from "@/hooks/use-mobile"
import { usePlatform } from "@/hooks/use-platform"
import { useZoomLevel } from "@/hooks/use-appearance"
import { isDesktop } from "@/lib/platform"
import { ChromeReserve } from "@/components/layout/chrome-reserve"
import { leftChromeReserve, rightChromeReserve } from "@/lib/window-chrome"

/**
 * Corner reserve for a TOP-EDGE split-group strip. While split there is no
 * dedicated title-bar row above the shells (the workspace layout drops it
 * entirely instead of leaving a blank drag strip), so the strips along the
 * window's top edge must reserve the fixed corner overlays' width themselves —
 * exactly what the unsplit strip row does: left for LeftEdgeChrome while the
 * sidebar is collapsed (the conversation column then owns the window's left
 * edge), right for RightEdgeChrome while the column owns the right edge (aux
 * panel closed + conversation mode, or the column maximized over the file
 * column). Like that row, the reserve is mounted while its column holds the
 * corner — the right one only then; a mode switch or a maximize toggle snaps
 * the layout — and sized by the side panel over the corner, so a sidebar/aux
 * toggle slides it (ChromeReserve). Mobile shows the full-width FolderTitleBar
 * instead of corner overlays — no reserve. Self-subscribed so sidebar/aux/zoom
 * toggles re-render these slivers, not the whole panel.
 */
export function SplitStripCornerReserve({ side }: { side: "left" | "right" }) {
  const isMobile = useIsMobile()
  const { isOpen: sidebarOpen } = useSidebarContext()
  const { isOpen: auxOpen } = useAuxPanelContext()
  const { mode, conversationMaximized } = useWorkspaceView()
  const { isMac, isWindows, isLinux } = usePlatform()
  const { zoomLevel } = useZoomLevel()
  if (isMobile) return null
  const holdsRight = mode === "conversation" || conversationMaximized
  if (side === "right" && !holdsRight) return null
  const width =
    side === "left"
      ? sidebarOpen
        ? 0
        : leftChromeReserve(isMac && isDesktop(), zoomLevel)
      : auxOpen
        ? 0
        : rightChromeReserve(isDesktop() && (isWindows || isLinux), zoomLevel)
  return <ChromeReserve width={width} />
}
