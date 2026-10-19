import Foundation

struct CoreClientProvider: Sendable {
    let endpoint: @Sendable () -> CoreEndpoint?

    func client() throws -> CoreClient {
        guard let endpoint = endpoint() else {
            throw CoreError(code: "core_not_ready", message: "核心尚未连接，先启动核心。", detail: nil)
        }
        return CoreClient(endpoint: endpoint)
    }
}
