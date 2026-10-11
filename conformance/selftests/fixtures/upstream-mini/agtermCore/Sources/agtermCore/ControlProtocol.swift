/// A control command name. Raw values are the wire strings.
public enum Command: String, Codable, Sendable {
    case tree
    case eventsRead = "events.read"
    // case retired = "retired.command"
    /* case alsoRetired = "also.retired" { */
    case sessionResize = "session.resize"
    case fontInc = "font.inc", fontDec = "font.dec"
    case fontReset = "font.reset"
    case quick
    case sessionBackground = "session.background"
    case sessionHudOpen = "session.hud.open"
    case sessionOverlayOpen = "session.overlay.open"
    case sessionOverlayNavigate = "session.overlay.navigate"
    case zmxTree = "zmx.tree"
    case zmxAttach = "zmx.attach"
    case zmxPresent = "zmx.present"
    /// A viewer's helper claiming a remote overlay job; after an ok reply the connection carries job frames.
    case sessionOverlayJobRun = "session.overlay.job.run"
    case version = "version"
    /// UI-TEST-ONLY.
    case debugAppearance = "debug.appearance"

    public var isStreaming: Bool {
        switch self {
        case .zmxPresent, .sessionOverlayJobRun: return true
        default: return false
        }
    }
}

public struct ControlArgs: Codable, Sendable, Equatable {
    public var name: String?
}

public struct ControlResult: Codable, Sendable, Equatable {
    public var id: String?
    /// The applied split fraction; braces in a comment { do not count.
    public var ratio: Double?
    public var tree: ControlTree?
    public var note: String? = "a { brace in a string"
    public static let empty = ControlResult()
    public var isEmpty: Bool { id == nil }

    public init(id: String? = nil, ratio: Double? = nil,
                tree: ControlTree? = nil) {
        self.id = id
        self.ratio = ratio
        self.tree = tree
    }
}
