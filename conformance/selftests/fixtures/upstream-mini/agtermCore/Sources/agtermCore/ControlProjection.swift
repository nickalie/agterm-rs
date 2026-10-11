public struct ControlSessionNode: Codable, Sendable, Equatable {
    public let id: String
    public let splitRatio: Double?
    public let hud: Hud?

    public struct Hud: Codable, Sendable, Equatable {
        public let message: String
    }

    public enum Kind: String {
        case plain
    }
}

extension ControlSessionNode {
    public var hasHud: Bool { hud != nil }
}

public struct ControlWorkspaceNode: Codable, Sendable, Equatable {
    public let id: String
    public let sessions: [ControlSessionNode]
}

public struct ControlTree: Codable, Sendable, Equatable {
    public let workspaces: [ControlWorkspaceNode]
    public let sidebarVisible: Bool?
}

public struct ControlWindowNode: Codable, Sendable, Equatable {
    public let id: String
    public let isOpen: Bool
    public let minimized: Bool?

    enum CodingKeys: String, CodingKey {
        case id, isOpen = "open"
        case minimized
    }
}
