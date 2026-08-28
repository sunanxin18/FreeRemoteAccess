use std::collections::BTreeMap;
use std::process::Command;

#[test]
fn product_dependency_graph_preserves_protocol_and_legacy_boundaries() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("Windows app is nested under the workspace");
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(workspace)
        .env("RUSTUP_TOOLCHAIN", "stable")
        .env("CARGO_BUILD_JOBS", "2")
        .output()
        .expect("cargo metadata starts");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    let dependencies = packages
        .iter()
        .map(|package| {
            let name = package["name"].as_str().unwrap().to_owned();
            let direct = package["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .map(|dependency| dependency["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            (name, direct)
        })
        .collect::<BTreeMap<_, _>>();

    for neutral in [
        "frd-core",
        "frd-frame",
        "frd-media-api",
        "frd-protocol-api",
        "frd-session",
        "frd-app",
        "frd-ui-model",
        "frd-ui-egui",
        "frd-render-wgpu",
        "frd-compositor-wgpu",
        "frd-platform-api",
        "frd-platform-windows",
        "frd-shell-desktop",
    ] {
        let direct = dependencies.get(neutral).expect("neutral package exists");
        assert!(
            !direct.iter().any(|dependency| {
                dependency.starts_with("frd-protocol-") && dependency != "frd-protocol-api"
            }),
            "{neutral} must not depend on a concrete protocol: {direct:?}"
        );
    }

    for (package, direct) in &dependencies {
        if direct.iter().any(|dependency| dependency == "minifb") {
            assert_eq!(package, "frd-legacy-minifb-lab");
        }
    }
}
