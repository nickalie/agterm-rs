import SwiftUI

struct AskDialogView: View {
    let choices: [Choice]
    let explicitItems: [String]?

    var body: some View {
        VStack {
            // .accessibilityIdentifier("commented-out")
            Text("call .accessibilityIdentifier(\"in-a-string\") here")
                .accessibilityIdentifier("ask-dialog")
            ForEach(choices) { choice in
                Button(choice.title) {}
                    .accessibilityIdentifier("ask-button-\(choice.id)")
            }
        }
        .accessibilityIdentifier(
            explicitItems == nil
                ? "command-palette"
                : "pick-palette"
        )
        .accessibilityIdentifier("ask-dialog")
    }
}
