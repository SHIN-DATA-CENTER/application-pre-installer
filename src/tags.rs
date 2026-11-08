use crate::{
    model::{SCHEMA_VERSION, timestamp},
    platform::{Backend, ps_json, winget},
    store::{read_json, write_json},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagIndex {
    pub schema_version: u32,
    pub source: String,
    pub source_version: String,
    pub fetched_at: u64,
    pub tags: Vec<String>,
}
#[derive(Deserialize)]
struct IndexResponse {
    source_version: String,
    tags: Vec<String>,
}
fn validate_source(text: &str) -> Result<()> {
    let source: serde_json::Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}').trim())?;
    ensure!(
        source["Name"] == "winget"
            && source["Type"] == "Microsoft.PreIndexed.Package"
            && source["Identifier"] == "Microsoft.Winget.Source_8wekyb3d8bbwe"
            && matches!(
                source["Arg"].as_str(),
                Some(
                    "https://cdn.winget.microsoft.com/cache" | "https://winget.azureedge.net/cache"
                )
            ),
        "タグ一覧はMicrosoft標準のWinGetコミュニティソースに対応しています。現在のソース設定では取得できません"
    );
    Ok(())
}
impl TagIndex {
    fn from_response(response: IndexResponse) -> Result<Self> {
        let mut tags = response.tags;
        // Keep original spelling, punctuation and non-Latin tags, including case variants.
        tags.sort_by_cached_key(|tag| (tag.to_lowercase(), tag.clone()));
        tags.dedup();
        let index = Self {
            schema_version: SCHEMA_VERSION,
            source: "winget".into(),
            source_version: response.source_version,
            fetched_at: timestamp(),
            tags,
        };
        index.validate()?;
        Ok(index)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == SCHEMA_VERSION && self.source == "winget",
            "未対応のタグ一覧です"
        );
        ensure!(
            !self.source_version.is_empty() && self.source_version.len() <= 128,
            "タグ一覧の提供元を確認できません"
        );
        ensure!(
            !self.tags.is_empty() && self.tags.len() <= 100_000,
            "タグ一覧の件数が不正です"
        );
        let mut unique = std::collections::BTreeSet::new();
        for tag in &self.tags {
            ensure!(
                !tag.trim().is_empty()
                    && tag.chars().count() <= 100
                    && !tag.chars().any(char::is_control),
                "タグの文字列が不正です"
            );
            ensure!(unique.insert(tag), "タグが重複しています");
        }
        Ok(())
    }
}
impl Backend {
    pub fn load_tags(&self) -> Result<Option<TagIndex>> {
        let path = self.store.root.join("cache/winget-tags.json");
        if !path.exists() {
            return Ok(None);
        }
        let index: TagIndex = read_json(&path)?;
        index.validate()?;
        Ok(Some(index))
    }
    pub fn fetch_tags(&self) -> Result<TagIndex> {
        let response = if self.demo {
            IndexResponse {
                source_version: "demo".into(),
                tags: (0..25_000)
                    .map(|i| format!("demo-tag-{i:05}"))
                    .chain(
                        ["browser", "developer-tools", "ゲーム", ".net", ".NET", "3d"]
                            .map(String::from),
                    )
                    .collect(),
            }
        } else {
            let source = winget(&["source", "export", "--name", "winget"])?;
            ensure!(source.code == 0, "WinGetソースの設定を確認できません");
            validate_source(&source.text)?;
            ps_json(
                include_str!("../scripts/tag-index.ps1"),
                &serde_json::json!({}),
            )?
        };
        let index = TagIndex::from_response(response)?;
        write_json(&self.store.root.join("cache/winget-tags.json"), &index)?;
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_and_different_sources_cannot_be_mislabeled_as_community_tags() {
        let source = serde_json::json!({"Name":"winget", "Type":"Microsoft.PreIndexed.Package", "Identifier":"Microsoft.Winget.Source_8wekyb3d8bbwe", "Arg":"https://cdn.winget.microsoft.com/cache"});
        assert!(validate_source(&source.to_string()).is_ok());
        for (key, value) in [
            ("Arg", "https://private.example/cache"),
            ("Name", "msstore"),
            ("Type", "Microsoft.Rest"),
            ("Identifier", "unknown"),
        ] {
            let mut altered = source.clone();
            altered[key] = value.into();
            assert!(validate_source(&altered.to_string()).is_err());
        }
    }
    #[test]
    fn tags_preserve_unicode_punctuation_and_case_variants_without_duplicate_rows() {
        let index = TagIndex::from_response(IndexResponse {
            source_version: "test".into(),
            tags: ["ゲーム", "browser", ".net", ".NET", "browser", "3d"]
                .map(String::from)
                .to_vec(),
        })
        .unwrap();
        assert_eq!(index.tags, [".NET", ".net", "3d", "browser", "ゲーム"]);
        let mut invalid = index.clone();
        invalid.tags.push("\n".into());
        assert!(invalid.validate().is_err());
        invalid = index.clone();
        invalid.source = "msstore".into();
        assert!(invalid.validate().is_err());
        invalid = index.clone();
        invalid.tags.push("browser".into());
        assert!(invalid.validate().is_err());
        invalid = index;
        invalid.schema_version += 1;
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn tag_cache_roundtrip_and_failed_read_leave_previous_cache_untouched() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let backend = Backend {
            store: crate::store::Store { root: root.clone() },
            demo: true,
        };
        assert!(backend.load_tags().unwrap().is_none());
        let index = backend.fetch_tags().unwrap();
        assert_eq!(index.tags.len(), 25_006);
        assert_eq!(backend.load_tags().unwrap().unwrap().tags, index.tags);
        let path = root.join("cache/winget-tags.json");
        let mut invalid = index;
        invalid.source = "untrusted".into();
        write_json(&path, &invalid).unwrap();
        let original = std::fs::read(&path).unwrap();
        assert!(backend.load_tags().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::remove_dir_all(root).unwrap();
    }
}
