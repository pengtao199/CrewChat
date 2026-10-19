import Foundation

struct TokenStore {
    let url: URL

    init(directory: URL) {
        url = directory.appending(path: "token")
    }

    func load() -> String? {
        guard let data = try? Data(contentsOf: url),
              let raw = String(data: data, encoding: .utf8) else {
            return nil
        }
        let token = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        return token.isEmpty ? nil : token
    }

    func save(_ token: String) throws {
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try Data(token.utf8).write(to: url, options: [.atomic])
        _ = try? FileManager.default.setAttributes(
            [.posixPermissions: 0o600],
            ofItemAtPath: url.path
        )
    }
}
