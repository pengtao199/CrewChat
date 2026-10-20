import Foundation

enum CoreEvent: Sendable, Equatable {
    case contentDelta(String)
    case thinking(String)
    case toolCall(id: String, title: String, status: String)
    case toolUpdate(id: String, title: String?, status: String?, output: String?)
    case turnComplete
    case detached
    case status(String)
    case other(String)
}

enum CoreEventParser {
    static func parse(_ text: String) -> [CoreEvent] {
        guard let data = text.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return []
        }

        switch root["type"] as? String {
        case "event":
            guard let envelope = root["envelope"] as? [String: Any] else { return [] }
            return event(envelope)
        case "replay":
            let events = root["events"] as? [[String: Any]] ?? []
            return events.flatMap(event)
        case "detached":
            return [.detached]
        default:
            return []
        }
    }

    private static func event(_ payload: [String: Any]) -> [CoreEvent] {
        switch payload["type"] as? String {
        case "content_delta":
            return [.contentDelta(payload["text"] as? String ?? "")]
        case "thinking":
            return [.thinking(payload["text"] as? String ?? "")]
        case "tool_call":
            return [
                .toolCall(
                    id: payload["tool_call_id"] as? String ?? UUID().uuidString,
                    title: payload["title"] as? String ?? "tool",
                    status: payload["status"] as? String ?? ""
                )
            ]
        case "tool_call_update":
            return [
                .toolUpdate(
                    id: payload["tool_call_id"] as? String ?? "",
                    title: payload["title"] as? String,
                    status: payload["status"] as? String,
                    output: payload["raw_output"] as? String
                )
            ]
        case "turn_complete":
            return [.turnComplete]
        case "status_changed":
            return [.status(payload["status"] as? String ?? "")]
        case let type?:
            return [.other(type)]
        default:
            return []
        }
    }
}

final class CoreEventStream: @unchecked Sendable {
    private let endpoint: CoreEndpoint

    init(endpoint: CoreEndpoint) {
        self.endpoint = endpoint
    }

    func events(connectionID: String) -> AsyncStream<CoreEvent> {
        AsyncStream { continuation in
            var request = URLRequest(url: endpoint.eventsURL)
            request.setValue("Bearer \(endpoint.token)", forHTTPHeaderField: "Authorization")
            request.setValue("codeg-events", forHTTPHeaderField: "Sec-WebSocket-Protocol")

            let session = URLSession(configuration: .default)
            let task = session.webSocketTask(with: request)
            task.resume()

            let subscriptionID = UUID().uuidString
            let attach: [String: Any] = [
                "action": "attach",
                "subscription_id": subscriptionID,
                "connection_id": connectionID,
            ]
            if let data = try? JSONSerialization.data(withJSONObject: attach),
               let text = String(data: data, encoding: .utf8) {
                task.send(.string(text)) { _ in }
            }

            let receiver = Task {
                while !Task.isCancelled {
                    do {
                        let message = try await task.receive()
                        guard case let .string(text) = message else { continue }
                        for event in CoreEventParser.parse(text) {
                            continuation.yield(event)
                        }
                    } catch {
                        break
                    }
                }
                continuation.finish()
            }

            continuation.onTermination = { _ in
                receiver.cancel()
                task.cancel(with: .goingAway, reason: nil)
                session.invalidateAndCancel()
            }
        }
    }
}
