import Foundation

struct CoreError: Error, LocalizedError {
    let code: String
    let message: String
    let detail: String?

    var errorDescription: String? { message }

    static func decode(_ data: Data, status: Int) -> CoreError {
        struct Body: Decodable {
            let code: String?
            let message: String?
            let detail: String?
        }
        let body = try? JSONDecoder().decode(Body.self, from: data)
        return CoreError(
            code: body?.code ?? "http_\(status)",
            message: body?.message ?? "HTTP \(status)",
            detail: body?.detail
        )
    }
}
