#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod catalog;
mod model;
mod platform;
mod runner;
mod search;
mod store;
mod tags;
mod transfer;
mod ui;
mod uninstall;

slint::include_modules!();

fn main() {
    if let Err(e) = start() {
        eprintln!("{e:#}");
        if std::env::args().len() == 1 {
            rfd::MessageDialog::new()
                .set_title("Application Pre-Installer")
                .set_description(format!("起動できませんでした。\n{e:#}"))
                .set_level(rfd::MessageLevel::Error)
                .show();
        }
        std::process::exit(1);
    }
}

fn start() -> anyhow::Result<()> {
    use crate::platform::DriverProvider;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--driver-worker") {
        anyhow::ensure!(args.len() == 2, "ワーカー引数が不正です");
        return platform::driver_worker(std::path::Path::new(&args[1]));
    }
    if args.iter().any(|a| a == "--validate-catalog") {
        let store = store::Store::open(false)?;
        let catalog = store
            .load_catalog()?
            .map(|c| c.catalog)
            .unwrap_or_else(model::Catalog::empty);
        catalog.validate()?;
        println!(
            "Valid cached catalog: {} entries; no bundled presets",
            catalog.apps.len()
        );
        return Ok(());
    }
    let demo = args.iter().any(|a| a == "--demo");
    let store = store::Store::open(demo)?;
    let backend = platform::Backend {
        store: store.clone(),
        demo,
    };
    if args.iter().any(|a| a == "--scan-apps") {
        anyhow::ensure!(
            args.iter()
                .all(|a| matches!(a.as_str(), "--scan-apps" | "--accept-source-agreements"))
                && args.iter().any(|a| a == "--accept-source-agreements"),
            "WinGetソースの利用条件に同意した場合は --scan-apps --accept-source-agreements を指定してください"
        );
        let cached = backend.fetch_catalog()?;
        println!(
            "{}",
            serde_json::json!({"count": cached.catalog.apps.len(), "fetched_at": cached.fetched_at, "cache": backend.store.root.join("cache/winget-catalog.json"), "presets": false})
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--check-environment") {
        println!("{}", serde_json::to_string_pretty(&backend.environment()?)?);
        return Ok(());
    }
    if args.iter().any(|a| a == "--scan-windows-apps") {
        anyhow::ensure!(
            args.len() == 1,
            "--scan-windows-apps を単独で指定してください"
        );
        println!(
            "{}",
            serde_json::to_string_pretty(&backend.removable_apps()?)?
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--scan-tags") {
        anyhow::ensure!(
            args.iter()
                .all(|a| matches!(a.as_str(), "--scan-tags" | "--demo")),
            "--scan-tags または --scan-tags --demo を指定してください"
        );
        let index = backend.fetch_tags()?;
        println!(
            "{}",
            serde_json::json!({"count": index.tags.len(), "source": index.source, "source_version": index.source_version, "cache": backend.store.root.join("cache/winget-tags.json")})
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--scan-installed-apps") {
        anyhow::ensure!(
            args.iter().all(|a| matches!(
                a.as_str(),
                "--scan-installed-apps" | "--accept-source-agreements" | "--demo"
            )) && (demo || args.iter().any(|a| a == "--accept-source-agreements")),
            "--scan-installed-apps --accept-source-agreements を指定してください"
        );
        let catalog = store
            .load_catalog()?
            .map(|c| c.catalog)
            .unwrap_or_else(model::Catalog::empty);
        let transfer = backend.export_installed(&catalog)?;
        let apps = transfer
            .profile
            .validate(&catalog)?
            .ordered(&transfer.profile.selected_apps)?;
        println!(
            "{}",
            serde_json::json!({"count": apps.len(), "winget": apps.iter().filter(|a| matches!(&a.method, model::InstallMethod::Winget { source, .. } if source == "winget")).count(), "msstore": apps.iter().filter(|a| matches!(&a.method, model::InstallMethod::Winget { source, .. } if source == "msstore")).count(), "registered_names": transfer.detected.len(), "missing_files": transfer.missing_files().len(), "installs_performed": 0})
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--scan-drivers") {
        println!("{}", serde_json::to_string_pretty(&backend.search()?)?);
        return Ok(());
    }
    anyhow::ensure!(
        args.iter().all(|a| a == "--demo"),
        "未対応の引数です。--demo / --validate-catalog / --scan-apps --accept-source-agreements / --scan-installed-apps --accept-source-agreements / --scan-tags / --check-environment / --scan-drivers / --scan-windows-apps"
    );
    let _instance_lock = store.lock()?;
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()?;
    ui::run(backend)
}
