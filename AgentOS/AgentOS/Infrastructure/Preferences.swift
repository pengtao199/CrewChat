import Foundation

enum Preferences {
    private static var defaults: UserDefaults { .standard }

    static var coreBinaryPath: String? {
        get { defaults.string(forKey: "core.binaryPath") }
        set { defaults.set(newValue, forKey: "core.binaryPath") }
    }

    static var coreDataDirectory: String? {
        get { defaults.string(forKey: "core.dataDirectory") }
        set { defaults.set(newValue, forKey: "core.dataDirectory") }
    }
}
