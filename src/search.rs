use anyhow::{Context, Result, ensure};
use regex::{Regex, RegexBuilder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    All,
    Any,
    Regex,
}
impl SearchMode {
    pub fn from_label(label: &str) -> Self {
        match label {
            "いずれかの語（OR）" => Self::Any,
            "正規表現" => Self::Regex,
            _ => Self::All,
        }
    }
}
pub enum SearchPattern {
    Words { words: Vec<String>, any: bool },
    Regex(Regex),
}
impl SearchPattern {
    pub fn new(query: &str, mode: SearchMode) -> Result<Self> {
        ensure!(
            query.chars().count() <= 500,
            "検索語は500文字以内で入力してください"
        );
        Ok(if mode == SearchMode::Regex && !query.trim().is_empty() {
            Self::Regex(
                RegexBuilder::new(query)
                    .case_insensitive(true)
                    .size_limit(1_000_000)
                    .dfa_size_limit(1_000_000)
                    .nest_limit(64)
                    .build()
                    .context("正規表現が不正か、複雑すぎます")?,
            )
        } else {
            Self::Words {
                words: query.split_whitespace().map(str::to_lowercase).collect(),
                any: mode == SearchMode::Any,
            }
        })
    }
    pub fn matches(&self, text: &str) -> bool {
        match self {
            Self::Words { words, any } => {
                let text = text.to_lowercase();
                words.is_empty()
                    || if *any {
                        words.iter().any(|w| text.contains(w))
                    } else {
                        words.iter().all(|w| text.contains(w))
                    }
            }
            Self::Regex(pattern) => pattern.is_match(text),
        }
    }
    pub fn matches_fields(&self, fields: &[&str]) -> bool {
        match self {
            Self::Regex(pattern) => fields.iter().any(|field| pattern.is_match(field)),
            Self::Words { .. } => self.matches(&fields.join(" ")),
        }
    }
}

/// Preserve product, edition and channel words. Only split whitespace; never guess an ID.
pub fn name_terms(name: &str) -> Vec<String> {
    let mut terms = vec![name.to_owned()];
    for part in name.split_whitespace() {
        if !terms.iter().any(|s| s.eq_ignore_ascii_case(part)) {
            terms.push(part.to_owned());
        }
    }
    terms
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn word_search_is_order_independent_case_insensitive_and_handles_japanese_spaces() {
        let all = SearchPattern::new("CHROME　google", SearchMode::All).unwrap();
        assert!(all.matches("Google Chrome (EXE)"));
        assert!(!all.matches("Google Drive"));
        let any = SearchPattern::new("Firefox Chrome", SearchMode::Any).unwrap();
        assert!(any.matches("Google Chrome"));
        assert!(!any.matches("Google Drive"));
        assert!(
            SearchPattern::new("", SearchMode::Any)
                .unwrap()
                .matches("anything")
        );
        assert!(
            SearchPattern::new("C++ .NET", SearchMode::All)
                .unwrap()
                .matches("C++ and .NET")
        );
    }
    #[test]
    fn grep_patterns_are_local_and_invalid_patterns_are_errors() {
        let regex = SearchPattern::new("^google.*chrome$|^mozilla\\.", SearchMode::Regex).unwrap();
        assert!(regex.matches("Google Chrome"));
        assert!(regex.matches("Mozilla.Firefox"));
        assert!(!regex.matches("Google Chrome Beta"));
        assert!(regex.matches_fields(&["Google Chrome", "Google.Chrome"]));
        assert!(!regex.matches_fields(&["Google Chrome Beta", "Google.Chrome.Beta"]));
        assert!(SearchPattern::new("[", SearchMode::Regex).is_err());
        assert!(SearchPattern::new(&"x".repeat(501), SearchMode::All).is_err());
        assert_eq!(
            name_terms("Google　Chrome Beta"),
            ["Google　Chrome Beta", "Google", "Chrome", "Beta"]
        );
    }
}
