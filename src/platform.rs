use crate::{
    model::*,
    store::{CachedCatalog, Store, read_json, write_json},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Clone)]
pub struct Backend {
    pub store: Store,
    pub demo: bool,
}
pub struct AppDetails {
    pub text: String,
    pub publisher_url: Option<String>,
}

pub trait DriverProvider {
    fn search(&self) -> Result<DriverScan>;
    fn install(
        &self,
        drivers: &[DriverCandidate],
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(&DriverProgress),
    ) -> Result<DriverProgress>;
    fn verify(&self, driver: &DriverCandidate) -> Result<Outcome>;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DriverScan {
    pub candidates: Vec<DriverCandidate>,
    pub excluded: usize,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DriverProgress {
    pub outcomes: BTreeMap<String, Outcome>,
    pub running: Option<String>,
    pub finished: bool,
}
#[derive(Serialize, Deserialize)]
struct DriverRequest {
    drivers: Vec<DriverCandidate>,
}

pub trait Installer {
    fn installed(&self, app: &AppEntry) -> Result<bool>;
    fn install_app(
        &self,
        app: &AppEntry,
        update: bool,
        log: &mut dyn FnMut(String),
    ) -> Result<Outcome>;
}

impl Backend {
    pub fn environment(&self) -> Result<Environment> {
        if self.demo {
            return Ok(Environment {
                os: "Windows 11 (デモ表示)".into(),
                windows_client: true,
                network_available: true,
                build: 26100,
                architecture: "x64".into(),
                manufacturer: "DEMO".into(),
                model: "Preview PC".into(),
                board_manufacturer: "サンプル".into(),
                board_product: "マザーボード".into(),
                board_revision: "1.0".into(),
                winget: "デモ / 実行しません".into(),
                warnings: vec!["デモモード: PCへのインストールや設定変更は行いません".into()],
                ..Default::default()
            });
        }
        let mut environment: Environment = ps_json(
            include_str!("../scripts/environment.ps1"),
            &serde_json::json!({}),
        )?;
        if !environment.supported() {
            environment
                .warnings
                .push("初版の対応OSはWindows 11 x64です。インストールは無効になります。".into());
        }
        Ok(environment)
    }
    pub fn prepare_winget(&self) -> Result<String> {
        ensure!(!self.demo, "デモモードでは準備処理を実行しません");
        let output = run_ps(
            include_str!("../scripts/bootstrap.ps1"),
            &serde_json::json!({}),
        )?;
        ensure!(
            output.code == 0,
            "WinGet準備に失敗しました。Microsoft Storeの「アプリ インストーラー」を更新して再ログインしてください。\n{}",
            output.text
        );
        Ok(output.text)
    }
    pub fn search_apps(&self, query: &str) -> Result<Vec<AppEntry>> {
        self.search_packages("--query", query)
    }
    pub fn search_tag(&self, tag: &str) -> Result<Vec<AppEntry>> {
        ensure!(
            !tag.trim().is_empty() && tag.chars().count() <= 100,
            "タグは1〜100文字で入力してください"
        );
        self.search_packages("--tag", tag.trim())
    }
    pub fn fetch_catalog(&self) -> Result<CachedCatalog> {
        let apps = self.search_apps(".")?;
        ensure!(
            !apps.is_empty(),
            "一覧を取得できませんでした。WinGetのソース・ネットワークを確認してください"
        );
        let cached = CachedCatalog::new(Catalog {
            schema_version: SCHEMA_VERSION,
            apps,
        });
        self.store.save_catalog(&cached)?;
        Ok(cached)
    }
    fn search_packages(&self, flag: &str, query: &str) -> Result<Vec<AppEntry>> {
        if self.demo {
            let count = if flag == "--tag" { 80 } else { 20_000 };
            return (0..count)
                .map(|i| {
                    if flag != "--tag" && i < 2 {
                        return AppEntry::from_winget(
                            if i == 0 {
                                "Google Chrome"
                            } else {
                                "Google Chrome Beta"
                            },
                            if i == 0 {
                                "Demo.Chrome"
                            } else {
                                "Demo.Chrome.Beta"
                            },
                            "1.0（デモ候補・実行しません）",
                        );
                    }
                    AppEntry::from_winget(
                        &format!("Demo application {i:05}"),
                        &format!("Demo.Application{i:05}"),
                        "1.0 (架空データ)",
                    )
                })
                .collect();
        }
        // The equals form keeps tags such as "- cli" from being parsed as options.
        let query_argument = format!("{flag}={query}");
        let output = winget(&[
            "search",
            &query_argument,
            "--source",
            "winget",
            "--accept-source-agreements",
            "--disable-interactivity",
        ])?;
        if output.code == 0x8A150014 {
            return Ok(Vec::new());
        }
        ensure!(
            output.code == 0,
            "WinGet検索に失敗しました (0x{:08X})\n{}",
            output.code,
            output.text
        );
        parse_search(&output.text)
    }
    pub fn details(&self, app: &AppEntry) -> Result<String> {
        let mut details = format!(
            "{}\n{}\n提供元: {}\n参照ページ: {}\n取得方法: {}\n依存関係: {}\n",
            app.name,
            app.description,
            app.publisher,
            app.website,
            app.source_label(),
            if app.dependencies.is_empty() {
                "なし（WinGet内部の依存関係はWinGetが解決）".to_string()
            } else {
                app.dependencies.join(", ")
            }
        );
        match &app.method {
            InstallMethod::Winget { package_id, source } if !self.demo => {
                let r = winget(&["show", "--id", package_id, "--exact", "--source", source, "--accept-source-agreements", "--disable-interactivity"])?;
                ensure!(r.code == 0, "候補の取得元・利用条件を確認できません (0x{:08X})。公式サイトから手動で確認してください。\n{}", r.code, r.text);
                details.push_str(&r.text);
            }
            InstallMethod::Custom { args, sha256, .. } => details.push_str(&format!("引数（JSON）: {}\nSHA-256: {}\n独自登録は対話型で起動します。終了コードだけで成功とは判定しません。", serde_json::to_string(args)?, sha256.as_deref().unwrap_or("未指定"))),
            _ => {}
        }
        Ok(details)
    }
    pub fn app_details(&self, app: &AppEntry) -> Result<AppDetails> {
        let text = self.details(app)?;
        let publisher_url = publisher_url(&text);
        Ok(AppDetails {
            text,
            publisher_url,
        })
    }
    pub fn review(&self, apps: &[AppEntry], drivers: &[DriverCandidate]) -> Result<String> {
        let mut result = String::new();
        for d in drivers {
            ensure!(d.eligible(), "対象外のドライバーです");
            result.push_str(&format!("【ドライバー】{}\n提供元: {} / Windows Update\nハードウェアID: {}\n利用条件:\n{}\n\n", d.title, d.provider, d.hardware_id, if d.eula.is_empty() { "個別の利用条件は提示されていません。" } else { &d.eula }));
        }
        for app in apps {
            result.push_str(&self.details(app)?);
            result.push_str("\n\n────────────────────────\n\n");
        }
        Ok(result)
    }
    pub fn verify_driver(&self, driver: &DriverCandidate) -> Result<Outcome> {
        if self.demo {
            return Ok(Outcome::new(
                Status::Unknown,
                "デモのため状態を確認しません",
            ));
        }
        let result: bool = ps_json(
            "$s = New-Object -ComObject Microsoft.Update.Session; $r = $s.CreateUpdateSearcher().Search(\"IsInstalled=1 and Type='Driver' and UpdateID='$($payload.update_id)'\"); if ([int]$r.ResultCode -ne 2) { throw '更新状態を確認できません' }; [bool]($r.Updates.Count -gt 0) | ConvertTo-Json -Compress",
            &serde_json::json!({ "update_id": driver.update_id }),
        )?;
        Ok(if result {
            Outcome::new(
                Status::Success,
                "再検査でドライバー更新の適用を確認しました",
            )
        } else {
            Outcome::new(
                Status::Unknown,
                "適用を確認できません。再検索してから再選択してください",
            )
        })
    }
}

impl DriverProvider for Backend {
    fn search(&self) -> Result<DriverScan> {
        if self.demo {
            return Ok(DriverScan::default());
        }
        let script = format!(
            "{}\n{}",
            include_str!("../scripts/driver-common.ps1"),
            include_str!("../scripts/driver-scan.ps1")
        );
        let mut scan: DriverScan = ps_json(&script, &serde_json::json!({}))?;
        let before = scan.candidates.len();
        scan.candidates.retain(DriverCandidate::eligible);
        scan.excluded += before - scan.candidates.len();
        Ok(scan)
    }
    fn verify(&self, driver: &DriverCandidate) -> Result<Outcome> {
        self.verify_driver(driver)
    }
    fn install(
        &self,
        drivers: &[DriverCandidate],
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(&DriverProgress),
    ) -> Result<DriverProgress> {
        ensure!(
            drivers.iter().all(DriverCandidate::eligible),
            "対象外のドライバーを拒否しました"
        );
        if self.demo {
            return Ok(DriverProgress {
                finished: true,
                running: None,
                outcomes: drivers
                    .iter()
                    .map(|d| {
                        (
                            d.key(),
                            Outcome::new(Status::Unknown, "デモモード: インストールは実行しません"),
                        )
                    })
                    .collect(),
            });
        }
        let dir = self
            .store
            .root
            .join("cache")
            .join(format!("drivers-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir)?;
        let request = dir.join("request.json");
        write_json(
            &request,
            &DriverRequest {
                drivers: drivers.to_vec(),
            },
        )?;
        elevated_worker(&request, cancel, progress)?;
        let r: DriverProgress = read_json(&dir.join("result.json"))
            .context("管理者処理の結果を読み込めません。権限拒否・処理中断の可能性があります")?;
        ensure!(
            r.finished,
            "ドライバー処理が途中で終了しました。再開時に適用状態を確認してください"
        );
        Ok(r)
    }
}

impl Installer for Backend {
    fn installed(&self, app: &AppEntry) -> Result<bool> {
        if self.demo {
            return Ok(false);
        }
        if let InstallMethod::Winget { package_id, source } = &app.method {
            let r = winget(&[
                "list",
                "--id",
                package_id,
                "--exact",
                "--source",
                source,
                "--accept-source-agreements",
                "--disable-interactivity",
            ])?;
            if r.code == 0 {
                return Ok(true);
            }
            if r.code == 0x8A150014 {
                return Ok(false);
            }
            bail!(
                "インストール済み状態を確認できません (0x{:08X})\n{}",
                r.code,
                r.text
            );
        }
        let present: bool = ps_json(
            "$found = $false; foreach ($k in @('HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*','HKLM:\\SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*','HKCU:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*')) { if (@(Get-ItemProperty $k -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -eq $payload.name }).Count -gt 0) { $found = $true } }; $found | ConvertTo-Json -Compress",
            &serde_json::json!({"name": app.name}),
        )?;
        Ok(present)
    }
    fn install_app(
        &self,
        app: &AppEntry,
        update: bool,
        log: &mut dyn FnMut(String),
    ) -> Result<Outcome> {
        app.validate()?;
        if self.demo {
            return Ok(Outcome::new(
                Status::Unknown,
                "デモモード: インストールは実行しません",
            ));
        }
        match &app.method {
            InstallMethod::Manual { .. } => {
                if !update && self.installed(app).unwrap_or(false) {
                    Ok(Outcome::new(
                        Status::Skipped,
                        "同名アプリの登録を確認したためスキップしました",
                    ))
                } else {
                    Ok(Outcome::new(
                        Status::AwaitingUser,
                        "公式サイトまたはランチャーで操作してください。「公式ページ」ボタンから開けます",
                    ))
                }
            }
            InstallMethod::Unavailable { reason } => Ok(Outcome::new(Status::Skipped, reason)),
            InstallMethod::Winget { package_id, source } => {
                let installed = self.installed(app)?;
                if installed && !update {
                    return Ok(Outcome::new(
                        Status::Skipped,
                        "インストール済みのためスキップしました",
                    ));
                }
                let verb = if installed { "upgrade" } else { "install" };
                let mut args = vec![
                    verb,
                    "--id",
                    package_id,
                    "--exact",
                    "--source",
                    source,
                    "--silent",
                    "--accept-source-agreements",
                    "--accept-package-agreements",
                    "--disable-interactivity",
                ];
                if !installed {
                    args.push("--no-upgrade");
                }
                // Never --allow-reboot, --force, --ignore-security-hash or --override.
                log(format!("WinGet {verb}: {package_id} / {source}"));
                let result = winget(&args)?;
                let failure = winget_failure(&result);
                log(result.text);
                if installed && result.code == 0x8A15002B {
                    return Ok(Outcome::new(
                        Status::Skipped,
                        "更新可能なバージョンがありません",
                    ));
                }
                if result.code == 3010 || matches!(result.code, 0x8A150109..=0x8A15010B) {
                    return Ok(Outcome::new(
                        Status::RebootRequired,
                        "再起動が必要です。利用者が再起動後、状態を再確認してください",
                    ));
                }
                if let Some(outcome) = failure {
                    return Ok(outcome);
                }
                match self.installed(app) {
                    Ok(true) => Ok(Outcome::new(
                        Status::Success,
                        "WinGetの正常終了とインストール済み状態を確認しました",
                    )),
                    _ => Ok(Outcome::new(
                        Status::Unknown,
                        "インストーラーは正常終了しましたが、インストール済み状態を確認できません",
                    )),
                }
            }
            InstallMethod::Custom {
                location,
                args,
                sha256,
            } => {
                let was_installed = self.installed(app)?;
                if was_installed && !update {
                    return Ok(Outcome::new(
                        Status::Skipped,
                        "同名アプリをレジストリで確認したためスキップしました",
                    ));
                }
                let path = if location.starts_with("https://") {
                    self.download(location, sha256.as_deref(), log)?
                } else {
                    PathBuf::from(location)
                        .canonicalize()
                        .context("ローカルインストーラーが見つかりません")?
                };
                validate_installer_name(path.file_name().and_then(|s| s.to_str()).unwrap_or(""))?;
                if let Some(expected) = sha256 {
                    verify_hash(&path, expected)?;
                }
                let msi = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .eq_ignore_ascii_case("msi");
                if !msi {
                    let mut f = File::open(&path)?;
                    let mut magic = [0; 2];
                    f.read_exact(&mut magic)?;
                    ensure!(&magic == b"MZ", "Windows実行ファイルではありません");
                }
                log(format!(
                    "独自登録を対話型で起動: {}。再起動を要求された場合は「後で」を選んでください",
                    path.display()
                ));
                let mut command = if msi {
                    let mut c = Command::new(system_exe("msiexec.exe"));
                    c.arg("/i").arg(&path).args(args).arg("/norestart");
                    c
                } else {
                    let mut c = Command::new(&path);
                    c.args(args);
                    c
                };
                // EXEs are a manual handoff: no unattended switches are invented for arbitrary installers.
                let code = match command.status() {
                    Ok(status) => status.code().unwrap_or(-1) as u32,
                    Err(e) if e.raw_os_error() == Some(740) => interactive_elevated(&path, args)?,
                    Err(e) => return Err(e).context("インストーラーを起動できません"),
                };
                if code == 3010 {
                    return Ok(Outcome::new(
                        Status::RebootRequired,
                        "インストーラーが再起動を要求しています",
                    ));
                }
                if code != 0 {
                    return Ok(Outcome::new(
                        Status::Failed,
                        format!("インストーラー終了コード: {code}"),
                    ));
                }
                if was_installed {
                    Ok(Outcome::new(
                        Status::Unknown,
                        "既存アプリはありますが、独自インストーラーによる更新完了は自動確認できません。版を確認してください",
                    ))
                } else if self.installed(app)? {
                    Ok(Outcome::new(
                        Status::Success,
                        "インストーラー正常終了後、同名アプリの登録を確認しました",
                    ))
                } else {
                    Ok(Outcome::new(
                        Status::Unknown,
                        "インストーラーは終了しました。登録名との一致を確認できないため、完了状態を確認してください",
                    ))
                }
            }
        }
    }
}

impl Backend {
    fn download(
        &self,
        url: &str,
        expected: Option<&str>,
        log: &mut dyn FnMut(String),
    ) -> Result<PathBuf> {
        let parsed = validate_https(url)?;
        let name = parsed
            .path_segments()
            .and_then(|mut x| x.next_back())
            .context("直接ダウンロードURLが必要です")?;
        validate_installer_name(name)?;
        let ext = if name.to_ascii_lowercase().ends_with(".msi") {
            "msi"
        } else {
            "exe"
        };
        let path = self.store.root.join("cache").join(format!(
            "installer-{}.{}",
            uuid::Uuid::new_v4(),
            ext
        ));
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(3600))
            .https_only(true)
            .user_agent("ApplicationPreInstaller/0.2.0")
            .build()?;
        let mut response = client.get(url).send()?.error_for_status()?;
        ensure!(
            response.content_length().unwrap_or(0) <= 2 * 1024 * 1024 * 1024,
            "インストーラーが2GiBを超えています"
        );
        log(format!("ダウンロード: {url}"));
        let result = (|| -> Result<()> {
            let mut out = File::create_new(&path)?;
            let mut total = 0u64;
            let mut buffer = [0u8; 65536];
            loop {
                let n = response.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                ensure!(
                    total <= 2 * 1024 * 1024 * 1024,
                    "ダウンロード上限を超えました"
                );
                out.write_all(&buffer[..n])?;
            }
            out.sync_all()?;
            if let Some(hash) = expected {
                verify_hash(&path, hash)?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&path);
        }
        result?;
        Ok(path)
    }
}

pub fn driver_worker(request: &Path) -> Result<()> {
    ensure!(
        request.file_name().and_then(|s| s.to_str()) == Some("request.json"),
        "不正なワーカー要求です"
    );
    let dir = request.parent().context("要求パスが不正です")?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        for ancestor in dir.ancestors() {
            ensure!(
                fs::symlink_metadata(ancestor)?.file_attributes() & 0x400 == 0,
                "ワーカー通信にリンクやジャンクションは使用できません"
            );
        }
    }
    let dirname = dir
        .file_name()
        .and_then(|s| s.to_str())
        .context("要求パスが不正です")?;
    ensure!(
        dirname.starts_with("drivers-") && uuid::Uuid::parse_str(&dirname[8..]).is_ok(),
        "要求ディレクトリが不正です"
    );
    ensure!(
        fs::metadata(request)?.len() < 4 * 1024 * 1024,
        "要求が大きすぎます"
    );
    let request: DriverRequest = read_json(request)?;
    ensure!(
        !request.drivers.is_empty()
            && request.drivers.len() <= 256
            && request.drivers.iter().all(DriverCandidate::eligible),
        "対象外の要求です"
    );
    let mut result = DriverProgress::default();
    let path = dir.join("result.json");
    for driver in &request.drivers {
        if dir.join("cancel").exists() {
            break;
        }
        result.running = Some(driver.key());
        write_json(&path, &result)?;
        let script = format!(
            "{}\n{}",
            include_str!("../scripts/driver-common.ps1"),
            include_str!("../scripts/driver-install.ps1")
        );
        let outcome = ps_json::<Outcome>(
            &script,
            &serde_json::json!({"update_id": driver.update_id, "revision": driver.revision}),
        )
        .unwrap_or_else(|e| Outcome::new(Status::Failed, format!("{e:#}")));
        let reboot = outcome.status == Status::RebootRequired;
        result.outcomes.insert(driver.key(), outcome);
        write_json(&path, &result)?;
        if reboot {
            break;
        }
    }
    result.running = None;
    result.finished = true;
    write_json(&path, &result)
}

#[cfg(windows)]
fn elevated_worker(
    request: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&DriverProgress),
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{GetExitCodeProcess, WaitForSingleObject},
        UI::{
            Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::SW_HIDE,
        },
    };
    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }
    let exe = wide(std::env::current_exe()?.as_os_str());
    let verb = wide(std::ffi::OsStr::new("runas"));
    let parameter = wide(std::ffi::OsStr::new(&format!(
        "--driver-worker \"{}\"",
        request.display()
    )));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = exe.as_ptr();
    info.lpParameters = parameter.as_ptr();
    info.nShow = SW_HIDE;
    // ShellExecuteEx owns the child; buffers remain alive through this call. Close its handle once.
    ensure!(
        unsafe { ShellExecuteExW(&mut info) } != 0,
        "管理者権限が許可されませんでした: {}",
        std::io::Error::last_os_error()
    );
    ensure!(
        !info.hProcess.is_null(),
        "ワーカーのプロセスハンドルを取得できません"
    );
    let dir = request.parent().unwrap();
    let mut last = String::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = fs::write(dir.join("cancel"), b"stop");
        }
        if let Ok(text) = fs::read_to_string(dir.join("result.json"))
            && text != last
        {
            if let Ok(state) = serde_json::from_str::<DriverProgress>(&text) {
                progress(&state);
            }
            last = text;
        }
        let wait = unsafe { WaitForSingleObject(info.hProcess, 250) };
        if wait == WAIT_OBJECT_0 {
            break;
        }
        if wait == 0xFFFFFFFF {
            unsafe {
                CloseHandle(info.hProcess);
            }
            bail!(
                "ワーカーの待機に失敗しました: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    let mut code = 1;
    unsafe {
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    ensure!(code == 0, "管理者ワーカーが終了しました: {code}");
    Ok(())
}
#[cfg(not(windows))]
fn elevated_worker(_: &Path, _: &AtomicBool, _: &mut dyn FnMut(&DriverProgress)) -> Result<()> {
    bail!("Windows専用機能です")
}

#[cfg(windows)]
fn interactive_elevated(path: &Path, args: &[String]) -> Result<u32> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{GetExitCodeProcess, WaitForSingleObject},
        UI::{
            Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    };
    let exe: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let verb: Vec<_> = "open".encode_utf16().chain(Some(0)).collect();
    let params: Vec<_> = args
        .iter()
        .map(|a| quote_windows_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = exe.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    ensure!(
        unsafe { ShellExecuteExW(&mut info) } != 0,
        "インストーラーを起動できません（権限拒否を含む）: {}",
        std::io::Error::last_os_error()
    );
    ensure!(
        !info.hProcess.is_null(),
        "インストーラーの終了を確認できません"
    );
    let wait = unsafe { WaitForSingleObject(info.hProcess, u32::MAX) };
    let mut code = 1;
    unsafe {
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    ensure!(wait == 0, "インストーラーの終了待機に失敗しました");
    Ok(code)
}
#[cfg(not(windows))]
fn interactive_elevated(_: &Path, _: &[String]) -> Result<u32> {
    bail!("Windows専用機能です")
}
fn quote_windows_arg(arg: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            slashes += 1;
        } else {
            out.push_str(&"\\".repeat(if c == '\"' { slashes * 2 + 1 } else { slashes }));
            out.push(c);
            slashes = 0;
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('\"');
    out
}

pub struct ProcessOutput {
    pub code: u32,
    pub text: String,
}
fn publisher_url(text: &str) -> Option<String> {
    for label in [
        "homepage",
        "package url",
        "packageurl",
        "ホームページ",
        "publisher url",
        "publisherurl",
        "発行元 url",
    ] {
        for line in text.lines() {
            if let Some((key, value)) = line.split_once(':')
                && key.trim().eq_ignore_ascii_case(label)
                && validate_https(value.trim()).is_ok()
            {
                return Some(value.trim().to_owned());
            }
        }
    }
    None
}

fn winget_failure(output: &ProcessOutput) -> Option<Outcome> {
    if output.code == 0 {
        return None;
    }
    // WinGet can wrap an AppX HRESULT in its own exit code. Inspect complete
    // output tokens as well, without confusing a longer code with this HRESULT.
    let requires_admin = output.code == 0x80073D28
        || output
            .text
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|token| {
                token.eq_ignore_ascii_case("0x80073D28")
                    || token
                        .eq_ignore_ascii_case("ERROR_PACKAGED_SERVICE_REQUIRES_ADMIN_PRIVILEGES")
            });
    if requires_admin {
        return Some(Outcome::new(
            Status::AwaitingUser,
            format!(
                "0x80073D28: 管理者権限が必要です。\nサービスを含むパッケージの導入が通常権限では許可されませんでした。\n「詳細」の配布元情報から公式インストーラーを取得し、管理者の承認を受けて導入してください。\n手動導入の完了を確認してから、必要に応じて再選択してください。\nWinGet終了コード: 0x{:08X}",
                output.code
            ),
        ));
    }
    Some(Outcome::new(
        Status::Failed,
        format!(
            "WinGet終了コード 0x{:08X}。詳細ログを確認してください。公式サイトからの手動導入も利用できます",
            output.code
        ),
    ))
}

pub fn run_ps(script: &str, payload: &impl Serialize) -> Result<ProcessOutput> {
    ensure!(cfg!(windows), "Windows専用機能です");
    let wrapper = format!(
        "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false); $OutputEncoding=[Console]::OutputEncoding; try {{ $payload = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($env:API_PAYLOAD)) | ConvertFrom-Json; {script}\n }} catch {{ [Console]::Error.WriteLine($_.Exception.ToString()); exit 1 }}"
    );
    let encoded = STANDARD.encode(
        wrapper
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let mut cmd = Command::new(powershell());
    cmd.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-EncodedCommand",
        &encoded,
    ])
    .env("API_PAYLOAD", STANDARD.encode(serde_json::to_vec(payload)?));
    capture(cmd)
}
pub fn ps_json<T: serde::de::DeserializeOwned>(
    script: &str,
    payload: &impl Serialize,
) -> Result<T> {
    let output = run_ps(script, payload)?;
    ensure!(
        output.code == 0,
        "Windows処理に失敗しました (0x{:08X})\n{}",
        output.code,
        output.text
    );
    serde_json::from_str(output.text.trim_start_matches('\u{feff}').trim()).with_context(|| {
        format!(
            "Windowsからの応答を解釈できません: {}",
            output.text.chars().take(1200).collect::<String>()
        )
    })
}
pub fn winget(args: &[&str]) -> Result<ProcessOutput> {
    let alias = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Microsoft/WindowsApps/winget.exe");
    let mut cmd = Command::new(if alias.exists() {
        alias
    } else {
        PathBuf::from("winget.exe")
    });
    cmd.args(args);
    capture(cmd).context("WinGetを起動できません。環境確認でWinGetを準備してください")
}
fn capture(mut cmd: Command) -> Result<ProcessOutput> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let result = cmd.stdin(Stdio::null()).output()?;
    let mut text = String::from_utf8_lossy(&result.stdout).to_string();
    if !result.stderr.is_empty() {
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&result.stderr));
    }
    Ok(ProcessOutput {
        code: result.status.code().unwrap_or(-1) as u32,
        text: clean_terminal(&text),
    })
}
fn system_exe(name: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
        .join("System32")
        .join(name)
}
fn powershell() -> PathBuf {
    system_exe("WindowsPowerShell/v1.0/powershell.exe")
}
pub fn open_url(url: &str) -> Result<()> {
    validate_https(url)?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
        let u: Vec<_> = std::ffi::OsStr::new(url)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let verb: Vec<_> = "open".encode_utf16().chain(Some(0)).collect();
        ensure!(
            unsafe {
                ShellExecuteW(
                    std::ptr::null_mut(),
                    verb.as_ptr(),
                    u.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                )
            } as isize
                > 32,
            "ブラウザーを開けません"
        );
        Ok(())
    }
    #[cfg(not(windows))]
    {
        bail!("Windows専用機能です")
    }
}
pub fn open_folder(path: &Path) -> Result<()> {
    Command::new(system_exe("explorer.exe")).arg(path).spawn()?;
    Ok(())
}
pub fn verify_hash(path: &Path, expected: &str) -> Result<()> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hasher.update(&bytes[..n]);
    }
    ensure!(
        format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(expected),
        "SHA-256が一致しません。実行を中止しました"
    );
    Ok(())
}
pub fn clean_terminal(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        } else if c == '\r' {
            if chars.peek() != Some(&'\n') {
                out.push('\n');
            }
        } else if c == '\n' || c == '\t' || !c.is_control() {
            out.push(c);
        }
    }
    out
}

pub fn parse_search(text: &str) -> Result<Vec<AppEntry>> {
    use unicode_width::UnicodeWidthChar;
    // Headers define terminal display columns, not byte offsets or runs of two spaces.
    // A longest cell is padded by just one space; Japanese names occupy two columns.
    let lines: Vec<_> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let separator = lines
        .iter()
        .position(|l| l.trim().starts_with("-----"))
        .context("検索結果の形式が変わりました。WinGetを更新してください")?;
    ensure!(separator > 0, "検索結果の見出しがありません");
    let header = lines[separator - 1];
    let mut starts = Vec::new();
    let mut column = 0;
    let mut in_word = false;
    for c in header.chars() {
        if !c.is_whitespace() && !in_word {
            starts.push(column);
        }
        in_word = !c.is_whitespace();
        column += c.width().unwrap_or(0);
    }
    ensure!(starts.len() >= 3, "検索結果の列を認識できません");
    let mut entries = Vec::new();
    for row in lines.iter().skip(separator + 1) {
        let mut fields = vec![String::new(); starts.len()];
        let mut col = 0;
        for c in row.chars() {
            if let Some(i) = starts.iter().rposition(|s| *s <= col) {
                fields[i].push(c);
            }
            col += c.width().unwrap_or(0);
        }
        let fields: Vec<_> = fields.into_iter().map(|f| f.trim().to_string()).collect();
        let id = &fields[1];
        ensure!(
            !id.contains('…') && !id.contains("...") && valid_winget_id(id),
            "検索結果のIDが省略または不正です: {id}。WinGetを更新して一覧を再取得してください"
        );
        let app = AppEntry::from_winget(&fields[0], id, &fields[2])?;
        entries.push(app);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publisher_link_prefers_homepage_and_rejects_insecure_or_credential_urls() {
        assert_eq!(
            publisher_url(
                "Publisher Url: https://vendor.example/\nHomepage: https://app.example/download\nLicense Url: https://license.example"
            ),
            Some("https://app.example/download".into())
        );
        assert_eq!(
            publisher_url(
                "Homepage: http://insecure.example/\nPublisher Url: https://user:secret@vendor.example"
            ),
            None
        );
    }
    #[test]
    fn packaged_service_privilege_errors_wait_for_the_user() {
        for (code, text) in [
            (0x80073D28, ""),
            (0x8A150104, "AddPackage failed: HRESULT 0x80073D28."),
            (0x8A150104, "エラー: (0x80073d28)"),
            (1, "ERROR_PACKAGED_SERVICE_REQUIRES_ADMIN_PRIVILEGES"),
        ] {
            let outcome = winget_failure(&ProcessOutput {
                code,
                text: text.into(),
            })
            .unwrap();
            assert_eq!(outcome.status, Status::AwaitingUser);
            assert!(!outcome.status.satisfies_dependency());
            assert!(outcome.message.contains("0x80073D28"));
            assert!(outcome.message.contains(&format!("0x{code:08X}")));
        }
    }
    #[test]
    fn successful_and_unrelated_winget_outputs_are_not_privilege_failures() {
        assert!(
            winget_failure(&ProcessOutput {
                code: 0,
                text: "Previous error was 0x80073D28; the retry succeeded".into(),
            })
            .is_none()
        );
        for text in ["0x80073D02", "0x80073D280", "prefix0x80073D28", ""] {
            let outcome = winget_failure(&ProcessOutput {
                code: 0x8A150104,
                text: text.into(),
            })
            .unwrap();
            assert_eq!(outcome.status, Status::Failed);
        }
    }
    #[cfg(windows)]
    #[test]
    fn powershell_bridge_passes_data_without_evaluating_it() {
        let text = "日本語 ' \" ; $(Get-Date) ` & | \\n";
        let response: String = ps_json(
            "$payload.text | ConvertTo-Json -Compress",
            &serde_json::json!({"text":text}),
        )
        .unwrap();
        assert_eq!(response, text);
    }
    #[cfg(windows)]
    #[test]
    fn driver_filter_uses_pnp_class_instead_of_generic_wua_category() {
        let mock = r#"
$u = [pscustomobject]@{ DriverClass='OtherHardware'; DriverHardwareID='PCI\VEN_8086&DEV_1234'; DriverProvider='Intel'; IsBeta=$false; Title='Intel Net'; Identity=[pscustomobject]@{UpdateID='8cd6090a-c2fe-4995-9d3b-82c548bf4067';RevisionNumber=1}; DriverModel='LAN'; BrowseOnly=$true; IsAssigned=$false; EulaText='terms'; InstallationBehavior=[pscustomobject]@{CanRequestUserInput=$true} }
$devices = @(@{Name='LAN'; Class='Net'; IDs=@('PCI\VEN_8086&DEV_1234'); Version='1'})
$allowed = $null -ne (Get-EligibleDriver $u $devices)
$u.Title = 'UEFI Firmware'
$firmware = $null -ne (Get-EligibleDriver $u $devices)
$u.Title = 'Intel Net'; $devices[0].Class = 'Firmware'
$firmwareClass = $null -ne (Get-EligibleDriver $u $devices)
$devices[0].Class = 'Net'; $devices[0].IDs = @('different hardware')
$mismatch = $null -ne (Get-EligibleDriver $u $devices)
@{allowed=$allowed;firmware=$firmware;firmware_class=$firmwareClass;mismatch=$mismatch} | ConvertTo-Json -Compress
"#;
        let script = format!("{}\n{mock}", include_str!("../scripts/driver-common.ps1"));
        let response: serde_json::Value = ps_json(&script, &serde_json::json!({})).unwrap();
        assert_eq!(response["allowed"], true);
        assert_eq!(response["firmware"], false);
        assert_eq!(response["firmware_class"], false);
        assert_eq!(response["mismatch"], false);
    }
    #[cfg(windows)]
    #[test]
    fn embedded_windows_scripts_have_valid_syntax() {
        for script in [
            include_str!("../scripts/tag-index.ps1"),
            include_str!("../scripts/windows-apps.ps1"),
            include_str!("../scripts/environment.ps1"),
            include_str!("../scripts/installed-names.ps1"),
            include_str!("../scripts/driver-common.ps1"),
            include_str!("../scripts/driver-scan.ps1"),
            include_str!("../scripts/driver-install.ps1"),
            include_str!("../scripts/bootstrap.ps1"),
        ] {
            let parsed: serde_json::Value = ps_json("$tokens=$null; $syntaxErrors=$null; $null=[System.Management.Automation.Language.Parser]::ParseInput($payload.script,[ref]$tokens,[ref]$syntaxErrors); @{ errors=@($syntaxErrors | ForEach-Object { $_.Message }) } | ConvertTo-Json -Depth 3 -Compress", &serde_json::json!({"script":script})).unwrap();
            assert_eq!(parsed["errors"].as_array().unwrap().len(), 0, "{parsed}");
        }
    }
    #[test]
    fn windows_arguments_preserve_quotes_and_trailing_backslashes() {
        assert_eq!(
            quote_windows_arg("C:\\Program Files\\"),
            "\"C:\\Program Files\\\\\""
        );
        assert_eq!(quote_windows_arg("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote_windows_arg(""), "\"\"");
    }
    #[test]
    fn japanese_and_english_search_tables_work_without_header_matching() {
        let rows=parse_search("名前            ID                バージョン\n\n------------------------------------------------\n日本語 アプリ   Vendor.App        1.0\nGoogle Chrome   Google.Chrome     100\n").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "日本語 アプリ");
        assert!(parse_search("unexpected format").is_err());
        assert!(parse_search("Name            Id                Version\n------------------------------------------------\nTruncated       Vendor.Thi…       1\n").is_err());
        let narrow = parse_search(
            "Name  Id        Version\n-----------------------\n7-Zip 7zip.7zip 26.05\n",
        )
        .unwrap();
        assert_eq!(narrow[0].name, "7-Zip");
    }
    #[test]
    fn hash_mismatch_never_executes() {
        let p = std::env::temp_dir().join(format!("{}.exe", uuid::Uuid::new_v4()));
        fs::write(&p, b"test").unwrap();
        assert!(verify_hash(&p, &"0".repeat(64)).is_err());
        assert!(
            verify_hash(
                &p,
                "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
            )
            .is_ok()
        );
        fs::remove_file(p).unwrap();
    }
}
