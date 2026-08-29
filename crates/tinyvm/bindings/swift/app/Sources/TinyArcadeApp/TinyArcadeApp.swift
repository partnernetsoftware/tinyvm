// The smallest app that is really an app: a SwiftUI scene whose one view
// calls into the TinyArcadeRuntime module, so the link against the
// XCFramework is exercised, not just declared. Building this for the
// simulator and for a device is acceptance #5.
import Foundation
import SwiftUI
import TinyArcadeRuntime

@main
struct TinyArcadeApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}

struct ContentView: View {
    /// `inspect` on an empty cartridge is refused by the runtime; the refusal
    /// text proves the call went through the bridge and came back.
    private let probe: String = {
        do {
            _ = try TinyArcadeCartridgeDescriptorV1.inspect(Data())
            return "inspected an empty cartridge"
        } catch {
            return "refused: \(error)"
        }
    }()

    var body: some View {
        VStack(spacing: 12) {
            Text("TinyArcade")
                .font(.title)
            Text(probe)
                .font(.footnote)
                .multilineTextAlignment(.center)
        }
        .padding()
    }
}
