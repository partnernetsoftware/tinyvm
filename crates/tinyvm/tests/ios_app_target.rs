//! Acceptance #5: a real iOS app target consumes the XCFramework.
//!
//! The build itself is `smoke-ios-bridge.sh` (the third verification
//! command; it needs macOS, Xcode and xcodegen): it generates the project
//! from `bindings/swift/app/project.yml`, builds it for the simulator and
//! for a device with signing off, and checks the `.app` exists. This test
//! pins what the smoke relies on being in the repository, so a deleted spec
//! or a removed smoke step is caught by `cargo test` on any host.

use std::path::Path;

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn the_app_target_spec_and_source_are_in_the_repository() {
    let spec = crate_dir().join("bindings/swift/app/project.yml");
    let source = crate_dir().join("bindings/swift/app/Sources/TinyArcadeApp/TinyArcadeApp.swift");
    let spec_text = std::fs::read_to_string(&spec).expect("project.yml");
    let source_text = std::fs::read_to_string(&source).expect("TinyArcadeApp.swift");
    assert!(spec_text.contains("type: application"), "an application target");
    assert!(spec_text.contains("product: TinyArcadeRuntime"), "consumes the runtime product");
    assert!(source_text.contains("import TinyArcadeRuntime"), "the app imports the module");
    assert!(
        source_text.contains("TinyArcadeCartridgeDescriptorV1.inspect"),
        "the app calls through the bridge, not only links it"
    );
}

#[test]
fn the_ios_smoke_builds_the_app_target_for_both_destinations() {
    let smoke = std::fs::read_to_string(crate_dir().join("smoke-ios-bridge.sh")).expect("smoke script");
    assert!(smoke.contains("xcodegen generate"), "the project is generated from the spec");
    assert!(smoke.contains("-scheme TinyArcadeApp"), "the app scheme is built");
    assert!(smoke.contains("generic/platform=iOS Simulator") && smoke.contains("generic/platform=iOS'"), "simulator and device");
    assert!(smoke.contains("TinyArcadeApp.app"), "the built app is checked for");
}
