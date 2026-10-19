import Foundation

struct CoreHealth: Decodable {
    let status: String
    let version: String
}

actor CoreClient {
    private let endpoint: CoreEndpoint
    private let session: URLSession

    init(endpoint: CoreEndpoint) {
        self.endpoint = endpoint
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 60
        configuration.timeoutIntervalForResource = 60
        self.session = URLSession(configuration: configuration)
    }

    func health() async throws -> CoreHealth {
        var request = URLRequest(url: endpoint.url("health"))
        request.httpMethod = "POST"
        request.setValue("Bearer \(endpoint.token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = Data("{}".utf8)

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw CoreError(code: "invalid_response", message: "核心没有返回 HTTP 响应", detail: nil)
        }
        guard (200..<300).contains(http.statusCode) else {
            throw CoreError.decode(data, status: http.statusCode)
        }
        return try JSONDecoder().decode(CoreHealth.self, from: data)
    }
}
