import Foundation

struct TokenUsage: Decodable, Sendable, Equatable {
    let input: Int?
    let output: Int?
    let total: Int?

    init(input: Int? = nil, output: Int? = nil, total: Int? = nil) {
        self.input = input
        self.output = output
        self.total = total
    }

    enum CodingKeys: String, CodingKey {
        case inputTokens
        case inputTokensSnake = "input_tokens"
        case outputTokens
        case outputTokensSnake = "output_tokens"
        case totalTokens
        case totalTokensSnake = "total_tokens"
        case used
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        input = try container.decodeIfPresent(Int.self, forKey: .inputTokens)
            ?? container.decodeIfPresent(Int.self, forKey: .inputTokensSnake)
        output = try container.decodeIfPresent(Int.self, forKey: .outputTokens)
            ?? container.decodeIfPresent(Int.self, forKey: .outputTokensSnake)
        total = try container.decodeIfPresent(Int.self, forKey: .totalTokens)
            ?? container.decodeIfPresent(Int.self, forKey: .totalTokensSnake)
            ?? container.decodeIfPresent(Int.self, forKey: .used)
    }

    var displayTotal: Int? {
        total ?? {
            guard input != nil || output != nil else { return nil }
            return (input ?? 0) + (output ?? 0)
        }()
    }
}
