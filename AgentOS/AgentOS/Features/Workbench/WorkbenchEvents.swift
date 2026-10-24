import Foundation

extension WorkbenchViewModel {
    func subscribe(conversationID: String) {
        if subscribedConversationID == conversationID, eventTask != nil {
            return
        }
        eventTask?.cancel()
        subscribedConversationID = conversationID
        eventTask = Task { [weak self] in
            guard let self else { return }
            for await frame in self.sessionClient.events(conversationID: conversationID) {
                self.handle(frame)
            }
        }
    }

    func handle(_ frame: CoreEventFrame) {
        guard !frame.isReplay else { return }
        waitingForTurn = false
        if let conversationID = frame.conversationID,
           let current = liveConversationID,
           conversationID != current {
            return
        }

        switch frame.event {
        case let .contentDelta(text):
            bufferStream(frame.speakerRoleID, text: text)
            return
        case let .thinking(text):
            bufferStream(frame.speakerRoleID, thinking: text)
            return
        default:
            flushStreamBuffers()
        }

        switch frame.event {
        case .contentDelta, .thinking:
            break
        case let .toolCall(id, title, status):
            updateLive(frame.speakerRoleID) { live in
                if let index = live.tools.firstIndex(where: { $0.id == id }) {
                    live.tools[index].title = title
                    live.tools[index].status = status
                    markToolFinishedIfNeeded(&live.tools[index])
                } else {
                    var tool = LiveTool(id: id, title: title, status: status, output: nil)
                    markToolFinishedIfNeeded(&tool)
                    live.tools.append(tool)
                }
            }
        case let .toolUpdate(id, title, status, output):
            updateLive(frame.speakerRoleID) { live in
                guard let index = live.tools.firstIndex(where: { $0.id == id }) else { return }
                if let title { live.tools[index].title = title }
                if let status { live.tools[index].status = status }
                if let output { live.tools[index].output = output }
                markToolFinishedIfNeeded(&live.tools[index])
            }
        case let .usage(usage):
            updateLive(frame.speakerRoleID) { live in
                live.usage = usage
            }
        case let .permissionRequest(request):
            pendingPermissions.removeAll { $0.id == request.id }
            pendingPermissions.append(
                PendingPermission(request: request, speakerRoleID: frame.speakerRoleID)
            )
            refreshRunningState()
        case .permissionAutoApproved:
            break
        case .delegationStarted, .delegationCompleted, .delegationCancelled:
            setRunning(true)
            refreshSoon()
        case .conversationCancelled:
            setRunning(false)
            pendingPermissions = []
            refreshSoon()
        case .turnComplete:
            completeLive(frame.speakerRoleID)
        case .detached:
            setRunning(false)
        case let .status(value):
            if value == "idle" || value == "ready" {
                refreshSoon()
            }
        case .other:
            break
        }
    }

    private func bufferStream(
        _ speakerRoleID: String?,
        text: String = "",
        thinking: String = ""
    ) {
        let key = speakerKey(speakerRoleID)
        var chunk = pendingStreamChunks[key]
            ?? PendingStreamChunk(speakerRoleID: speakerRoleID)
        chunk.text += text
        chunk.thinking += thinking
        pendingStreamChunks[key] = chunk

        guard streamFlushTask == nil else { return }
        streamFlushTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(50))
            guard let self, !Task.isCancelled else { return }
            self.flushStreamBuffers()
            self.streamFlushTask = nil
        }
    }

    private func flushStreamBuffers() {
        guard !pendingStreamChunks.isEmpty else { return }
        let chunks = pendingStreamChunks
        pendingStreamChunks.removeAll()
        for chunk in chunks.values {
            updateLive(chunk.speakerRoleID) { live in
                live.text += chunk.text
                live.thinking += chunk.thinking
            }
        }
    }

    private func updateLive(_ speakerRoleID: String?, mutate: (inout LiveTurn) -> Void) {
        let key = speakerKey(speakerRoleID)
        var live = liveReplies[key] ?? LiveTurn(
            id: key,
            speakerRoleID: speakerRoleID,
            text: "",
            thinking: "",
            tools: [],
            isRunning: true,
            usage: nil,
            startedAt: Date(),
            duration: nil
        )
        live.isRunning = true
        mutate(&live)
        liveReplies[key] = live
        setRunning(true)
    }

    private func completeLive(_ speakerRoleID: String?) {
        let key = speakerKey(speakerRoleID)
        guard var live = liveReplies[key] else {
            refreshSoon()
            return
        }
        live.isRunning = false
        live.duration = Date().timeIntervalSince(live.startedAt)
        liveReplies[key] = live
        pendingLiveRemoval[key] = live
        refreshSoon()
        refreshRunningState()
    }

    func clearPersistedLive() {
        guard let detail else { return }
        for (key, live) in pendingLiveRemoval {
            let persisted = detail.turns.contains { turn in
                guard turn.role == .assistant, turn.speakerRoleID == live.speakerRoleID else {
                    return false
                }
                return turn.plainText.trimmingCharacters(in: .whitespacesAndNewlines)
                    == live.text.trimmingCharacters(in: .whitespacesAndNewlines)
            }
            if persisted {
                liveReplies.removeValue(forKey: key)
                pendingLiveRemoval.removeValue(forKey: key)
            }
        }
    }

    func refreshSoon() {
        refreshTask?.cancel()
        refreshTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            guard let self, let conversationID = self.liveConversationID else { return }
            await self.open(conversationID)
        }
    }

    private func speakerKey(_ roleID: String?) -> String {
        roleID ?? "__default__"
    }

    private func markToolFinishedIfNeeded(_ tool: inout LiveTool) {
        let status = ToolActivityStatus.from(tool.status)
        if status == .completed || status == .failed {
            if tool.finishedAt == nil {
                tool.finishedAt = Date()
            }
        } else if status == .running || status == .pending {
            tool.finishedAt = nil
        }
    }
}
