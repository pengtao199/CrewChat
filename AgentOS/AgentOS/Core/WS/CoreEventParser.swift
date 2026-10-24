import Foundation

enum CoreEventParser {
    static func parse(_ text: String) -> [CoreEventFrame] {
        guard let data = text.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return []
        }

        switch root["type"] as? String {
        case "event":
            guard let envelope = root["envelope"] as? [String: Any] else { return [] }
            return frame(envelope, isReplay: false).map { [$0] } ?? []
        case "replay":
            let events = root["events"] as? [[String: Any]] ?? []
            return events.compactMap { frame($0, isReplay: true) }
        case "detached":
            return [CoreEventFrame(event: .detached, conversationID: nil, speakerRoleID: nil, seq: 0, isReplay: false)]
        default:
            return []
        }
    }

    private static func frame(_ payload: [String: Any], isReplay: Bool) -> CoreEventFrame? {
        guard let event = event(payload) else { return nil }
        return CoreEventFrame(
            event: event,
            conversationID: payload["conversation_id"] as? String,
            speakerRoleID: payload["speaker_role_id"] as? String,
            seq: UInt64(payload["seq"] as? Int ?? 0),
            isReplay: isReplay
        )
    }

    private static func event(_ payload: [String: Any]) -> CoreEvent? {
        switch payload["type"] as? String {
        case "content_delta":
            return .contentDelta(payload["text"] as? String ?? "")
        case "thinking":
            return .thinking(payload["text"] as? String ?? "")
        case "tool_call":
            return .toolCall(
                id: payload["tool_call_id"] as? String ?? UUID().uuidString,
                title: payload["title"] as? String ?? "tool",
                status: payload["status"] as? String ?? ""
            )
        case "tool_call_update":
            return .toolUpdate(
                id: payload["tool_call_id"] as? String ?? "",
                title: payload["title"] as? String,
                status: payload["status"] as? String,
                output: payload["raw_output"] as? String
            )
        case "usage":
            return .usage(usage(payload["usage"]))
        case "permission_request":
            return .permissionRequest(permission(payload))
        case "permission_auto_approved":
            return .permissionAutoApproved(
                requestID: payload["request_id"] as? String ?? "",
                optionName: payload["option_name"] as? String
            )
        case "delegation_started":
            return .delegationStarted(
                id: payload["delegation_id"] as? String ?? "",
                memberRoleID: payload["member_role_id"] as? String,
                task: payload["task"] as? String ?? ""
            )
        case "delegation_completed":
            return .delegationCompleted(
                id: payload["delegation_id"] as? String ?? "",
                memberRoleID: payload["member_role_id"] as? String,
                status: payload["status"] as? String ?? "",
                summary: payload["summary"] as? String,
                error: payload["error"] as? String
            )
        case "delegation_cancelled":
            return .delegationCancelled(
                id: payload["delegation_id"] as? String ?? "",
                memberRoleID: payload["member_role_id"] as? String
            )
        case "conversation_cancelled":
            return .conversationCancelled(
                cancelledDelegations: payload["cancelled_delegations"] as? Int ?? 0
            )
        case "member_added":
            return .memberAdded(roleID: payload["role_id"] as? String ?? "")
        case "turn_complete":
            return .turnComplete
        case "status_changed":
            return .status(payload["status"] as? String ?? "")
        case let type?:
            return .other(type)
        default:
            return nil
        }
    }

    private static func usage(_ value: Any?) -> TokenUsage {
        let object = value as? [String: Any] ?? [:]
        return TokenUsage(
            input: int(object, ["inputTokens", "input_tokens"]),
            output: int(object, ["outputTokens", "output_tokens"]),
            total: int(object, ["totalTokens", "total_tokens"])
        )
    }

    private static func permission(_ payload: [String: Any]) -> CorePermissionRequest {
        let request = payload["request"] as? [String: Any] ?? [:]
        let tool = request["toolCall"] as? [String: Any]
            ?? request["tool_call"] as? [String: Any]
        let title = tool?["title"] as? String
            ?? request["title"] as? String
            ?? "Permission request"
        let options = (request["options"] as? [[String: Any]] ?? []).compactMap {
            option -> CorePermissionOption? in
            guard let id = option["optionId"] as? String
                ?? option["option_id"] as? String
                ?? option["id"] as? String
            else {
                return nil
            }
            return CorePermissionOption(
                id: id,
                name: option["name"] as? String ?? id,
                kind: option["kind"] as? String ?? ""
            )
        }
        return CorePermissionRequest(
            id: payload["request_id"] as? String ?? "",
            title: title,
            detail: tool?["content"] as? String,
            options: options
        )
    }

    private static func int(_ object: [String: Any], _ keys: [String]) -> Int? {
        for key in keys {
            if let value = object[key] as? Int { return value }
            if let value = object[key] as? Double { return Int(value) }
            if let value = object[key] as? String, let number = Int(value) { return number }
        }
        return nil
    }
}
