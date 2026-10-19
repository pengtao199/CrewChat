import Foundation

struct CoreRuntimeState: Codable, Sendable {
    let port: Int

    private static func file(_ directory: URL) -> URL {
        directory.appending(path: "core.json")
    }

    static func load(directory: URL) -> CoreRuntimeState? {
        guard let data = try? Data(contentsOf: file(directory)) else { return nil }
        return try? JSONDecoder().decode(CoreRuntimeState.self, from: data)
    }

    static func save(port: Int, directory: URL) {
        let state = CoreRuntimeState(port: port)
        guard let data = try? JSONEncoder().encode(state) else { return }
        try? data.write(to: file(directory), options: [.atomic])
    }

    static func clear(directory: URL) {
        try? FileManager.default.removeItem(at: file(directory))
    }
}
