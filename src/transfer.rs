use crate::{
    model::{AppEntry, Catalog, InstallMethod, Profile, SCHEMA_VERSION, UNCLASSIFIED},
    platform::{Backend, ps_json, winget},
    store::{read_json, write_json},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// A reviewed document, never an instruction to run `winget import` directly.
pub struct Transfer {
    pub profile: Profile,
    pub document: serde_json::Value,
    pub report: String,
    pub detected: Vec<DetectedSoftware>,
    pub name_matches: BTreeMap<usize, AppEntry>,
    match_origin: Option<(serde_json::Value, String)>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DetectedSoftware {
    pub name: String,
    pub version: String,
    pub publisher: String,
}
#[derive(Deserialize)]
struct NameInventory {
    apps: Vec<DetectedSoftware>,
    warnings: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct Packages {
    #[serde(rename = "$schema", default)]
    schema: Option<String>,
    #[serde(default)]
    creation_date: Option<String>,
    #[serde(default)]
    win_get_version: Option<String>,
    sources: Vec<Source>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct Source {
    source_details: SourceDetails,
    packages: Vec<Package>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct SourceDetails {
    name: String,
    identifier: String,
    argument: String,
    #[serde(rename = "Type")]
    kind: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct Package {
    package_identifier: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    initial_override_arguments: Option<String>,
    #[serde(default)]
    initial_custom_switches: Option<String>,
}

impl Transfer {
    pub fn read(path: &Path, catalog: &Catalog) -> Result<Self> {
        Self::parse(read_json(path)?, catalog)
    }

    pub fn parse(mut document: serde_json::Value, catalog: &Catalog) -> Result<Self> {
        let (mut profile, report) = if document.get("Sources").is_some() {
            let packages: Packages = serde_json::from_value(document.clone())
                .context("WinGetのエクスポート形式を読み込めません")?;
            ensure!(packages.sources.len() <= 2, "未対応のWinGetソース構成です");
            let mut apps = Vec::new();
            let mut ids = BTreeSet::new();
            let mut sources = BTreeSet::new();
            let mut versioned = false;
            for source in packages.sources {
                let details = source.source_details;
                ensure!(
                    canonical_source(&details) && sources.insert(details.name.clone()),
                    "Microsoft公式のwinget / msstoreソースのみ読み込めます。ソースの追加・変更は行いません"
                );
                ensure!(source.packages.len() <= 25_000, "項目が多すぎます");
                for package in source.packages {
                    ensure!(
                        package.channel.as_deref().is_none_or(str::is_empty),
                        "Channel指定のあるJSONは未対応です。製品・チャンネル別のパッケージIDで選択してください"
                    );
                    ensure!(
                        package
                            .scope
                            .as_deref()
                            .is_none_or(|s| matches!(s, "user" | "machine")),
                        "未対応のScope指定です"
                    );
                    ensure!(
                        package
                            .initial_override_arguments
                            .as_ref()
                            .is_none_or(|s| s.len() <= 32_768)
                            && package
                                .initial_custom_switches
                                .as_ref()
                                .is_none_or(|s| s.len() <= 32_768),
                        "追加引数が長すぎます"
                    );
                    versioned |= package.version.is_some();
                    let app = package_app(&details.name, &package.package_identifier, catalog)?;
                    ensure!(
                        ids.insert(app.id.clone()),
                        "同じパッケージIDが重複しています"
                    );
                    apps.push(app);
                }
            }
            // Do not carry arbitrary switches (or possibly private values) to another PC.
            if let Some(sources) = document.get_mut("Sources").and_then(|v| v.as_array_mut()) {
                for source in sources {
                    if let Some(packages) =
                        source.get_mut("Packages").and_then(|v| v.as_array_mut())
                    {
                        for package in packages {
                            if let Some(fields) = package.as_object_mut() {
                                fields.remove("InitialOverrideArguments");
                                fields.remove("InitialCustomSwitches");
                            }
                        }
                    }
                }
            }
            (
                Profile {
                    schema_version: SCHEMA_VERSION,
                    selected_apps: ids,
                    custom_apps: apps,
                    update_existing: false,
                },
                if versioned {
                    "WinGet形式。指定バージョンは固定せず、同じID・配布元の最新提供版を導入します。"
                } else {
                    "WinGet形式。同じID・配布元の最新提供版を導入します。"
                }
                .to_owned()
                    + "\n元のインストーラー追加引数は除外します。Scope指定は引き継がず、移行先では標準の導入範囲を使います。",
            )
        } else {
            (
                serde_json::from_value(document.clone())
                    .context("プロファイル形式を読み込めません")?,
                "Application Pre-Installer形式。独自登録のURL・引数も確認してください。".into(),
            )
        };
        // Importing a file is never consent to upgrade existing software.
        profile.update_existing = false;
        profile.validate(catalog)?;
        Ok(Self {
            profile,
            document,
            report,
            detected: vec![],
            name_matches: BTreeMap::new(),
            match_origin: None,
        })
    }

    pub fn selection(catalog: &Catalog, selected: &BTreeSet<String>) -> Result<Self> {
        let apps = catalog.ordered(selected)?;
        ensure!(!apps.is_empty(), "書き出すアプリを選択してください");
        let profile = Profile {
            schema_version: SCHEMA_VERSION,
            selected_apps: selected.clone(),
            custom_apps: apps,
            update_existing: false,
        };
        profile.validate(&Catalog::empty())?;
        let document = serde_json::to_value(&profile)?;
        Ok(Self {
            profile,
            document,
            report:
                "選択したアプリと依存項目だけを含みます。ドライバー・削除対象・同意は含みません。"
                    .into(),
            detected: vec![],
            name_matches: BTreeMap::new(),
            match_origin: None,
        })
    }

    pub fn exported_names(&self, catalog: &Catalog) -> BTreeSet<String> {
        let names: BTreeMap<_, _> = catalog
            .apps
            .iter()
            .map(|a| (a.id.as_str(), a.name.as_str()))
            .collect();
        self.profile
            .custom_apps
            .iter()
            .filter(|a| self.profile.selected_apps.contains(&a.id))
            .map(|a| normalize_name(names.get(a.id.as_str()).copied().unwrap_or(&a.name)))
            .collect()
    }

    pub fn add_name_match(
        &mut self,
        index: usize,
        app: &AppEntry,
        catalog: &Catalog,
    ) -> Result<()> {
        ensure!(index < self.detected.len(), "対象の登録名がありません");
        app.validate()?;
        ensure!(
            matches!(&app.method, InstallMethod::Winget { source, .. } if source == "winget"),
            "名前の補完にはWinGetコミュニティの候補を選んでください"
        );
        let mut matches = self.name_matches.clone();
        matches.insert(index, app.clone());
        self.rebuild_matches(matches, catalog)
    }

    pub fn remove_name_match(&mut self, index: usize, catalog: &Catalog) -> Result<()> {
        let mut matches = self.name_matches.clone();
        matches.remove(&index);
        self.rebuild_matches(matches, catalog)
    }

    fn rebuild_matches(
        &mut self,
        matches: BTreeMap<usize, AppEntry>,
        catalog: &Catalog,
    ) -> Result<()> {
        let (original, report) = self
            .match_origin
            .as_ref()
            .cloned()
            .unwrap_or_else(|| (self.document.clone(), self.report.clone()));
        let mut document = original.clone();
        for app in matches.values() {
            append_package(&mut document, app)?;
        }
        let rebuilt = Self::parse(document, catalog)?;
        // Commit only after the complete document passes validation.
        self.document = rebuilt.document;
        self.profile = rebuilt.profile;
        self.report = report.clone();
        if !matches.is_empty() {
            self.report
                .push_str("\n\n【名前から探して利用者が追加した対応】\n");
            for (index, app) in &matches {
                self.report.push_str(&format!(
                    "{} → {}\n",
                    self.detected[*index].name,
                    app.source_label()
                ));
            }
        }
        self.match_origin = Some((original, report));
        self.name_matches = matches;
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_json(path, &self.document)
    }

    pub fn preview(&self, catalog: &Catalog) -> Result<String> {
        let merged = self.profile.validate(catalog)?;
        let apps = merged.ordered(&self.profile.selected_apps)?;
        let mut text = format!("対象 {} 件（依存項目を含む）\n", apps.len());
        for app in &apps {
            text.push_str(&format!("\n{}\n  {}\n", app.name, app.source_label()));
            if let InstallMethod::Custom { args, sha256, .. } = &app.method {
                text.push_str(&format!(
                    "  引数: {}\n  SHA-256: {}\n",
                    serde_json::to_string(args)?,
                    sha256.as_deref().unwrap_or("未指定")
                ));
            }
        }
        text.push_str(&format!(
            "\n【形式・注意事項・読み取り結果】\n{}",
            self.report
        ));
        Ok(text)
    }

    pub fn missing_files(&self) -> Vec<(String, String)> {
        let entries: BTreeMap<_, _> = self
            .profile
            .custom_apps
            .iter()
            .map(|a| (a.id.as_str(), a))
            .collect();
        let mut required = self.profile.selected_apps.clone();
        let mut queue: Vec<_> = required.iter().cloned().collect();
        while let Some(id) = queue.pop() {
            if let Some(app) = entries.get(id.as_str()) {
                for dependency in &app.dependencies {
                    if required.insert(dependency.clone()) {
                        queue.push(dependency.clone());
                    }
                }
            }
        }
        self.profile
            .custom_apps
            .iter()
            .filter(|app| required.contains(&app.id))
            .filter_map(|app| match &app.method {
                InstallMethod::Custom { location, .. }
                    if !location.starts_with("https://") && !Path::new(location).is_file() =>
                {
                    Some((app.id.clone(), location.clone()))
                }
                _ => None,
            })
            .collect()
    }
}

pub fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn append_package(document: &mut serde_json::Value, app: &AppEntry) -> Result<()> {
    let InstallMethod::Winget { package_id, source } = &app.method else {
        anyhow::bail!("WinGet候補ではありません");
    };
    if let Some(sources) = document.get_mut("Sources") {
        let sources = sources.as_array_mut().context("Sourcesが不正です")?;
        let index = sources.iter().position(|s| s["SourceDetails"]["Name"].as_str() == Some(source.as_str())).unwrap_or_else(|| {
            sources.push(serde_json::json!({"SourceDetails": {"Name":"winget", "Identifier":"Microsoft.Winget.Source_8wekyb3d8bbwe", "Argument":"https://cdn.winget.microsoft.com/cache", "Type":"Microsoft.PreIndexed.Package"}, "Packages":[]}));
            sources.len() - 1
        });
        let packages = sources[index]["Packages"]
            .as_array_mut()
            .context("Packagesが不正です")?;
        if !packages.iter().any(|p| {
            p["PackageIdentifier"]
                .as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(package_id))
        }) {
            packages.push(serde_json::json!({"PackageIdentifier": package_id}));
        }
    } else {
        let mut profile: Profile = serde_json::from_value(document.clone())?;
        profile.selected_apps.insert(app.id.clone());
        if !profile.custom_apps.iter().any(|a| a.id == app.id) {
            profile.custom_apps.push(app.clone());
        }
        *document = serde_json::to_value(profile)?;
    }
    Ok(())
}

fn canonical_source(source: &SourceDetails) -> bool {
    match source.name.as_str() {
        "winget" => {
            source.identifier == "Microsoft.Winget.Source_8wekyb3d8bbwe"
                && source.kind == "Microsoft.PreIndexed.Package"
                && matches!(
                    source.argument.trim_end_matches('/'),
                    "https://cdn.winget.microsoft.com/cache" | "https://winget.azureedge.net/cache"
                )
        }
        "msstore" => {
            source.identifier == "StoreEdgeFD"
                && source.kind == "Microsoft.Rest"
                && source.argument.trim_end_matches('/')
                    == "https://storeedgefd.dsx.mp.microsoft.com/v9.0"
        }
        _ => false,
    }
}

fn package_app(source: &str, package_id: &str, catalog: &Catalog) -> Result<AppEntry> {
    if let Some(app) = catalog.apps.iter().find(|app|
        matches!(&app.method, InstallMethod::Winget { package_id: id, source: s } if id == package_id && s == source)) {
        return Ok(app.clone());
    }
    if source == "winget" {
        return AppEntry::from_winget(package_id, package_id, "最新提供版（移行時に確認）");
    }
    let app = AppEntry {
        id: format!("search-msstore-{package_id}"),
        name: format!("Microsoft Store / {package_id}"),
        category: UNCLASSIFIED.into(),
        subcategory: "Microsoft Store".into(),
        description: "インストール済みのStore IDから復元。製品名・提供元は詳細で確認してください。"
            .into(),
        publisher: "詳細で確認".into(),
        website: format!("https://apps.microsoft.com/detail/{package_id}"),
        dependencies: vec![],
        method: InstallMethod::Winget {
            package_id: package_id.into(),
            source: source.into(),
        },
    };
    app.validate()?;
    Ok(app)
}

impl Backend {
    pub fn export_installed(&self, catalog: &Catalog) -> Result<Transfer> {
        if self.demo {
            let mut transfer = Transfer::selection(
                &Catalog {
                    schema_version: SCHEMA_VERSION,
                    apps: vec![AppEntry::from_winget(
                        "デモ: インストール済みアプリ",
                        "Demo.InstalledApp",
                        "1.0",
                    )?],
                },
                &BTreeSet::from(["search-Demo.InstalledApp".into()]),
            )?;
            transfer.report = "デモの読み取り結果です。実際のPCは調査していません。\n識別できないソフト（デモ）は書き出し対象外です。".into();
            transfer.detected = vec![
                DetectedSoftware {
                    name: "Google Chrome".into(),
                    version: "1.0 (架空)".into(),
                    publisher: "デモの登録名".into(),
                },
                DetectedSoftware {
                    name: "Demo Editor (64-bit)".into(),
                    version: "2.0 (架空)".into(),
                    publisher: "デモの登録名".into(),
                },
            ];
            return Ok(transfer);
        }
        let path = self
            .store
            .root
            .join("cache")
            .join(format!("installed-{}.json", uuid::Uuid::new_v4()));
        let result = (|| {
            let output = winget(&[
                "export",
                "--output",
                path.to_str().context("保存先が不正です")?,
                "--accept-source-agreements",
                "--disable-interactivity",
            ])?;
            ensure!(
                output.code == 0,
                "インストール済みアプリの読み取りに失敗しました (0x{:08X})。WinGetの準備・接続・ソース設定を確認してください。\n{}",
                output.code,
                output.text
            );
            let mut transfer = Transfer::read(&path, catalog)?;
            match self.installed_names() {
                Ok(inventory) => {
                    transfer.detected = inventory.apps;
                    for warning in inventory.warnings {
                        transfer
                            .report
                            .push_str(&format!("\n登録名の検出: {warning}"));
                    }
                }
                Err(e) => transfer.report.push_str(&format!(
                    "\n登録名からの補完は利用できません。再度読み取ってください: {e:#}"
                )),
            }
            transfer.report.push_str("\n\n自動で照合できたアプリを書き出します。不足分は「名前から不足アプリを探す」で候補を確認して追加できます。\n設定・データ・ライセンス・ドライバー検出結果は移行しません。\n\n【WinGetの自動照合結果・警告（補完前）】\n");
            transfer.report.push_str(&output.text);
            Ok(transfer)
        })();
        let _ = std::fs::remove_file(&path);
        result
    }

    fn installed_names(&self) -> Result<NameInventory> {
        let mut inventory: NameInventory = ps_json(
            include_str!("../scripts/installed-names.ps1"),
            &serde_json::json!({}),
        )?;
        ensure!(inventory.apps.len() <= 25_000, "アプリ登録が多すぎます");
        let mut seen = BTreeSet::new();
        inventory.apps.retain(|a| {
            !a.name.trim().is_empty()
                && a.name.chars().count() <= 500
                && a.version.len() <= 512
                && a.publisher.len() <= 2000
                && seen.insert((
                    normalize_name(&a.name),
                    a.version.clone(),
                    a.publisher.to_lowercase(),
                ))
        });
        inventory
            .apps
            .sort_by_cached_key(|a| (normalize_name(&a.name), a.version.clone()));
        Ok(inventory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native() -> serde_json::Value {
        serde_json::json!({"$schema":"https://aka.ms/winget-packages.schema.2.0.json","WinGetVersion":"1.29.380","Sources":[
            {"SourceDetails":{"Name":"winget","Identifier":"Microsoft.Winget.Source_8wekyb3d8bbwe","Argument":"https://cdn.winget.microsoft.com/cache","Type":"Microsoft.PreIndexed.Package"},"Packages":[{"PackageIdentifier":"Test.Stable"},{"PackageIdentifier":"Test.Nightly","Version":"1.0"}]},
            {"SourceDetails":{"Name":"msstore","Identifier":"StoreEdgeFD","Argument":"https://storeedgefd.dsx.mp.microsoft.com/v9.0","Type":"Microsoft.Rest"},"Packages":[{"PackageIdentifier":"9ABCDE123456"}]}]})
    }
    #[test]
    fn installed_export_round_trip_retains_sources_and_channels_without_catalog() {
        let t = Transfer::parse(native(), &Catalog::empty()).unwrap();
        let merged = t.profile.validate(&Catalog::empty()).unwrap();
        assert_eq!(merged.ordered(&t.profile.selected_apps).unwrap().len(), 3);
        assert!(t.report.contains("固定せず"));
        assert!(
            matches!(&merged.apps[2].method, InstallMethod::Winget { source, package_id } if source == "msstore" && package_id == "9ABCDE123456")
        );
        assert!(!t.profile.update_existing);
        let p = serde_json::to_value(&t.profile).unwrap();
        assert_eq!(
            Transfer::parse(p, &Catalog::empty())
                .unwrap()
                .profile
                .selected_apps,
            t.profile.selected_apps
        );
    }
    #[test]
    fn foreign_sources_duplicates_invalid_ids_and_extra_instructions_are_rejected() {
        for field in ["Argument", "Name", "Identifier", "Type"] {
            let mut value = native();
            value["Sources"][0]["SourceDetails"][field] = "https://example.com/evil".into();
            assert!(Transfer::parse(value, &Catalog::empty()).is_err());
        }
        let mut value = native();
        value["Sources"][0]["Packages"][1] = value["Sources"][0]["Packages"][0].clone();
        assert!(Transfer::parse(value, &Catalog::empty()).is_err());
        let mut value = native();
        value["Sources"][1]["Packages"][0]["PackageIdentifier"] = "--silent".into();
        assert!(Transfer::parse(value, &Catalog::empty()).is_err());
        let mut value = native();
        value["Sources"][0]["Packages"][0]["Command"] = "evil.exe".into();
        assert!(Transfer::parse(value, &Catalog::empty()).is_err());
    }

    fn attach_registered_names(transfer: &mut Transfer) {
        transfer.detected = vec![
            DetectedSoftware {
                name: "Google Chrome".into(),
                version: "1.0".into(),
                publisher: "Google".into(),
            },
            DetectedSoftware {
                name: "Google Chrome (64-bit)".into(),
                version: "1.0".into(),
                publisher: "Google".into(),
            },
        ];
    }

    #[test]
    fn chrome_name_match_is_explicit_and_survives_export_without_a_catalog() {
        let mut value = native();
        value["Sources"].as_array_mut().unwrap().remove(0); // The PC exported only Store packages.
        let mut transfer = Transfer::parse(value, &Catalog::empty()).unwrap();
        attach_registered_names(&mut transfer);
        let chrome = AppEntry::from_winget("Google Chrome", "Google.Chrome", "1").unwrap();
        assert!(!transfer.profile.selected_apps.contains(&chrome.id));
        transfer
            .add_name_match(0, &chrome, &Catalog::empty())
            .unwrap();
        assert_eq!(transfer.document["Sources"].as_array().unwrap().len(), 2);
        let restored = Transfer::parse(transfer.document.clone(), &Catalog::empty()).unwrap();
        assert!(
            restored
                .profile
                .selected_apps
                .contains("search-Google.Chrome")
        );
        assert_eq!(restored.profile.selected_apps.len(), 2);
        assert!(restored.profile.validate(&Catalog::empty()).is_ok());
    }

    #[test]
    fn repeated_names_do_not_duplicate_packages_and_replacement_and_undo_keep_originals() {
        let mut transfer = Transfer::parse(native(), &Catalog::empty()).unwrap();
        attach_registered_names(&mut transfer);
        let original = transfer.document.clone();
        let chrome = AppEntry::from_winget("Google Chrome", "Google.Chrome", "1").unwrap();
        let beta = AppEntry::from_winget("Google Chrome Beta", "Google.Chrome.Beta", "1").unwrap();
        transfer
            .add_name_match(0, &chrome, &Catalog::empty())
            .unwrap();
        transfer
            .add_name_match(1, &chrome, &Catalog::empty())
            .unwrap();
        assert_eq!(transfer.profile.selected_apps.len(), 4);
        transfer.remove_name_match(0, &Catalog::empty()).unwrap();
        assert!(transfer.profile.selected_apps.contains(&chrome.id));
        transfer
            .add_name_match(1, &beta, &Catalog::empty())
            .unwrap();
        assert!(!transfer.profile.selected_apps.contains(&chrome.id));
        assert!(transfer.profile.selected_apps.contains(&beta.id));
        transfer.remove_name_match(1, &Catalog::empty()).unwrap();
        assert_eq!(transfer.document, original);
        // Undo a mapping to a package already in the original export never removes that package.
        let original_app = AppEntry::from_winget("Stable", "Test.Stable", "1").unwrap();
        transfer
            .add_name_match(0, &original_app, &Catalog::empty())
            .unwrap();
        transfer.remove_name_match(0, &Catalog::empty()).unwrap();
        assert_eq!(transfer.document, original);
    }

    #[test]
    fn invalid_name_matches_leave_the_export_unchanged_and_profiles_still_work() {
        let catalog = Catalog::fixture();
        let mut transfer =
            Transfer::selection(&catalog, &BTreeSet::from([catalog.apps[0].id.clone()])).unwrap();
        attach_registered_names(&mut transfer);
        let before = transfer.document.clone();
        assert!(
            transfer
                .add_name_match(99, &catalog.apps[1], &catalog)
                .is_err()
        );
        let mut custom = catalog.apps[1].clone();
        custom.method = InstallMethod::Custom {
            location: "setup.exe".into(),
            args: vec![],
            sha256: None,
        };
        assert!(transfer.add_name_match(0, &custom, &catalog).is_err());
        assert_eq!(transfer.document, before);
        transfer
            .add_name_match(0, &catalog.apps[1], &catalog)
            .unwrap();
        let restored = Transfer::parse(transfer.document.clone(), &Catalog::empty()).unwrap();
        assert_eq!(restored.profile.selected_apps.len(), 2);
    }
    #[test]
    fn selection_export_includes_dependencies_but_not_unselected_registrations() {
        let mut catalog = Catalog::fixture();
        let mut unrelated = catalog.apps[0].clone();
        unrelated.id = "custom-unrelated".into();
        catalog.apps.push(unrelated);
        catalog.apps[1].dependencies = vec![catalog.apps[0].id.clone()];
        let selected = BTreeSet::from([catalog.apps[1].id.clone()]);
        let t = Transfer::selection(&catalog, &selected).unwrap();
        assert_eq!(t.profile.custom_apps.len(), 2);
        assert!(
            !t.profile
                .custom_apps
                .iter()
                .any(|app| app.id == "custom-unrelated")
        );
        assert!(Transfer::selection(&catalog, &BTreeSet::new()).is_err());
    }

    #[test]
    fn utf8_bom_native_file_round_trips_without_executing_any_packages() {
        let path =
            std::env::temp_dir().join(format!("transfer-test-{}.json", uuid::Uuid::new_v4()));
        let mut bytes = b"\xef\xbb\xbf".to_vec();
        bytes.extend(serde_json::to_vec(&native()).unwrap());
        std::fs::write(&path, bytes).unwrap();
        let transfer = Transfer::read(&path, &Catalog::empty()).unwrap();
        transfer.save(&path).unwrap();
        let restored = Transfer::read(&path, &Catalog::empty()).unwrap();
        assert_eq!(
            restored.profile.selected_apps,
            transfer.profile.selected_apps
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn native_export_switches_are_not_carried_or_executed_and_explicit_channels_are_rejected() {
        let mut value = native();
        value["Sources"][0]["Packages"][0]["InitialOverrideArguments"] =
            "/FORCERESTART private-value".into();
        value["Sources"][0]["Packages"][0]["InitialCustomSwitches"] = "/CUSTOM".into();
        value["Sources"][0]["Packages"][0]["Scope"] = "machine".into();
        let transfer = Transfer::parse(value.clone(), &Catalog::empty()).unwrap();
        let serialized = serde_json::to_string(&transfer.document).unwrap();
        assert!(!serialized.contains("private-value"));
        assert!(!serialized.contains("InitialOverrideArguments"));
        assert!(!serialized.contains("InitialCustomSwitches"));
        value["Sources"][0]["Packages"][0]["Channel"] = "beta".into();
        assert!(Transfer::parse(value, &Catalog::empty()).is_err());
    }

    #[test]
    fn missing_local_installers_include_dependencies_and_preserve_hash_on_relocation() {
        let mut app = AppEntry::from_winget("Local", "Test.Local", "1").unwrap();
        app.id = "custom-local".into();
        let missing = std::env::temp_dir().join(format!("{}.exe", uuid::Uuid::new_v4()));
        app.method = InstallMethod::Custom {
            location: missing.to_string_lossy().into_owned(),
            args: vec!["/arg".into()],
            sha256: Some("a".repeat(64)),
        };
        let mut other = app.clone();
        other.id = "custom-unselected".into();
        let mut parent = AppEntry::from_winget("Parent", "Test.Parent", "1").unwrap();
        parent.dependencies = vec![app.id.clone()];
        let profile = Profile {
            schema_version: SCHEMA_VERSION,
            selected_apps: BTreeSet::from([parent.id.clone()]),
            custom_apps: vec![app, other, parent],
            update_existing: true,
        };
        let mut transfer =
            Transfer::parse(serde_json::to_value(profile).unwrap(), &Catalog::empty()).unwrap();
        assert_eq!(transfer.missing_files().len(), 1);
        assert!(!transfer.profile.update_existing);
        let existing = std::env::current_exe().unwrap();
        if let InstallMethod::Custom {
            location,
            args,
            sha256,
        } = &mut transfer.profile.custom_apps[0].method
        {
            *location = existing.to_string_lossy().into_owned();
            assert_eq!(args, &["/arg"]);
            assert_eq!(
                sha256.as_deref(),
                Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            );
        }
        assert!(transfer.missing_files().is_empty());
    }
}
