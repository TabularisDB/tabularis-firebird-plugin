//! Compiled as a temporary example in a real Tabularis host checkout.
use std::{path::Path, sync::Arc, time::Duration};
use tabularis_lib::{
    application::plugins as application,
    drivers::registry as drivers,
    plugins::{install_cancellation, installer, registry, tabularium},
    runtime::{
        events::NoopRuntimeEvents, paths::FixedRuntimePaths, secrets::RuntimeSecrets,
        RuntimeContext,
    },
};

struct NoSecrets;
impl RuntimeSecrets for NoSecrets {
    fn get(&self, _: &str) -> Result<Option<String>, String> {
        Err("No credential access in this test".into())
    }
    fn set(&self, _: &str, _: &str) -> Result<(), String> {
        Err("No credential access in this test".into())
    }
    fn delete(&self, _: &str) -> Result<(), String> {
        Err("No credential access in this test".into())
    }
}

fn installed(runtime: &RuntimeContext, expected: &str) {
    let entries = application::get_installed_plugins(runtime);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "firebird");
    assert_eq!(entries[0].version, expected);
}

async fn running(expected: &str) {
    let driver = drivers::get_driver("firebird")
        .await
        .expect("installed driver was not loaded");
    assert_eq!(driver.manifest().version, expected);
    let params =
        serde_json::from_value(serde_json::json!({"driver":"firebird", "database":[]})).unwrap();
    let error = driver.test_connection(&params).await.unwrap_err();
    assert!(
        error.contains("database must be a server-side path or alias"),
        "unexpected plugin response: {error}"
    );
}

async fn lifecycle(root: &Path) {
    let runtime = RuntimeContext::new(
        Arc::new(FixedRuntimePaths::new(
            root.join("config"),
            root.join("data"),
        )),
        Arc::new(NoopRuntimeEvents),
        Arc::new(NoSecrets),
    );
    let registry_url = "https://registry.tabularis.dev";
    let old = std::env::var("FIREBIRD_REGISTRY_OLD_VERSION").unwrap();
    let current = std::env::var("FIREBIRD_REGISTRY_NEW_VERSION").unwrap();
    for version in [&old, &current] {
        assert_eq!(
            tabularium::check_release_signature(registry_url, "firebird", version).await,
            registry::SignatureStatus::Verified
        );
    }
    application::install_plugin(
        &runtime,
        "firebird".into(),
        Some(old.clone()),
        Some(registry_url.into()),
    )
    .await
    .unwrap();
    installed(&runtime, &old);
    running(&old).await;
    assert_eq!(
        registry::classify_install(Some(&old), &current),
        registry::InstallAction::Update
    );

    let asset = registry::resolve_tabularium_asset(
        registry_url,
        "firebird",
        &current,
        &registry::get_current_platform(),
    )
    .await
    .unwrap();
    assert!(
        asset.expected_sha256.is_some(),
        "signed hash must be enforced"
    );
    {
        let guard = install_cancellation::begin("firebird").unwrap();
        let error = installer::download_and_install_into(
            &runtime.paths.plugins_dir(),
            "firebird",
            &asset.download_url,
            Some(&"0".repeat(64)),
            Some(&current),
            guard.cancellation(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("SHA-256 mismatch"), "{error}");
    }
    installed(&runtime, &old);
    running(&old).await;
    {
        let guard = install_cancellation::begin("firebird").unwrap();
        let error = installer::download_and_install_into(
            &runtime.paths.plugins_dir(),
            "firebird",
            &asset.download_url,
            asset.expected_sha256.as_deref(),
            Some("999.0.0"),
            guard.cancellation(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("archive version mismatch"), "{error}");
    }
    installed(&runtime, &old);
    running(&old).await;
    {
        let guard = install_cancellation::begin("firebird").unwrap();
        assert!(install_cancellation::cancel("firebird"));
        let error = installer::download_and_install_into(
            &runtime.paths.plugins_dir(),
            "firebird",
            &asset.download_url,
            asset.expected_sha256.as_deref(),
            Some(&current),
            guard.cancellation(),
        )
        .await
        .unwrap_err();
        assert_eq!(error, install_cancellation::INSTALL_CANCELLED_ERROR);
    }
    installed(&runtime, &old);
    running(&old).await;

    application::install_plugin(
        &runtime,
        "firebird".into(),
        Some(current.clone()),
        Some(registry_url.into()),
    )
    .await
    .unwrap();
    installed(&runtime, &current);
    running(&current).await;
    assert_eq!(
        registry::classify_install(Some(&current), &current),
        registry::InstallAction::UpToDate
    );
    application::disable_plugin("firebird".into())
        .await
        .unwrap();
    assert!(drivers::get_driver("firebird").await.is_none());
    installed(&runtime, &current);
    application::enable_plugin(&runtime, "firebird".into())
        .await
        .unwrap();
    running(&current).await;
    application::uninstall_plugin(&runtime, "firebird".into())
        .await
        .unwrap();
    assert!(application::get_installed_plugins(&runtime).is_empty());
    assert!(!drivers::is_registered("firebird").await);
    assert!(!runtime
        .paths
        .plugins_dir()
        .join("drivers/firebird")
        .exists());
    assert!(!runtime
        .paths
        .plugins_dir()
        .join("drivers/.tmp-firebird")
        .exists());
    println!("Verified actual host registry lifecycle: signed install {old}, rejected hash/version/cancel preserving running driver, update {current}, disable/enable and uninstall.");
}

#[tokio::main]
async fn main() {
    let parent = std::env::var("FIREBIRD_REGISTRY_TEST_ROOT").unwrap();
    let root = tempfile::Builder::new()
        .prefix("host-registry-")
        .tempdir_in(parent)
        .unwrap();
    let path = root.path().to_path_buf();
    let result = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(180), lifecycle(&path))
            .await
            .unwrap();
    })
    .await;
    drivers::shutdown_external_drivers().await;
    drop(root);
    result.unwrap();
}
