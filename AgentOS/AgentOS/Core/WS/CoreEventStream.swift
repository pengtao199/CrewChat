import Foundation

final class CoreEventStream: @unchecked Sendable {
    private let endpoint: CoreEndpoint

    init(endpoint: CoreEndpoint) {
        self.endpoint = endpoint
    }

    func events(conversationID: String, sinceSeq: UInt64 = 0) -> AsyncStream<CoreEventFrame> {
        AsyncStream { continuation in
            var request = URLRequest(url: endpoint.eventsURL)
            request.setValue("Bearer \(endpoint.token)", forHTTPHeaderField: "Authorization")
            request.setValue("crewchat-events", forHTTPHeaderField: "Sec-WebSocket-Protocol")

            let session = URLSession(configuration: .default)
            let task = session.webSocketTask(with: request)
            task.resume()

            let attach: [String: Any] = [
                "action": "attach",
                "subscription_id": UUID().uuidString,
                "conversation_id": conversationID,
                "since_seq": sinceSeq,
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
                        for frame in CoreEventParser.parse(text) {
                            continuation.yield(frame)
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
