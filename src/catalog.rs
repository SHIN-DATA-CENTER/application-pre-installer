use crate::model::{AppEntry, InstallMethod};
use std::{cmp::Reverse, collections::BTreeSet};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    NameAscending,
    NameDescending,
    IdAscending,
    IdDescending,
    SelectedFirst,
}
impl SortOrder {
    pub fn from_label(label: &str) -> Self {
        match label {
            "名前 Z → A" => Self::NameDescending,
            "ID A → Z" => Self::IdAscending,
            "ID Z → A" => Self::IdDescending,
            "選択中を先に" => Self::SelectedFirst,
            _ => Self::NameAscending,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::NameAscending => "名前 A → Z",
            Self::NameDescending => "名前 Z → A",
            Self::IdAscending => "ID A → Z",
            Self::IdDescending => "ID Z → A",
            Self::SelectedFirst => "選択中を先に",
        }
    }
}
pub fn package_id(app: &AppEntry) -> &str {
    match &app.method {
        InstallMethod::Winget { package_id, .. } => package_id,
        _ => &app.id,
    }
}
pub fn sort_apps(apps: &mut Vec<&AppEntry>, order: SortOrder, selected: &BTreeSet<String>) {
    match order {
        SortOrder::NameAscending => {
            apps.sort_by_cached_key(|a| (a.name.to_lowercase(), package_id(a).to_lowercase()))
        }
        SortOrder::NameDescending => apps
            .sort_by_cached_key(|a| Reverse((a.name.to_lowercase(), package_id(a).to_lowercase()))),
        SortOrder::IdAscending => apps.sort_by_cached_key(|a| package_id(a).to_lowercase()),
        SortOrder::IdDescending => {
            apps.sort_by_cached_key(|a| Reverse(package_id(a).to_lowercase()))
        }
        SortOrder::SelectedFirst => apps.sort_by_cached_key(|a| {
            (
                !selected.contains(&a.id),
                a.name.to_lowercase(),
                package_id(a).to_lowercase(),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sorting_is_case_insensitive_and_uses_ids_to_break_ties() {
        let apps = [
            AppEntry::from_winget("Beta", "A.Beta", "1").unwrap(),
            AppEntry::from_winget("alpha", "Z.Second", "1").unwrap(),
            AppEntry::from_winget("Alpha", "B.First", "1").unwrap(),
        ];
        let selected = BTreeSet::from([apps[1].id.clone()]);
        for (order, expected) in [
            (
                SortOrder::NameAscending,
                vec!["B.First", "Z.Second", "A.Beta"],
            ),
            (
                SortOrder::NameDescending,
                vec!["A.Beta", "Z.Second", "B.First"],
            ),
            (
                SortOrder::IdAscending,
                vec!["A.Beta", "B.First", "Z.Second"],
            ),
            (
                SortOrder::IdDescending,
                vec!["Z.Second", "B.First", "A.Beta"],
            ),
            (
                SortOrder::SelectedFirst,
                vec!["Z.Second", "B.First", "A.Beta"],
            ),
        ] {
            let mut rows = apps.iter().collect();
            sort_apps(&mut rows, order, &selected);
            assert_eq!(
                rows.into_iter().map(package_id).collect::<Vec<_>>(),
                expected
            );
        }
    }
}
