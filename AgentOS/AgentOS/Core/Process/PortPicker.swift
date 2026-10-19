import Darwin
import Foundation

enum PortPicker {
    static func isFree(_ port: Int) -> Bool {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { close(fd) }

        var address = sockaddr_in()
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = UInt16(port).bigEndian
        address.sin_addr.s_addr = inet_addr("127.0.0.1")

        var reuse: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))

        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        return bound == 0
    }

    static func freePort() -> Int {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return 3080 }
        defer { close(fd) }

        var address = sockaddr_in()
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = 0
        address.sin_addr.s_addr = inet_addr("127.0.0.1")

        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0 else { return 3080 }

        var result = sockaddr_in()
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        let named = withUnsafeMutablePointer(to: &result) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                getsockname(fd, $0, &length)
            }
        }
        guard named == 0 else { return 3080 }
        return Int(UInt16(bigEndian: result.sin_port))
    }
}
