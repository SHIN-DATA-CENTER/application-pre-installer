use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA_VERSION: u32 = 1;
pub const UNCLASSIFIED: &str = "未分類";
pub const APP_CATEGORIES: [&str; 4] = [
    "一般的なアプリケーション",
    "ゲーム系アプリケーション",
    "クリエイターアプリケーション",
    "デベロッパーアプリケーション",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema_version: u32,
    pub apps: Vec<AppEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    pub category: String,
    #[serde(default)]
    pub subcategory: String,
    pub description: String,
    pub publisher: String,
    pub website: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub method: InstallMethod,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InstallMethod {
    Winget {
        package_id: String,
        source: String,
    },
    Manual {
        url: String,
    },
    Custom {
        location: String,
        args: Vec<String>,
        sha256: Option<String>,
    },
    Unavailable {
        reason: String,
    },
}

impl AppEntry {
    pub fn from_winget(name: &str, package_id: &str, version: &str) -> Result<Self> {
        ensure!(
            valid_winget_id(package_id),
            "WinGetのIDが不正です: {package_id}"
        );
        let mut website =
            url::Url::parse("https://github.com/microsoft/winget-pkgs/tree/master/manifests/")?;
        {
            let mut path = website
                .path_segments_mut()
                .map_err(|_| anyhow::anyhow!("URLを作成できません"))?;
            path.pop_if_empty().push(
                &package_id
                    .chars()
                    .next()
                    .unwrap()
                    .to_lowercase()
                    .to_string(),
            );
            for part in package_id.split('.') {
                path.push(part);
            }
        }
        let app = Self {
            id: format!("search-{package_id}"),
            name: name.to_owned(),
            category: UNCLASSIFIED.into(),
            subcategory: "WinGet".into(),
            description: format!(
                "提供バージョン: {version} / 提供元・利用条件は詳細で確認できます。"
            ),
            publisher: "詳細で確認".into(),
            website: website.into(),
            dependencies: vec![],
            method: InstallMethod::Winget {
                package_id: package_id.into(),
                source: "winget".into(),
            },
        };
        app.validate()?;
        Ok(app)
    }
    pub fn source_label(&self) -> String {
        match &self.method {
            InstallMethod::Winget { package_id, source } => {
                format!("WinGet / {source} / {package_id}")
            }
            InstallMethod::Manual { url } => format!("手動操作 / {url}"),
            InstallMethod::Custom { location, .. } => format!("独自登録 / {location}"),
            InstallMethod::Unavailable { reason } => format!("未確定 / {reason}"),
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            APP_CATEGORIES.contains(&self.category.as_str()) || self.category == UNCLASSIFIED,
            "未対応のカテゴリーです: {}",
            self.category
        );
        ensure!(
            !self.id.is_empty() && self.id.len() <= 520,
            "項目IDが不正です"
        );
        ensure!(
            !self.name.trim().is_empty() && self.name.chars().count() <= 500,
            "アプリ名が不正です"
        );
        ensure!(self.description.len() <= 8000, "説明が長すぎます");
        if self.website.is_empty() {
            ensure!(
                matches!(self.method, InstallMethod::Custom { .. }),
                "公式サイトが未指定です"
            );
        } else {
            validate_https(&self.website)?;
        }
        match &self.method {
            InstallMethod::Winget { package_id, source } => {
                ensure!(
                    matches!(source.as_str(), "winget" | "msstore"),
                    "未対応のWinGetソースです"
                );
                ensure!(
                    if source == "winget" {
                        valid_winget_id(package_id)
                    } else {
                        !package_id.is_empty()
                            && package_id.len() <= 200
                            && package_id.chars().all(|c| c.is_ascii_alphanumeric())
                    },
                    "パッケージIDが不正です"
                );
            }
            InstallMethod::Manual { url } => {
                validate_https(url)?;
            }
            InstallMethod::Custom {
                location,
                args,
                sha256,
            } => {
                ensure!(location.len() <= 8192, "パスが長すぎます");
                let filename = if location.starts_with("https://") {
                    let url = validate_https(location)?;
                    url.path_segments()
                        .and_then(|mut p| p.next_back())
                        .unwrap_or("")
                        .to_string()
                } else {
                    ensure!(
                        !location.contains("://"),
                        "ダウンロードURLはHTTPSのみ対応します"
                    );
                    std::path::Path::new(location)
                        .file_name()
                        .and_then(|x| x.to_str())
                        .unwrap_or("")
                        .to_owned()
                };
                validate_installer_name(&filename)?;
                ensure!(
                    args.len() <= 64 && args.iter().all(|a| a.len() < 8192 && !a.contains('\0')),
                    "引数が不正です"
                );
                if let Some(hash) = sha256 {
                    ensure!(
                        hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()),
                        "SHA-256は64桁の16進数です"
                    );
                }
            }
            InstallMethod::Unavailable { .. } => {}
        }
        Ok(())
    }
}

pub fn valid_winget_id(id: &str) -> bool {
    let parts: Vec<_> = id.split('.').collect();
    id.chars().count() <= 128
        && (2..=8).contains(&parts.len())
        && parts.iter().all(|p| (1..=32).contains(&p.chars().count()))
        && id
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && !"\\/:*?\"<>|".contains(c))
}

pub fn validate_https(raw: &str) -> Result<url::Url> {
    let u = url::Url::parse(raw).context("URLが不正です")?;
    ensure!(
        u.scheme() == "https"
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none(),
        "認証情報を含まないHTTPS URLを指定してください"
    );
    Ok(u)
}

pub fn validate_installer_name(filename: &str) -> Result<()> {
    let name = filename.to_ascii_lowercase();
    ensure!(
        name.ends_with(".exe") || name.ends_with(".msi"),
        "登録できるのは.exeまたは.msiです（直接ダウンロードURLを指定）"
    );
    ensure!(
        ![
            "cmd.exe",
            "powershell.exe",
            "pwsh.exe",
            "wscript.exe",
            "cscript.exe",
            "mshta.exe",
            "rundll32.exe",
            "regsvr32.exe",
            "msiexec.exe"
        ]
        .contains(&name.as_str()),
        "コマンド実行プログラムは登録できません"
    );
    Ok(())
}

impl Catalog {
    pub fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            apps: vec![],
        }
    }
    #[cfg(test)]
    pub fn fixture() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            apps: (0..3)
                .map(|i| {
                    AppEntry::from_winget(&format!("Test {i}"), &format!("Test.App{i}"), "1.0")
                        .unwrap()
                })
                .collect(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.apps.len() <= 100_000, "候補が多すぎます");
        ensure!(
            self.schema_version == SCHEMA_VERSION,
            "未対応のカタログ形式です"
        );
        let mut keys = BTreeSet::new();
        for app in &self.apps {
            app.validate().with_context(|| format!("候補 {}", app.id))?;
            ensure!(keys.insert(&app.id), "候補IDが重複しています: {}", app.id);
        }
        self.ordered(&self.apps.iter().map(|a| a.id.clone()).collect())?;
        Ok(())
    }

    pub fn ordered(&self, selected: &BTreeSet<String>) -> Result<Vec<AppEntry>> {
        fn visit(
            id: &str,
            entries: &BTreeMap<&str, &AppEntry>,
            visiting: &mut BTreeSet<String>,
            done: &mut BTreeSet<String>,
            out: &mut Vec<AppEntry>,
        ) -> Result<()> {
            if done.contains(id) {
                return Ok(());
            }
            ensure!(visiting.len() < 256, "依存関係の階層が深すぎます");
            ensure!(
                visiting.insert(id.to_owned()),
                "依存関係が循環しています: {id}"
            );
            let app = entries
                .get(id)
                .with_context(|| format!("候補が存在しません: {id}"))?;
            for dep in &app.dependencies {
                visit(dep, entries, visiting, done, out)?;
            }
            visiting.remove(id);
            done.insert(id.to_owned());
            out.push((*app).clone());
            Ok(())
        }
        let entries: BTreeMap<_, _> = self.apps.iter().map(|a| (a.id.as_str(), a)).collect();
        let (mut visiting, mut done, mut out) = (BTreeSet::new(), BTreeSet::new(), Vec::new());
        for id in selected {
            visit(id, &entries, &mut visiting, &mut done, &mut out)?;
        }
        Ok(out)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub schema_version: u32,
    pub selected_apps: BTreeSet<String>,
    pub custom_apps: Vec<AppEntry>,
    pub update_existing: bool,
}

impl Profile {
    pub fn capture(catalog: &Catalog, selected: &BTreeSet<String>, update_existing: bool) -> Self {
        let required: BTreeSet<_> = catalog
            .ordered(selected)
            .map(|apps| apps.into_iter().map(|a| a.id).collect())
            .unwrap_or_else(|_| selected.clone());
        Self {
            schema_version: SCHEMA_VERSION,
            selected_apps: selected.clone(),
            custom_apps: catalog
                .apps
                .iter()
                .filter(|a| a.id.starts_with("custom-") || required.contains(&a.id))
                .cloned()
                .collect(),
            update_existing,
        }
    }
    pub fn validate(&self, bundled: &Catalog) -> Result<Catalog> {
        ensure!(
            self.schema_version == SCHEMA_VERSION,
            "未対応のプロファイル形式です"
        );
        ensure!(
            self.custom_apps.len() <= 25_000 && self.selected_apps.len() <= 25_000,
            "項目が多すぎます"
        );
        let mut catalog = bundled.clone();
        for app in &self.custom_apps {
            ensure!(
                app.id.starts_with("custom-") || app.id.starts_with("search-"),
                "独自項目のIDが不正です"
            );
            let mut app = app.clone();
            if !APP_CATEGORIES.contains(&app.category.as_str()) && app.category != UNCLASSIFIED {
                // Preserve registrations made by the initial preview's more detailed categories.
                app.subcategory = app.category;
                app.category = APP_CATEGORIES[0].into();
            }
            if app.id.starts_with("search-") {
                let InstallMethod::Winget { package_id, source } = &app.method else {
                    bail!("検索項目の導入方法を変更することはできません");
                };
                ensure!(
                    (source == "winget" && app.id == format!("search-{package_id}"))
                        || (source == "msstore"
                            && app.id == format!("search-msstore-{package_id}")),
                    "検索項目のIDとパッケージIDが一致しません"
                );
                if let Some(existing) = catalog.apps.iter().find(|a| a.id == app.id) {
                    ensure!(
                        matches!(&existing.method, InstallMethod::Winget { package_id: p, source: s } if p == package_id && s == source),
                        "既存候補の導入方法は上書きできません"
                    );
                    continue;
                }
            }
            catalog.apps.push(app);
        }
        catalog.validate()?;
        let ordered = catalog.ordered(&self.selected_apps)?;
        ensure!(
            ordered
                .iter()
                .all(|a| !matches!(a.method, InstallMethod::Unavailable { .. })),
            "未確定の項目は選択できません"
        );
        Ok(catalog)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Environment {
    pub os: String,
    pub build: u32,
    pub architecture: String,
    pub windows_client: bool,
    pub network_available: bool,
    pub manufacturer: String,
    pub model: String,
    pub board_manufacturer: String,
    pub board_product: String,
    pub board_revision: String,
    pub winget: String,
    #[serde(default)]
    pub devices: Vec<Device>,
    #[serde(default)]
    pub installed: Vec<InstalledApp>,
    #[serde(default)]
    pub warnings: Vec<String>,
}
impl Environment {
    pub fn supported(&self) -> bool {
        self.build >= 22000 && self.architecture == "x64" && self.windows_client
    }
    pub fn summary(&self) -> String {
        let mut text = format!(
            "{} / {} / Build {}\n{} {}\nマザーボード: {} {} / Rev. {}\nWinGet: {}\n検出デバイス: {} 件 / インストール済み: {} 件{}",
            self.os,
            self.architecture,
            self.build,
            self.manufacturer,
            self.model,
            self.board_manufacturer,
            self.board_product,
            self.board_revision,
            if self.winget.is_empty() {
                "未準備"
            } else {
                &self.winget
            },
            self.devices.len(),
            self.installed.len(),
            if self.warnings.is_empty() {
                String::new()
            } else {
                format!("\n\n注意: {}", self.warnings.join("\n"))
            }
        );
        text.push_str(if self.network_available {
            "\nネットワーク接続あり（取得先への到達性は処理時に確認）"
        } else {
            "\nネットワーク接続がありません。接続後に再確認してください。"
        });
        text.push_str("\n\n問題状態のデバイス:\n");
        let problems: Vec<_> = self
            .devices
            .iter()
            .filter(|d| d.problem_code != 0)
            .collect();
        if problems.is_empty() {
            text.push_str("問題コードのあるデバイスは検出されていません。\n");
        }
        for device in problems {
            text.push_str(&format!(
                "{} / Code {} / {}\n",
                device.name,
                device.problem_code,
                device.hardware_ids.join(", ")
            ));
        }
        text.push_str("\n検出したデバイスと現在のドライバー:\n");
        for device in &self.devices {
            text.push_str(&format!(
                "{} [{}]\n  {} / {}\n  {}\n",
                device.name,
                device.class,
                device.provider,
                device.version,
                device.hardware_ids.join(", ")
            ));
        }
        text
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    pub instance_id: String,
    pub class: String,
    pub hardware_ids: Vec<String>,
    pub problem_code: i32,
    pub version: String,
    pub provider: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InstalledApp {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DriverCandidate {
    pub update_id: String,
    pub revision: i32,
    pub title: String,
    pub class: String,
    pub hardware_id: String,
    pub provider: String,
    pub model: String,
    pub current_version: String,
    pub optional: bool,
    pub hardware_match: bool,
    pub eula: String,
}
impl DriverCandidate {
    pub fn key(&self) -> String {
        format!("{}:{}", self.update_id, self.revision)
    }
    pub fn eligible(&self) -> bool {
        const CLASSES: &[&str] = &[
            "audioendpoint",
            "audioprocessingobject",
            "bluetooth",
            "camera",
            "display",
            "extension",
            "hdc",
            "hidclass",
            "image",
            "keyboard",
            "media",
            "modem",
            "monitor",
            "mouse",
            "net",
            "printer",
            "scsiadapter",
            "securitydevices",
            "sensor",
            "smartcardreader",
            "softwarecomponent",
            "sound",
            "system",
            "usb",
            "usbdevice",
        ];
        let title = self.title.to_lowercase();
        let hwid = self.hardware_id.to_lowercase();
        self.hardware_match
            && !self.provider.trim().is_empty()
            && uuid::Uuid::parse_str(&self.update_id).is_ok()
            && self.revision > 0
            && !hwid.is_empty()
            && !hwid.contains("uefi")
            && !hwid.contains("firmware")
            && CLASSES.contains(&self.class.to_ascii_lowercase().as_str())
            && !["firmware", "bios", "uefi", "ファームウェア"]
                .iter()
                .any(|s| title.contains(s))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Running,
    Success,
    Failed,
    Skipped,
    AwaitingUser,
    RebootRequired,
    Unknown,
}
impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pending => "待機",
            Self::Running => "実行中",
            Self::Success => "成功",
            Self::Failed => "失敗",
            Self::Skipped => "スキップ",
            Self::AwaitingUser => "利用者の操作待ち",
            Self::RebootRequired => "再起動待ち",
            Self::Unknown => "確認不能",
        }
    }
    pub fn satisfies_dependency(&self) -> bool {
        matches!(self, Self::Success | Self::Skipped)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub status: Status,
    pub message: String,
}
impl Outcome {
    pub fn new(status: Status, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    App { app: AppEntry },
    Driver { driver: DriverCandidate },
    Remove { app: crate::uninstall::RemovableApp },
}
impl Target {
    pub fn key(&self) -> String {
        match self {
            Self::App { app } => app.id.clone(),
            Self::Driver { driver } => format!("driver-{}", driver.key()),
            Self::Remove { app } => app.key(),
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Self::App { app } => &app.name,
            Self::Driver { driver } => &driver.title,
            Self::Remove { app } => app.title(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub target: Target,
    pub outcome: Outcome,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub schema_version: u32,
    pub id: String,
    pub created_at: u64,
    pub update_existing: bool,
    pub tasks: Vec<Task>,
}
impl Session {
    #[cfg(test)]
    pub fn new(
        apps: Vec<AppEntry>,
        drivers: Vec<DriverCandidate>,
        update_existing: bool,
    ) -> Result<Self> {
        Self::with_removals(apps, drivers, vec![], update_existing)
    }
    pub fn with_removals(
        apps: Vec<AppEntry>,
        drivers: Vec<DriverCandidate>,
        removals: Vec<crate::uninstall::RemovableApp>,
        update_existing: bool,
    ) -> Result<Self> {
        for app in &apps {
            app.validate()?;
            ensure!(
                !matches!(app.method, InstallMethod::Unavailable { .. }),
                "未確定の項目は実行できません"
            );
        }
        ensure!(
            drivers.iter().all(DriverCandidate::eligible),
            "適合性不明または対象外のドライバーが含まれます"
        );
        ensure!(
            removals
                .iter()
                .all(crate::uninstall::RemovableApp::eligible),
            "対象外の削除候補が含まれています"
        );
        let targets = removals
            .into_iter()
            .map(|app| Target::Remove { app })
            .chain(
                drivers
                    .into_iter()
                    .map(|driver| Target::Driver { driver })
                    .chain(apps.into_iter().map(|app| Target::App { app })),
            );
        let tasks: Vec<_> = targets
            .map(|target| Task {
                target,
                outcome: Outcome::new(Status::Pending, "未実行"),
            })
            .collect();
        ensure!(!tasks.is_empty(), "実行する対象を選択してください");
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            created_at: timestamp(),
            update_existing,
            tasks,
        })
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == SCHEMA_VERSION && self.tasks.len() <= 25_000,
            "履歴の形式が不正です"
        );
        uuid::Uuid::parse_str(&self.id)?;
        let mut keys = BTreeSet::new();
        for task in &self.tasks {
            ensure!(
                keys.insert(task.target.key()),
                "履歴内の項目が重複しています"
            );
            match &task.target {
                Target::App { app } => app.validate()?,
                Target::Driver { driver } if !driver.eligible() => {
                    bail!("履歴に対象外ドライバーが含まれます")
                }
                Target::Remove { app } => {
                    ensure!(app.eligible(), "履歴に対象外の削除候補が含まれます");
                }
                _ => {}
            }
        }
        Ok(())
    }
}
pub fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_catalog_profiles_save_only_selection_dependencies_and_custom_entries() {
        let mut catalog = Catalog {
            schema_version: SCHEMA_VERSION,
            apps: (0..10_000)
                .map(|i| {
                    AppEntry::from_winget(&format!("App {i}"), &format!("Test.App{i}"), "1")
                        .unwrap()
                })
                .collect(),
        };
        catalog.apps[0].dependencies = vec![catalog.apps[5].id.clone()];
        let selected = BTreeSet::from([catalog.apps[0].id.clone(), catalog.apps[9999].id.clone()]);
        let profile = Profile::capture(&catalog, &selected, false);
        assert_eq!(profile.custom_apps.len(), 3);
        let json = serde_json::to_vec(&profile).unwrap();
        assert!(json.len() < 10_000);
        let restored: Profile = serde_json::from_slice(&json).unwrap();
        assert_eq!(
            restored
                .validate(&Catalog::empty())
                .unwrap()
                .ordered(&restored.selected_apps)
                .unwrap()
                .len(),
            3
        );
        assert_eq!(restored.validate(&catalog).unwrap().apps.len(), 10_000);
        let mut tampered = restored.clone();
        tampered.custom_apps[0].name = "Changed profile label".into();
        assert_eq!(tampered.validate(&catalog).unwrap().apps[0].name, "App 0");
    }
    #[test]
    fn custom_categories_roundtrip_and_legacy_registration_is_preserved() {
        let bundled = Catalog::fixture();
        let mut app = bundled.apps[0].clone();
        app.id = "custom-example".into();
        app.category = APP_CATEGORIES[3].into();
        app.subcategory = "独自登録".into();
        app.website = String::new();
        app.method = InstallMethod::Custom {
            location: "C:\\setup.exe".into(),
            args: vec![],
            sha256: None,
        };
        let profile = Profile {
            schema_version: 1,
            selected_apps: BTreeSet::from([app.id.clone()]),
            custom_apps: vec![app],
            update_existing: false,
        };
        let mut restored: Profile =
            serde_json::from_str(&serde_json::to_string(&profile).unwrap()).unwrap();
        let c = restored.validate(&bundled).unwrap();
        assert_eq!(c.apps.last().unwrap().category, APP_CATEGORIES[3]);
        restored.custom_apps[0].category = "独自登録".into();
        let c = restored.validate(&bundled).unwrap();
        assert_eq!(c.apps.last().unwrap().category, APP_CATEGORIES[0]);
        assert_eq!(c.apps.last().unwrap().subcategory, "独自登録");
        assert_eq!(restored.selected_apps.len(), 1);
    }
    fn driver() -> DriverCandidate {
        DriverCandidate {
            update_id: uuid::Uuid::new_v4().to_string(),
            revision: 1,
            title: "Intel - Net".into(),
            class: "Net".into(),
            hardware_id: "PCI\\VEN_8086".into(),
            provider: "Intel".into(),
            model: "LAN".into(),
            current_version: "".into(),
            optional: true,
            hardware_match: true,
            eula: "".into(),
        }
    }
    #[test]
    fn firmware_and_unknown_devices_are_never_eligible() {
        let mut d = driver();
        assert!(d.eligible());
        d.title = "System Firmware".into();
        assert!(!d.eligible());
        d.title = "Intel Net".into();
        d.class = "Firmware".into();
        assert!(!d.eligible());
        d.class = "Net".into();
        d.hardware_match = false;
        assert!(!d.eligible());
        d.hardware_match = true;
        d.hardware_id = "UEFI\\RES".into();
        assert!(!d.eligible());
    }
    #[test]
    fn profiles_cannot_smuggle_drivers_or_override_bundled_entries() {
        let c = Catalog::fixture();
        let raw = r#"{"schema_version":1,"selected_apps":[],"custom_apps":[],"update_existing":false,"drivers":[]}"#;
        assert!(serde_json::from_str::<Profile>(raw).is_err());
        let mut p = Profile {
            schema_version: 1,
            ..Default::default()
        };
        p.custom_apps.push(c.apps[0].clone());
        p.custom_apps[0].method = InstallMethod::Winget {
            package_id: "Other.Package".into(),
            source: "winget".into(),
        };
        assert!(p.validate(&c).is_err());
    }
    #[test]
    fn dependency_order_cycle_and_missing_are_checked() {
        let mut c = Catalog::fixture();
        let a = c.apps[0].id.clone();
        let b = c.apps[1].id.clone();
        c.apps[0].dependencies = vec![b.clone()];
        let plan = c.ordered(&BTreeSet::from([a.clone()])).unwrap();
        assert_eq!(plan[0].id, b);
        assert_eq!(plan[1].id, a);
        c.apps[1].dependencies = vec![a.clone()];
        assert!(c.validate().is_err());
        c.apps[1].dependencies = vec!["missing".into()];
        assert!(c.validate().is_err());
    }
    #[test]
    fn custom_inputs_reject_scripts_credentials_and_bad_hashes() {
        assert!(validate_https("http://example.com/x.exe").is_err());
        assert!(validate_https("https://user:pass@example.com/x.exe").is_err());
        assert!(validate_installer_name("setup.ps1").is_err());
        assert!(validate_installer_name("POWERSHELL.EXE").is_err());
        assert!(validate_installer_name("setup.exe").is_ok());
        assert!(validate_installer_name("setup.msi").is_ok());
    }
}
