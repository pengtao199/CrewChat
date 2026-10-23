import Foundation

enum Preferences {
    private static var defaults: UserDefaults { .standard }

    static var autoStartCore: Bool {
        get { defaults.object(forKey: "core.autoStart") as? Bool ?? true }
        set { defaults.set(newValue, forKey: "core.autoStart") }
    }

    static var delegationInitialized: Bool {
        get { defaults.bool(forKey: "core.delegationInitialized") }
        set { defaults.set(newValue, forKey: "core.delegationInitialized") }
    }
}
