import AppKit

enum WelcomeAlert {
    static func make() -> NSAlert {
        let alert = NSAlert()
        alert.buttons.first?.setAccessibilityIdentifier("welcome-install")
        /* alert.buttons.last?.setAccessibilityIdentifier("block-comment") */
        return alert
    }

    static func row(_ field: NSTextField, kind: Kind) {
        field.setAccessibilityIdentifier(kind == .workspace ? "workspace-row" : "session-row")
    }
}
