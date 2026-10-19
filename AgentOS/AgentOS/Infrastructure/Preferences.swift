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

    static var autoStartCore: Bool {
        get { defaults.object(forKey: "core.autoStart") as? Bool ?? true }
        set { defaults.set(newValue, forKey: "core.autoStart") }
    }

    static var delegationInitialized: Bool {
        get { defaults.bool(forKey: "core.delegationInitialized") }
        set { defaults.set(newValue, forKey: "core.delegationInitialized") }
    }
}
