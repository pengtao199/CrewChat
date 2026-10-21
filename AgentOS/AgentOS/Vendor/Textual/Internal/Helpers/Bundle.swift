import Foundation

extension Bundle {
  // Vendored: Textual's SPM resource bundle name does not exist here, so the
  // Prism script is copied into the app bundle's Resources root.
  static let textual: Bundle? = {
    #if DEBUG
      if let override = ProcessInfo.processInfo.environment["PACKAGE_RESOURCE_BUNDLE_PATH"]
        ?? ProcessInfo.processInfo.environment["PACKAGE_RESOURCE_BUNDLE_URL"],
        let bundle = Bundle(url: URL(fileURLWithPath: override))
      {
        return bundle
      }
    #endif

    return .main
  }()
}
