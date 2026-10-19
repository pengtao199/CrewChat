import SwiftUI

struct ConnectionView: View {
    let model: ConnectionViewModel

    var body: some View {
        Form {
            Section("connection.section") {
                LabeledContent("connection.status") {
                    Label(model.statusKey, systemImage: model.statusSymbol)
                        .foregroundStyle(model.statusColor)
                }

                if let endpoint = model.phase.endpoint {
                    LabeledContent("connection.address") {
                        Text("127.0.0.1:\(endpoint.port)")
                            .monospacedDigit()
                    }
                }

                if case let .connected(_, version) = model.phase {
                    LabeledContent("connection.version") {
                        Text(version)
                    }
                }
            }

            Section {
                HStack(spacing: 12) {
                    if model.canStart {
                        Button("connection.start") {
                            Task { await model.start() }
                        }
                        .buttonStyle(.borderedProminent)
                    }

                    if model.isConnected || model.phase.isBusy {
                        Button("connection.retry") {
                            Task { await model.retry() }
                        }
                        .disabled(model.phase.isBusy)
                    }

                    Spacer()

                    if model.isStarting {
                        ProgressView()
                            .controlSize(.small)
                    }
                }
            } footer: {
                Group {
                    if let message = model.failureMessage {
                        Text(message)
                    } else {
                        Text(model.detailKey)
                    }
                }
                .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .navigationTitle("app.name")
        .task {
            if model.phase == .idle {
                await model.start()
            }
        }
    }
}
