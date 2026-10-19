import SwiftUI

@MainActor
@Observable
final class CoreSettingsViewModel {
    private(set) var delegationEnabled = true
    private(set) var delegationLoaded = false
    private(set) var isLoading = false
    private(set) var isSaving = false
    private(set) var isRestarting = false
    private(set) var errorMessage: String?

    private let settings: CoreSettings
    private let lifecycle: CoreLifecycle

    init(settings: CoreSettings, lifecycle: CoreLifecycle) {
        self.settings = settings
        self.lifecycle = lifecycle
    }

    func load() async {
        guard !isLoading else { return }
        isLoading = true
        errorMessage = nil

        do {
            let current = try await settings.delegation()
            delegationEnabled = current.enabled
            delegationLoaded = true
        } catch {
            delegationLoaded = false
            errorMessage = error.localizedDescription
        }

        isLoading = false
    }

    func applyDefaultsIfNeeded() async {
        guard !Preferences.delegationInitialized else { return }
        guard let current = try? await settings.delegation() else { return }

        if !current.enabled, let saved = try? await settings.setDelegationEnabled(true) {
            delegationEnabled = saved.enabled
        }
        Preferences.delegationInitialized = true
    }

    func setDelegation(_ enabled: Bool) async {
        guard !isSaving else { return }
        isSaving = true
        errorMessage = nil

        do {
            let saved = try await settings.setDelegationEnabled(enabled)
            delegationEnabled = saved.enabled
            Preferences.delegationInitialized = true
        } catch {
            errorMessage = error.localizedDescription
            if let current = try? await settings.delegation() {
                delegationEnabled = current.enabled
            }
        }

        isSaving = false
    }

    func restartCore() async {
        guard !isRestarting else { return }
        isRestarting = true
        errorMessage = nil

        await lifecycle.stop()
        await lifecycle.start()
        await load()

        isRestarting = false
    }
}
