use crate::{
    model::{Outcome, Status},
    platform::{Backend, ps_json},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

// Only these known, optional Windows app identities are candidates. Store, App Installer,
// shell components, runtimes, codecs and gaming services are deliberately outside this list.
const WINDOWS_APPS: &[(&str, &str, &str)] = &[
    ("Microsoft.BingNews", "8wekyb3d8bbwe", "Microsoft ニュース"),
    ("Microsoft.BingWeather", "8wekyb3d8bbwe", "MSN 天気"),
    ("Microsoft.GetHelp", "8wekyb3d8bbwe", "ヘルプを表示"),
    ("Microsoft.Getstarted", "8wekyb3d8bbwe", "ヒント"),
    (
        "Microsoft.MicrosoftSolitaireCollection",
        "8wekyb3d8bbwe",
        "Microsoft Solitaire Collection",
    ),
    (
        "Microsoft.MicrosoftOfficeHub",
        "8wekyb3d8bbwe",
        "Microsoft 365",
    ),
    ("Microsoft.MicrosoftStickyNotes", "8wekyb3d8bbwe", "付箋"),
    ("Microsoft.People", "8wekyb3d8bbwe", "People"),
    ("Microsoft.Todos", "8wekyb3d8bbwe", "Microsoft To Do"),
    (
        "Microsoft.PowerAutomateDesktop",
        "8wekyb3d8bbwe",
        "Power Automate",
    ),
    ("Microsoft.YourPhone", "8wekyb3d8bbwe", "スマートフォン連携"),
    (
        "Microsoft.ZuneMusic",
        "8wekyb3d8bbwe",
        "メディア プレーヤー",
    ),
    ("Microsoft.ZuneVideo", "8wekyb3d8bbwe", "映画 & テレビ"),
    ("Microsoft.WindowsMaps", "8wekyb3d8bbwe", "Windows マップ"),
    (
        "Microsoft.WindowsFeedbackHub",
        "8wekyb3d8bbwe",
        "フィードバック Hub",
    ),
    (
        "Microsoft.WindowsAlarms",
        "8wekyb3d8bbwe",
        "Windows クロック",
    ),
    (
        "microsoft.windowscommunicationsapps",
        "8wekyb3d8bbwe",
        "メール / カレンダー",
    ),
    (
        "Microsoft.Microsoft3DViewer",
        "8wekyb3d8bbwe",
        "3D ビューアー",
    ),
    ("Microsoft.3DBuilder", "8wekyb3d8bbwe", "3D Builder"),
    ("Microsoft.MSPaint", "8wekyb3d8bbwe", "ペイント 3D"),
    ("Microsoft.Paint", "8wekyb3d8bbwe", "ペイント"),
    ("Microsoft.SkypeApp", "kzf8qxf38zg5c", "Skype"),
    ("MicrosoftTeams", "8wekyb3d8bbwe", "Microsoft Teams (旧版)"),
    ("MSTeams", "8wekyb3d8bbwe", "Microsoft Teams"),
    (
        "Clipchamp.Clipchamp",
        "yxz26nhyzhsrt",
        "Microsoft Clipchamp",
    ),
    (
        "Microsoft.OutlookForWindows",
        "8wekyb3d8bbwe",
        "Outlook for Windows",
    ),
    ("Microsoft.Copilot", "8wekyb3d8bbwe", "Microsoft Copilot"),
    ("Microsoft.GamingApp", "8wekyb3d8bbwe", "Xbox"),
    (
        "Microsoft.XboxApp",
        "8wekyb3d8bbwe",
        "Xbox コンソール コンパニオン",
    ),
    (
        "Microsoft.XboxGamingOverlay",
        "8wekyb3d8bbwe",
        "Xbox Game Bar",
    ),
    ("Microsoft.WindowsCalculator", "8wekyb3d8bbwe", "電卓"),
    ("Microsoft.WindowsCamera", "8wekyb3d8bbwe", "Windows カメラ"),
    ("Microsoft.WindowsNotepad", "8wekyb3d8bbwe", "メモ帳"),
    (
        "Microsoft.WindowsSoundRecorder",
        "8wekyb3d8bbwe",
        "サウンド レコーダー",
    ),
    (
        "Microsoft.Windows.Photos",
        "8wekyb3d8bbwe",
        "Microsoft フォト",
    ),
    ("Microsoft.ScreenSketch", "8wekyb3d8bbwe", "Snipping Tool"),
    (
        "MicrosoftCorporationII.QuickAssist",
        "8wekyb3d8bbwe",
        "クイック アシスト",
    ),
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemovableApp {
    pub name: String,
    pub family: String,
    pub full_name: String,
    pub owner_sid: String,
    pub version: String,
    pub removable: bool,
    pub blocked_reason: String,
}
impl RemovableApp {
    pub fn key(&self) -> String {
        format!("remove-{}", self.family)
    }
    pub fn title(&self) -> &str {
        WINDOWS_APPS
            .iter()
            .find(|(name, publisher, _)| {
                self.name.eq_ignore_ascii_case(name)
                    && self
                        .family
                        .eq_ignore_ascii_case(&format!("{name}_{publisher}"))
            })
            .map(|(_, _, title)| *title)
            .unwrap_or(&self.name)
    }
    pub fn validate_identity(&self) -> Result<()> {
        ensure!(
            WINDOWS_APPS
                .iter()
                .any(|(name, publisher, _)| self.name.eq_ignore_ascii_case(name)
                    && self
                        .family
                        .eq_ignore_ascii_case(&format!("{name}_{publisher}"))),
            "対応していないWindowsアプリです"
        );
        let parts: Vec<_> = self.full_name.split('_').collect();
        let version = parts.get(1).copied().unwrap_or("");
        let numbers: Vec<_> = version.split('.').collect();
        ensure!(
            parts.len() == 5
                && parts[0].eq_ignore_ascii_case(&self.name)
                && self
                    .family
                    .eq_ignore_ascii_case(&format!("{}_{}", parts[0], parts[4]))
                && numbers.len() == 4
                && numbers
                    .iter()
                    .all(|s| !s.is_empty() && s.parse::<u16>().is_ok())
                && version == self.version
                && matches!(parts[2], "x64" | "x86" | "arm" | "arm64" | "neutral")
                && parts[3]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".-~".contains(c)),
            "Windowsアプリの識別情報が不正です"
        );
        ensure!(
            self.owner_sid.starts_with("S-1-")
                && self.owner_sid.len() <= 184
                && self.owner_sid[4..]
                    .split('-')
                    .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())),
            "利用者の識別情報が不正です"
        );
        Ok(())
    }
    pub fn eligible(&self) -> bool {
        self.removable && self.blocked_reason.is_empty() && self.validate_identity().is_ok()
    }
    pub fn details(&self) -> String {
        format!(
            "【削除】{}\nパッケージ: {}\nバージョン: {}\n対象: 現在ログイン中の利用者のみ\n方式: Windows Remove-AppxPackage\n状態: {}\n\nアプリ内のローカルデータや設定が失われる可能性があります。必要なデータは先にバックアップしてください。\n他の利用者・新規利用者向けのプロビジョニングは変更しません。再導入はMicrosoft Storeで行えますが、データの復元は保証しません。\n\nこれは標準アプリと一致するIDの検出結果です。出荷時からあったか、後から追加されたかは判別できません。",
            self.title(),
            self.full_name,
            self.version,
            if self.eligible() {
                "削除候補"
            } else {
                &self.blocked_reason
            }
        )
    }
}

pub trait AppRemover {
    fn remove_app(&self, app: &RemovableApp) -> Result<Outcome>;
    fn verify_removal(&self, app: &RemovableApp) -> Result<Outcome>;
}
impl Backend {
    pub fn removable_apps(&self) -> Result<Vec<RemovableApp>> {
        if self.demo {
            return Ok(demo_apps());
        }
        let apps: Vec<RemovableApp> = ps_json(
            include_str!("../scripts/windows-apps.ps1"),
            &payload("scan", None),
        )?;
        for app in &apps {
            app.validate_identity()?;
        }
        Ok(apps)
    }
}
fn payload(action: &str, app: Option<&RemovableApp>) -> serde_json::Value {
    serde_json::json!({ "action": action, "app": app, "known": WINDOWS_APPS.iter().map(|(name, publisher, _)| serde_json::json!({"name": name, "family": format!("{name}_{publisher}")})).collect::<Vec<_>>() })
}
impl AppRemover for Backend {
    fn remove_app(&self, app: &RemovableApp) -> Result<Outcome> {
        ensure!(app.eligible(), "削除条件を満たさないアプリです");
        if self.demo {
            return Ok(Outcome::new(Status::Unknown, "デモのため削除していません"));
        }
        ps_json(
            include_str!("../scripts/windows-apps.ps1"),
            &payload("remove", Some(app)),
        )
    }
    fn verify_removal(&self, app: &RemovableApp) -> Result<Outcome> {
        app.validate_identity()?;
        if self.demo {
            return Ok(Outcome::new(
                Status::Unknown,
                "デモのため削除状態は確認しません",
            ));
        }
        ps_json(
            include_str!("../scripts/windows-apps.ps1"),
            &payload("verify", Some(app)),
        )
    }
}
fn demo_apps() -> Vec<RemovableApp> {
    [
        "Microsoft.BingWeather",
        "Microsoft.WindowsCalculator",
        "Microsoft.Windows.Photos",
    ]
    .into_iter()
    .map(|name| RemovableApp {
        name: name.into(),
        family: format!("{name}_8wekyb3d8bbwe"),
        full_name: format!("{name}_1.0.0.0_x64__8wekyb3d8bbwe"),
        owner_sid: "S-1-5-21-0-0-0-1001".into(),
        version: "1.0.0.0".into(),
        removable: name != "Microsoft.Windows.Photos",
        blocked_reason: if name == "Microsoft.Windows.Photos" {
            "削除不可（デモ用の保護状態）".into()
        } else {
            String::new()
        },
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn windows_worker_rechecks_identity_dependencies_and_result_using_mock_packages_only() {
        let script = include_str!("../scripts/windows-apps.test.ps1");
        let result: serde_json::Value = ps_json(
            script,
            &serde_json::json!({"script": include_str!("../scripts/windows-apps.ps1")}),
        )
        .unwrap();
        for (case, status, calls) in [
            ("success", "success", 1),
            ("absent", "skipped", 0),
            ("changed", "awaiting_user", 0),
            ("protected", "awaiting_user", 0),
            ("framework", "awaiting_user", 0),
            ("dependent", "awaiting_user", 0),
            ("badstatus", "awaiting_user", 0),
            ("unknownsignature", "awaiting_user", 0),
            ("failure", "failed", 1),
            ("verify_present", "unknown", 0),
            ("verify_absent", "success", 0),
        ] {
            assert_eq!(result[case]["status"], status, "{case}: {result}");
            assert_eq!(
                result[case]["calls"].as_array().unwrap().len(),
                calls,
                "{case}"
            );
        }
        assert_eq!(
            result["success"]["calls"][0],
            "Microsoft.BingWeather_1.0.0.0_x64__8wekyb3d8bbwe"
        );
        assert_eq!(result["wrong_user"]["blocked"], true);
        assert_eq!(result["wrong_user"]["calls"].as_array().unwrap().len(), 0);
        assert_eq!(result["scan"].as_array().unwrap().len(), 1);
        assert_eq!(
            result["scan"][0]["full_name"],
            "Microsoft.BingWeather_1.0.0.0_neutral_~_8wekyb3d8bbwe"
        );
    }
    #[test]
    fn removal_targets_reject_system_components_unknown_publishers_and_wildcards() {
        let base = demo_apps()[0].clone();
        assert!(base.eligible());
        for (name, family, full_name) in [
            (
                "Microsoft.WindowsStore",
                "Microsoft.WindowsStore_8wekyb3d8bbwe",
                "Microsoft.WindowsStore_1.0.0.0_x64__8wekyb3d8bbwe",
            ),
            (
                "Microsoft.DesktopAppInstaller",
                "Microsoft.DesktopAppInstaller_8wekyb3d8bbwe",
                "Microsoft.DesktopAppInstaller_1.0.0.0_x64__8wekyb3d8bbwe",
            ),
            (
                "Microsoft.BingWeather",
                "Microsoft.BingWeather_other",
                "Microsoft.BingWeather_1.0.0.0_x64__other",
            ),
            (
                "Microsoft.BingWeather",
                "Microsoft.BingWeather_8wekyb3d8bbwe",
                "*",
            ),
        ] {
            let app = RemovableApp {
                name: name.into(),
                family: family.into(),
                full_name: full_name.into(),
                ..base.clone()
            };
            assert!(!app.eligible());
        }
        assert!(!demo_apps()[2].eligible());
        let mut wrong_version = base.clone();
        wrong_version.version = "2.0.0.0".into();
        assert!(!wrong_version.eligible());
        let mut no_owner = base;
        no_owner.owner_sid.clear();
        assert!(!no_owner.eligible());
    }
}
