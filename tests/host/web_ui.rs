//! Real web transport/application APIs with explicit disposable storage.
use std::{path::PathBuf, sync::Arc};
use tabularis_lib::{
    application::RuntimeApplicationApi,
    cli::WebAuthMode,
    runtime::{
        bootstrap, paths::FixedRuntimePaths, secrets::RuntimeSecrets, state::ApplicationState,
        RuntimeContext,
    },
    transport::web::{
        events::WebEventBus,
        server::{self, WebServerOptions},
    },
};

struct NoSecrets;
impl RuntimeSecrets for NoSecrets {
    fn get(&self, _: &str) -> Result<Option<String>, String> {
        Err("OS credentials disabled in UI test".into())
    }
    fn set(&self, _: &str, _: &str) -> Result<(), String> {
        Err("OS credentials disabled in UI test".into())
    }
    fn delete(&self, _: &str) -> Result<(), String> {
        Err("OS credentials disabled in UI test".into())
    }
}

#[tokio::main]
async fn main() {
    let root =
        PathBuf::from(std::env::var("TABULARIS_DATA_DIR").expect("explicit test storage required"));
    assert!(root.is_absolute());
    std::fs::create_dir_all(&root).unwrap();
    assert_eq!(tabularis_lib::paths::get_app_config_dir(), root);
    assert_eq!(tabularis_lib::paths::get_app_data_dir(), root);
    bootstrap::install_process_prerequisites();
    bootstrap::initialize_logging(false);
    let events = WebEventBus::default();
    let context = RuntimeContext::new(
        Arc::new(FixedRuntimePaths::new(root.clone(), root.clone())),
        Arc::new(events.clone()),
        Arc::new(NoSecrets),
    );
    assert!(context.paths.plugins_dir().starts_with(&root));
    bootstrap::bootstrap_application(
        context.clone(),
        bootstrap::BootstrapOptions {
            load_external_plugins: false,
            run_connection_migrations: false,
        },
    )
    .await
    .unwrap();
    let state = Arc::new(ApplicationState::default());
    let port: u16 = std::env::var("FIREBIRD_UI_PORT").unwrap().parse().unwrap();
    let base = format!("http://127.0.0.1:{port}");
    server::run(WebServerOptions {
        trusted_proxies: vec![],
        host: "127.0.0.1".into(),
        port,
        web_root: PathBuf::from(std::env::var("FIREBIRD_UI_WEB_ROOT").unwrap()),
        data_dir: root.clone(),
        plugins_dir: context.paths.plugins_dir(),
        open_browser: false,
        auth: Some(WebAuthMode::Password),
        public_url: Some(base.clone()),
        allowed_origins: vec![base],
        allow_high_risk: true,
        server_file_browser_roots: vec![root],
        application: Arc::new(RuntimeApplicationApi::new(context, state)),
        events,
    })
    .await
    .unwrap();
}
