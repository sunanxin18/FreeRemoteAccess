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

    let product_dependencies = dependencies
        .get("freeremotedesk-windows")
        .expect("Windows product package exists");
    let concrete_product_dependencies = product_dependencies
        .iter()
        .filter(|dependency| {
            dependency.starts_with("frd-protocol-") && *dependency != "frd-protocol-api"
        })
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(concrete_product_dependencies, vec!["frd-protocol-apple"]);

    let mut concrete_imports = Vec::new();
    collect_concrete_imports(&workspace.join("apps"), workspace, &mut concrete_imports);
    collect_concrete_imports(&workspace.join("crates"), workspace, &mut concrete_imports);
    concrete_imports.sort();
    assert_eq!(
        concrete_imports,
        vec!["apps/freeremotedesk-windows/src/main.rs".to_owned()],
        "only the Windows composition root may import the concrete Apple adapter"
    );

    let main_source =
        std::fs::read_to_string(workspace.join("apps/freeremotedesk-windows/src/main.rs"))
            .expect("Windows composition root source is readable");
    let concrete_crate = ["frd_protocol", "apple"].join("_");
    assert_eq!(main_source.matches(&concrete_crate).count(), 1);
    assert_eq!(main_source.matches("AppleProtocolFactory").count(), 2);
    assert_eq!(
        main_source
            .matches("Arc::new(AppleProtocolFactory)")
            .count(),
        1,
        "the product registers exactly the approved Apple factory"
    );
}

fn collect_concrete_imports(
    directory: &std::path::Path,
    workspace: &std::path::Path,
    imports: &mut Vec<String>,
) {
    for entry in std::fs::read_dir(directory).expect("workspace source directory is readable") {
        let path = entry.expect("source directory entry is readable").path();
        if path.is_dir() {
            if path == workspace.join("crates/frd-protocol-apple") {
                continue;
            }
            collect_concrete_imports(&path, workspace, imports);
        } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("rs") {
            let source = std::fs::read_to_string(&path).expect("Rust source is readable");
            let concrete_crate = ["frd_protocol", "apple"].join("_");
            if source.contains(&concrete_crate) {
                imports.push(
                    path.strip_prefix(workspace)
                        .expect("source stays inside workspace")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
}
