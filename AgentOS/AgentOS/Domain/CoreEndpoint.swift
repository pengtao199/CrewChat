import Foundation

struct CoreEndpoint: Sendable, Equatable {
    let host: String
    let port: Int
    let token: String

    var baseURL: URL {
        URL(string: "http://\(host):\(port)")!
    }

    func url(_ command: String) -> URL {
        baseURL.appending(path: "api").appending(path: command)
    }

    var eventsURL: URL {
        URL(string: "ws://\(host):\(port)/ws/events")!
    }
}
