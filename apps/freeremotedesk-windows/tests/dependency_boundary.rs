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

    let credential_feature_owners = packages
        .iter()
        .filter(|package| {
            package["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|dependency| {
                    dependency["name"] == "windows-sys"
                        && dependency["features"].as_array().is_some_and(|features| {
                            features
                                .iter()
                                .any(|feature| feature == "Win32_Security_Credentials")
                        })
                })
        })
        .map(|package| package["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        credential_feature_owners,
        vec!["frd-platform-windows"],
        "only the Windows platform adapter may enable Win32 credential APIs"
    );

    for neutral in [
        "frd-wire-rfb",
        "frd-protocol-api",
        "frd-protocol-apple",
        "frd-render-wgpu",
        "frd-compositor-wgpu",
    ] {
        let direct = dependencies.get(neutral).expect("neutral package exists");
        assert!(
            !direct
                .iter()
                .any(|dependency| dependency == "frd-platform-windows"),
            "{neutral} must not depend on the profile or secure-store implementation: {direct:?}"
        );
    }

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
    assert_eq!(
        concrete_product_dependencies,
        vec!["frd-protocol-apple", "frd-protocol-rdp"]
    );

    let mut concrete_imports = Vec::new();
    collect_concrete_imports(&workspace.join("apps"), workspace, &mut concrete_imports);
    collect_concrete_imports(&workspace.join("crates"), workspace, &mut concrete_imports);
    concrete_imports.sort();
    assert_eq!(
        concrete_imports,
        vec!["apps/freeremotedesk-windows/src/main.rs".to_owned()],
        "only the Windows composition root may import concrete protocol adapters"
    );

    let main_source =
        std::fs::read_to_string(workspace.join("apps/freeremotedesk-windows/src/main.rs"))
            .expect("Windows composition root source is readable");
    let apple_crate = ["frd_protocol", "apple"].join("_");
    let rdp_crate = ["frd_protocol", "rdp"].join("_");
    assert_eq!(main_source.matches(&apple_crate).count(), 1);
    assert_eq!(main_source.matches(&rdp_crate).count(), 1);
    assert_eq!(main_source.matches("AppleProtocolFactory").count(), 2);
    assert_eq!(
        main_source
            .matches("AppleHighPerformanceProtocolFactory")
            .count(),
        2
    );
    assert_eq!(main_source.matches("RdpProtocolFactory").count(), 2);
    assert_eq!(
        main_source
            .matches("Arc::new(AppleProtocolFactory)")
            .count(),
        1,
        "the product registers exactly the approved Apple factory"
    );
    assert_eq!(
        main_source
            .matches("Arc::new(AppleHighPerformanceProtocolFactory)")
            .count(),
        1,
        "the product registers exactly one explicit Apple High Performance factory"
    );
    assert_eq!(
        main_source.matches("Arc::new(RdpProtocolFactory)").count(),
        1,
        "the product registers exactly the approved RDP factory"
    );
}

#[test]
fn windows_composition_wires_each_store_once_and_purges_before_app_launch() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("Windows app is nested under the workspace");
    let main_source =
        std::fs::read_to_string(workspace.join("apps/freeremotedesk-windows/src/main.rs"))
            .expect("Windows composition root source is readable");

    verify_windows_store_composition(&main_source)
        .expect("the product composition matches the secure-store ledger");

    let without_purge = main_source.replacen(
        "purge_pending_credentials(credentials.as_ref())",
        "purge_pending_credentials_disabled(credentials.as_ref())",
        1,
    );
    assert!(verify_windows_store_composition(&without_purge).is_err());

    let duplicated_credential_store = main_source.replacen(
        "let credentials = Arc::new(WindowsCredentialStore::new());",
        "let credentials = Arc::new(WindowsCredentialStore::new());\n    let duplicate_credentials = Arc::new(WindowsCredentialStore::new());",
        1,
    );
    assert!(verify_windows_store_composition(&duplicated_credential_store).is_err());
}

fn verify_windows_store_composition(source: &str) -> Result<(), String> {
    let required_once = [
        "DpapiServerIdentityStore::current_user_default()",
        "WindowsConnectionProfileStore::current_user_default()",
        "WindowsCredentialStore::new()",
        "purge_pending_credentials(credentials.as_ref())",
        "let credentials = credentials as Arc<dyn SecureCredentialStore>;",
        "DesktopPlatformStores::new(server_identities, profiles, credentials)",
        "AppLaunch::new_with_stores(",
        "stores.as_app_stores()",
    ];
    for needle in required_once {
        let count = source.matches(needle).count();
        if count != 1 {
            return Err(format!(
                "composition marker must occur once: {needle} ({count})"
            ));
        }
    }

    let position = |needle: &str| {
        source
            .find(needle)
            .ok_or_else(|| format!("missing composition marker: {needle}"))
    };
    let credential_constructor = position("WindowsCredentialStore::new()")?;
    let purge = position("purge_pending_credentials(credentials.as_ref())")?;
    let credential_erasure =
        position("let credentials = credentials as Arc<dyn SecureCredentialStore>;")?;
    let store_bundle =
        position("DesktopPlatformStores::new(server_identities, profiles, credentials)")?;
    let app_launch = position("AppLaunch::new_with_stores(")?;
    let launch_stores = position("stores.as_app_stores()")?;

    if !(credential_constructor < purge
        && purge < credential_erasure
        && credential_erasure < store_bundle
        && store_bundle < app_launch
        && app_launch < launch_stores)
    {
        return Err("credential purge and store bundle must precede AppLaunch".to_owned());
    }

    Ok(())
}

fn collect_concrete_imports(
    directory: &std::path::Path,
    workspace: &std::path::Path,
    imports: &mut Vec<String>,
) {
    for entry in std::fs::read_dir(directory).expect("workspace source directory is readable") {
        let path = entry.expect("source directory entry is readable").path();
        if path.is_dir() {
            if path == workspace.join("crates/frd-protocol-apple")
                || path == workspace.join("crates/frd-protocol-rdp")
            {
                continue;
            }
            collect_concrete_imports(&path, workspace, imports);
        } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("rs") {
            let source = std::fs::read_to_string(&path).expect("Rust source is readable");
            let concrete_crates = [
                ["frd_protocol", "apple"].join("_"),
                ["frd_protocol", "rdp"].join("_"),
            ];
            if concrete_crates
                .iter()
                .any(|concrete_crate| source.contains(concrete_crate))
            {
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
