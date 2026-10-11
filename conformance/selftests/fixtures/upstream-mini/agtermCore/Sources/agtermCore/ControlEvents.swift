public enum ControlEventKind: String, Codable, CaseIterable, Sendable, Equatable {
    case status
    case paneSplit = "pane.split"
}
