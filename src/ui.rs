use crate::{
    AppWindow, CategoryRow, ItemRow,
    catalog::{SortOrder, sort_apps},
    model::*,
    platform::{AppDetails, Backend, DriverProvider},
    runner::{self, RunEvent},
    search::{SearchMode, SearchPattern, name_terms},
    store::CachedCatalog,
    tags::TagIndex,
    transfer::Transfer,
    uninstall::RemovableApp,
};
use anyhow::{Context, Result, ensure};
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

enum Event {
    Environment(Result<Box<Environment>>),
    Catalog(Result<CachedCatalog>),
    Tag(Result<Vec<AppEntry>>, String),
    Tags(Result<TagIndex>),
    Drivers(Result<crate::platform::DriverScan>),
    Removals(Result<Vec<RemovableApp>>),
    Detail(Result<AppDetails>, String),
    Review(Result<String>),
    Run(RunEvent),
    Finished(Result<Session>),
    Prepared(Result<String>),
    Installed(Result<Transfer>),
    MatchDetail(Result<String>, String),
}
struct State {
    backend: Backend,
    base: Catalog,
    catalog: Catalog,
    selected: BTreeSet<String>,
    drivers: Vec<DriverCandidate>,
    selected_drivers: BTreeSet<String>,
    removals: Vec<RemovableApp>,
    selected_removals: BTreeSet<String>,
    environment: Option<Environment>,
    session: Option<Session>,
    resume: Option<Session>,
    query: String,
    category: String,
    sort: SortOrder,
    tag_matches: Option<BTreeSet<String>>,
    tag_query: String,
    tags: Option<TagIndex>,
    tag_status: String,
    tags_model: Rc<VecModel<slint::SharedString>>,
    catalog_status: String,
    apps_model: Rc<VecModel<ItemRow>>,
    removals_model: Rc<VecModel<ItemRow>>,
    detail_url: Option<String>,
    detail_key: Option<String>,
    reviewed: bool,
    cancel: Arc<AtomicBool>,
    log: String,
    transfer: Option<Transfer>,
    match_index: Option<usize>,
    match_sources_model: Rc<VecModel<ItemRow>>,
    match_apps_model: Rc<VecModel<ItemRow>>,
    match_detail_key: Option<String>,
}
impl State {
    fn filtered_match_sources(&self, window: &AppWindow) -> Vec<usize> {
        let Some(transfer) = &self.transfer else {
            return vec![];
        };
        let Ok(pattern) =
            SearchPattern::new(window.get_match_source_query().as_str(), SearchMode::All)
        else {
            return vec![];
        };
        let exported_names = transfer.exported_names(&self.base);
        transfer
            .detected
            .iter()
            .enumerate()
            .filter(|(i, a)| {
                pattern.matches(&format!("{} {}", a.name, a.publisher))
                    && (!window.get_match_only_missing()
                        || self.match_index == Some(*i)
                        || (!transfer.name_matches.contains_key(i)
                            && !exported_names.contains(&crate::transfer::normalize_name(&a.name))))
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn choose_match_source(&mut self, window: &AppWindow, index: Option<usize>) {
        self.match_index = index;
        self.match_detail_key = None;
        window.set_match_detail_text("候補の「詳細」で提供元・利用条件を確認できます。名前だけでは同一製品と判定していません。".into());
        window.set_match_mode("すべての語（AND）".into());
        if let Some(app) = self
            .transfer
            .as_ref()
            .and_then(|t| index.and_then(|i| t.detected.get(i)))
        {
            window.set_match_query(app.name.clone().into());
            window.set_match_term(app.name.clone().into());
            window.set_match_terms(ModelRc::new(VecModel::from(
                name_terms(&app.name)
                    .into_iter()
                    .map(slint::SharedString::from)
                    .collect::<Vec<_>>(),
            )));
            window.set_match_selected_text(
                format!("登録名: {} / {} / {}", app.name, app.version, app.publisher).into(),
            );
        } else {
            window.set_match_query("".into());
            window.set_match_terms(ModelRc::new(VecModel::<slint::SharedString>::default()));
            window.set_match_selected_text("左側で、候補を探す登録名を選んでください。".into());
        }
        self.render_matches(window);
    }

    fn render_matches(&self, window: &AppWindow) {
        let Some(transfer) = &self.transfer else {
            return;
        };
        let visible = self.filtered_match_sources(window);
        let exported_names = transfer.exported_names(&self.base);
        window.set_match_source_count(
            format!(
                "Windowsの登録名 {} / {} 件",
                visible.len(),
                transfer.detected.len()
            )
            .into(),
        );
        let source_rows = visible
            .into_iter()
            .map(|i| {
                let app = &transfer.detected[i];
                ItemRow {
                    key: i.to_string().into(),
                    title: app.name.clone().into(),
                    subtitle: format!("{} / {}", app.version, app.publisher).into(),
                    meta: transfer
                        .name_matches
                        .get(&i)
                        .map(|a| format!("追加: {}", a.source_label()))
                        .unwrap_or_else(|| {
                            if exported_names.contains(&crate::transfer::normalize_name(&app.name))
                            {
                                "自動出力に同名あり".into()
                            } else {
                                "名前から候補を確認".into()
                            }
                        })
                        .into(),
                    checked: self.match_index == Some(i),
                    enabled: true,
                    ..Default::default()
                }
            })
            .collect();
        update_rows(&self.match_sources_model, source_rows);
        window.set_match_can_undo(
            self.match_index
                .is_some_and(|i| transfer.name_matches.contains_key(&i)),
        );
        let pattern = match SearchPattern::new(
            window.get_match_query().as_str(),
            SearchMode::from_label(window.get_match_mode().as_str()),
        ) {
            Ok(p) => p,
            Err(e) => {
                self.match_apps_model.set_vec(vec![]);
                window.set_match_status(format!("{e:#}").into());
                return;
            }
        };
        let mut apps: Vec<_> = self
            .base
            .apps
            .iter()
            .filter(|app| {
                self.match_index.is_some()
                    && pattern.matches_fields(&[&app.name, crate::catalog::package_id(app)])
            })
            .collect();
        let exact = self
            .match_index
            .and_then(|i| transfer.detected.get(i))
            .map(|a| crate::transfer::normalize_name(&a.name))
            .unwrap_or_default();
        apps.sort_by_cached_key(|a| {
            (
                crate::transfer::normalize_name(&a.name) != exact,
                a.name.to_lowercase(),
                a.id.to_lowercase(),
            )
        });
        window.set_match_status(format!("候補 {} 件 / 取得済み一覧 {} 件。空白は単語の区切り、正規表現はこの一覧に適用します。", apps.len(), self.base.apps.len()).into());
        let rows = apps
            .into_iter()
            .map(|app| ItemRow {
                key: app.id.clone().into(),
                title: app.name.clone().into(),
                subtitle: app.description.clone().into(),
                meta: app.source_label().into(),
                checked: self
                    .match_index
                    .and_then(|i| transfer.name_matches.get(&i))
                    .is_some_and(|a| a.id == app.id),
                enabled: true,
                ..Default::default()
            })
            .collect();
        update_rows(&self.match_apps_model, rows);
    }

    fn profile(&self, update_existing: bool) -> Profile {
        Profile::capture(&self.catalog, &self.selected, update_existing)
    }
    fn selection(&self) -> Result<(Vec<AppEntry>, Vec<DriverCandidate>)> {
        if let Some(resume) = &self.resume {
            let apps = resume
                .tasks
                .iter()
                .filter(|t| {
                    matches!(
                        t.outcome.status,
                        Status::Pending | Status::Running | Status::RebootRequired
                    )
                })
                .filter_map(|t| match &t.target {
                    Target::App { app } => Some(app.clone()),
                    _ => None,
                })
                .collect();
            let drivers = resume
                .tasks
                .iter()
                .filter(|t| {
                    matches!(
                        t.outcome.status,
                        Status::Pending | Status::Running | Status::RebootRequired
                    )
                })
                .filter_map(|t| match &t.target {
                    Target::Driver { driver } => Some(driver.clone()),
                    _ => None,
                })
                .collect();
            return Ok((apps, drivers));
        }
        let apps = self.catalog.ordered(&self.selected)?;
        let drivers = self
            .drivers
            .iter()
            .filter(|d| self.selected_drivers.contains(&d.key()))
            .cloned()
            .collect();
        Ok((apps, drivers))
    }
    fn removal_selection(&self) -> Vec<RemovableApp> {
        if let Some(session) = &self.resume {
            return session
                .tasks
                .iter()
                .filter(|t| {
                    matches!(
                        t.outcome.status,
                        Status::Pending | Status::Running | Status::RebootRequired
                    )
                })
                .filter_map(|t| match &t.target {
                    Target::Remove { app } => Some(app.clone()),
                    _ => None,
                })
                .collect();
        }
        self.removals
            .iter()
            .filter(|a| self.selected_removals.contains(&a.key()))
            .cloned()
            .collect()
    }
    fn render_apps(&self, window: &AppWindow) {
        let pattern = SearchPattern::new(&self.query, SearchMode::All);
        let mut visible: Vec<_> = self
            .catalog
            .apps
            .iter()
            .filter(|a| {
                (self.category == "すべて"
                    || self.category == "選択中" && self.selected.contains(&a.id)
                    || self.category == "独自登録" && a.id.starts_with("custom-"))
                    && self
                        .tag_matches
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&a.id))
                    && pattern.as_ref().is_ok_and(|query| {
                        query.matches(&format!(
                            "{} {} {} {}",
                            a.name,
                            a.publisher,
                            a.description,
                            a.source_label()
                        ))
                    })
            })
            .collect();
        sort_apps(&mut visible, self.sort, &self.selected);
        let installed: BTreeMap<_, _> = self
            .environment
            .iter()
            .flat_map(|e| e.installed.iter())
            .map(|i| (i.name.to_lowercase(), i))
            .collect();
        let apps: Vec<ItemRow> = visible
            .into_iter()
            .map(|a| {
                let status = if let Some(installed) = installed.get(&a.name.to_lowercase()) {
                    format!("登録あり {}", installed.version)
                } else {
                    match &a.method {
                        InstallMethod::Winget { .. } => "WinGet".into(),
                        InstallMethod::Manual { .. } => "手動操作あり".into(),
                        InstallMethod::Custom { .. } => "独自登録・対話型".into(),
                        InstallMethod::Unavailable { .. } => "未確定".into(),
                    }
                };
                ItemRow {
                    key: a.id.clone().into(),
                    title: a.name.clone().into(),
                    subtitle: a.description.clone().into(),
                    meta: a.source_label().into(),
                    group_title: "".into(),
                    status: status.into(),
                    checked: self.selected.contains(&a.id),
                    enabled: !matches!(a.method, InstallMethod::Unavailable { .. }),
                }
            })
            .collect();
        window.set_visible_app_count(apps.len() as i32);
        let same_keys = self.apps_model.row_count() == apps.len()
            && apps.iter().enumerate().all(|(i, row)| {
                self.apps_model
                    .row_data(i)
                    .is_some_and(|old| old.key == row.key)
            });
        if same_keys {
            for (index, row) in apps.into_iter().enumerate() {
                if self.apps_model.row_data(index).as_ref() != Some(&row) {
                    self.apps_model.set_row_data(index, row);
                }
            }
        } else {
            self.apps_model.set_vec(apps);
        }
    }
    fn render(&self, window: &AppWindow) {
        if window.get_match_open() {
            self.render_matches(window);
        }
        window.set_app_category(self.category.clone().into());
        window.set_app_query(self.query.clone().into());
        window.set_app_sort(self.sort.label().into());
        window.set_active_tag(self.tag_query.clone().into());
        window.set_catalog_status(self.catalog_status.clone().into());
        window.set_has_catalog(!self.base.apps.is_empty());
        window.set_tag_status(self.tag_status.clone().into());
        if window.get_tags_open() {
            let query = window.get_tag_filter().to_lowercase();
            let visible: Vec<slint::SharedString> = self
                .tags
                .iter()
                .flat_map(|index| index.tags.iter())
                .filter(|tag| query.is_empty() || tag.to_lowercase().contains(&query))
                .map(|tag| tag.as_str().into())
                .collect();
            window.set_visible_tag_count(visible.len() as i32);
            window
                .set_total_tag_count(self.tags.as_ref().map(|i| i.tags.len()).unwrap_or(0) as i32);
            if self.tags_model.row_count() != visible.len()
                || visible
                    .iter()
                    .enumerate()
                    .any(|(i, tag)| self.tags_model.row_data(i).as_ref() != Some(tag))
            {
                self.tags_model.set_vec(visible);
            }
        }
        let categories = vec![
            CategoryRow {
                name: "すべて".into(),
                label: "すべてのアプリ".into(),
                count: self.catalog.apps.len() as i32,
            },
            CategoryRow {
                name: "選択中".into(),
                label: "選択中".into(),
                count: self.selected.len() as i32,
            },
            CategoryRow {
                name: "独自登録".into(),
                label: "独自登録".into(),
                count: self
                    .catalog
                    .apps
                    .iter()
                    .filter(|a| a.id.starts_with("custom-"))
                    .count() as i32,
            },
        ];
        let current_categories = window.get_categories();
        if current_categories.row_count() != categories.len() {
            window.set_categories(ModelRc::new(VecModel::from(categories)));
        } else {
            for (index, category) in categories.into_iter().enumerate() {
                if current_categories.row_data(index).as_ref() != Some(&category) {
                    current_categories.set_row_data(index, category);
                }
            }
        }
        if let Some(env) = &self.environment {
            window.set_os_label(format!("{} / {}", env.os, env.architecture).into());
            window.set_pc_label(format!("{} {}", env.manufacturer, env.model).trim().into());
            window.set_board_label(
                format!(
                    "{} {} / Rev. {}",
                    env.board_manufacturer, env.board_product, env.board_revision
                )
                .into(),
            );
            window.set_winget_label(if env.winget.is_empty() {
                "未準備".into()
            } else {
                env.winget.clone().into()
            });
            window.set_network_label(
                if env.network_available {
                    "接続あり"
                } else {
                    "未接続"
                }
                .into(),
            );
            window.set_device_label(
                format!(
                    "{} 件 / 問題コードあり {} 件",
                    env.devices.len(),
                    env.devices.iter().filter(|d| d.problem_code != 0).count()
                )
                .into(),
            );
        } else {
            window.set_os_label("未確認".into());
            window.set_pc_label("未確認".into());
            window.set_board_label("未確認".into());
            window.set_winget_label("未確認".into());
            window.set_network_label("未確認".into());
            window.set_device_label("未確認".into());
        }
        if window.get_step() == 1 {
            self.render_apps(window);
        }
        let drivers: Vec<_> = self
            .drivers
            .iter()
            .map(|d| ItemRow {
                group_title: "".into(),
                key: format!("driver-{}", d.key()).into(),
                title: d.title.clone().into(),
                subtitle: format!(
                    "{} / {} / 現在: {}",
                    d.model,
                    d.provider,
                    if d.current_version.is_empty() {
                        "不明"
                    } else {
                        &d.current_version
                    }
                )
                .into(),
                meta: format!("{} · {}", d.class, d.hardware_id).into(),
                status: if d.optional {
                    "任意更新"
                } else {
                    "更新候補"
                }
                .into(),
                checked: self.selected_drivers.contains(&d.key()),
                enabled: d.eligible(),
            })
            .collect();
        window.set_drivers(ModelRc::new(VecModel::from(drivers)));
        let removal_query = window.get_removal_query().to_lowercase();
        let removal_rows: Vec<_> = self
            .removals
            .iter()
            .filter(|a| {
                removal_query.is_empty()
                    || format!("{} {}", a.title(), a.name)
                        .to_lowercase()
                        .contains(&removal_query)
            })
            .map(|a| ItemRow {
                group_title: "".into(),
                key: a.key().into(),
                title: a.title().into(),
                subtitle: if a.eligible() {
                    format!("現在の利用者から削除 / {}", a.version)
                } else {
                    a.blocked_reason.clone()
                }
                .into(),
                meta: a.family.clone().into(),
                status: if a.eligible() {
                    "削除候補"
                } else {
                    "対象外"
                }
                .into(),
                checked: self.selected_removals.contains(&a.key()),
                enabled: a.eligible(),
            })
            .collect();
        update_rows(&self.removals_model, removal_rows);
        if let Some(session) = &self.session {
            let rows: Vec<_> = session
                .tasks
                .iter()
                .map(|t| ItemRow {
                    group_title: "".into(),
                    key: t.target.key().into(),
                    title: t.target.name().into(),
                    subtitle: t.outcome.message.clone().into(),
                    meta: match &t.target {
                        Target::App { app } => app.source_label(),
                        Target::Driver { driver } => {
                            format!("Windows Update / {}", driver.provider)
                        }
                        Target::Remove { .. } => "削除 / 現在の利用者 / Windows Appx".into(),
                    }
                    .into(),
                    status: t.outcome.status.label().into(),
                    checked: t.outcome.status == Status::Success,
                    enabled: true,
                })
                .collect();
            window.set_results(ModelRc::new(VecModel::from(rows)));
            if window.get_step() == 5 {
                let completed = session
                    .tasks
                    .iter()
                    .filter(|t| {
                        t.outcome.status != Status::Pending && t.outcome.status != Status::Running
                    })
                    .count();
                window.set_selection_text(
                    format!("処理済み {completed} / {} 件", session.tasks.len()).into(),
                );
            }
        }
        if window.get_step() != 5 {
            window.set_selection_text(
                format!(
                    "導入 {} 件 / 削除 {} 件 / ドライバー {} 件",
                    self.selected.len(),
                    self.selected_removals.len(),
                    self.selected_drivers.len()
                )
                .into(),
            );
        }
        window.set_log_text(self.log.clone().into());
        window.set_can_resume(self.session.is_some());
    }
    fn invalidate(&mut self, window: &AppWindow) {
        self.reviewed = false;
        self.resume = None;
        window.set_install_consent(false);
        window.set_removal_consent(false);
    }
}

fn begin(window: &AppWindow, message: &str) {
    window.set_busy(true);
    window.set_message(message.into());
}
fn update_rows(model: &VecModel<ItemRow>, rows: Vec<ItemRow>) {
    if model.row_count() == rows.len()
        && rows
            .iter()
            .enumerate()
            .all(|(i, row)| model.row_data(i).is_some_and(|old| old.key == row.key))
    {
        for (i, row) in rows.into_iter().enumerate() {
            if model.row_data(i).as_ref() != Some(&row) {
                model.set_row_data(i, row);
            }
        }
    } else {
        model.set_vec(rows);
    }
}
fn spawn(tx: &mpsc::Sender<Event>, job: impl FnOnce() -> Event + Send + 'static) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(job());
    });
}
fn message(window: &AppWindow, result: Result<()>) {
    if let Err(e) = result {
        window.set_message(format!("{e:#}").into());
    }
}

pub fn run(backend: Backend) -> Result<()> {
    let session_result = backend.store.load_session();
    let mut initial_message = match &session_result {
        Ok(Some(_)) => "前回の履歴があります。環境確認後に内容を確認して再開できます。".to_string(),
        Ok(None) => "必要な項目だけを選んで導入できます。".into(),
        Err(e) => format!("前回の履歴を読み込めませんでした: {e:#}"),
    };
    let (base, catalog_status) = match backend.store.load_catalog() {
        Ok(Some(cache)) => {
            let count = cache.catalog.apps.len();
            (cache.catalog, format!("前回取得した一覧: {count} 件"))
        }
        Ok(None) => (Catalog::empty(), "WinGet一覧は未取得です".into()),
        Err(e) => {
            initial_message =
                format!("一覧キャッシュを読み込めませんでした。一覧を再取得してください: {e:#}");
            (Catalog::empty(), "キャッシュの再取得が必要です".into())
        }
    };
    let mut catalog = base.clone();
    let (tags, tag_status) = match backend.load_tags() {
        Ok(Some(index)) => {
            let count = index.tags.len();
            (
                Some(index),
                format!("前回取得したタグ {count} 件 / WinGet索引"),
            )
        }
        Ok(None) => (None, "タグ一覧は未取得です".into()),
        Err(e) => (None, format!("タグ一覧を再取得してください: {e:#}")),
    };
    // Restore registrations only. Selections/consents always start empty unless explicitly imported.
    if let Ok(Some(mut profile)) = backend.store.load_profile() {
        profile.selected_apps.clear();
        profile.custom_apps.retain(|a| a.id.starts_with("custom-"));
        if let Ok(c) = profile.validate(&base) {
            catalog = c;
        }
    }
    let state = Rc::new(RefCell::new(State {
        backend: backend.clone(),
        base,
        catalog,
        selected: BTreeSet::new(),
        drivers: vec![],
        selected_drivers: BTreeSet::new(),
        removals: vec![],
        selected_removals: BTreeSet::new(),
        environment: None,
        session: session_result.ok().flatten(),
        resume: None,
        query: String::new(),
        category: "すべて".into(),
        sort: SortOrder::default(),
        tag_matches: None,
        tag_query: String::new(),
        tags,
        tag_status,
        tags_model: Rc::new(VecModel::default()),
        catalog_status,
        apps_model: Rc::new(VecModel::default()),
        removals_model: Rc::new(VecModel::default()),
        detail_url: None,
        detail_key: None,
        reviewed: false,
        cancel: Arc::new(AtomicBool::new(false)),
        log: String::new(),
        transfer: None,
        match_index: None,
        match_sources_model: Rc::new(VecModel::default()),
        match_apps_model: Rc::new(VecModel::default()),
        match_detail_key: None,
    }));
    let window = AppWindow::new()?;
    window.set_apps(ModelRc::from(state.borrow().apps_model.clone()));
    window.set_removals(ModelRc::from(state.borrow().removals_model.clone()));
    window.set_tag_names(ModelRc::from(state.borrow().tags_model.clone()));
    window.set_match_sources(ModelRc::from(state.borrow().match_sources_model.clone()));
    window.set_match_apps(ModelRc::from(state.borrow().match_apps_model.clone()));
    window.set_demo(backend.demo);
    window.set_message(initial_message.into());
    let (tx, rx) = mpsc::channel();

    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_show_environment(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() {
                    return;
                }
                let mut s = state.borrow_mut();
                s.detail_key = None;
                s.detail_url = None;
                w.set_detail_can_open(false);
                w.set_detail_category_editable(false);
                w.set_detail_text(w.get_environment_text());
                w.set_details_open(true);
            }
        });
    }

    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_refresh_environment(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() {
                    return;
                }
                begin(&w, "OS・マザーボード・デバイスを確認しています…");
                let b = state.borrow().backend.clone();
                spawn(&tx, move || {
                    Event::Environment(b.environment().map(Box::new))
                });
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_prepare_winget(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() {
                    return;
                }
                begin(&w, "Microsoft公式の手順でWinGetを準備しています…");
                let b = state.borrow().backend.clone();
                spawn(&tx, move || Event::Prepared(b.prepare_winget()));
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_filter_apps(move |query, category| {
            if let Some(w) = weak.upgrade() {
                let mut s = state.borrow_mut();
                s.query = query.to_string();
                s.category = category.to_string();
                s.render(&w);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_fetch_catalog(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() || !w.get_source_consent() {
                    return;
                }
                begin(&w, "WinGetの全件一覧を取得しています…");
                let b = state.borrow().backend.clone();
                spawn(&tx, move || Event::Catalog(b.fetch_catalog()));
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_sort_apps(move |label| {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() {
                    return;
                }
                let mut s = state.borrow_mut();
                s.sort = SortOrder::from_label(label.as_str());
                s.render(&w);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_open_tags(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            if w.get_busy() || !w.get_has_catalog() {
                return;
            }
            w.set_tags_open(true);
            let s = state.borrow();
            s.render(&w);
            if s.tags.is_none() {
                let b = s.backend.clone();
                begin(&w, "WinGetの索引からタグ一覧を読み取っています…");
                spawn(&tx, move || Event::Tags(b.fetch_tags()));
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_refresh_tags(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            if w.get_busy() {
                return;
            }
            let b = state.borrow().backend.clone();
            begin(&w, "WinGetの索引からタグ一覧を読み取っています…");
            spawn(&tx, move || Event::Tags(b.fetch_tags()));
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_filter_tags(move || {
            if let Some(w) = weak.upgrade() {
                state.borrow().render(&w);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_choose_tag(move |tag| {
            let Some(w) = weak.upgrade() else { return; };
            if w.get_busy() { return; }
            w.set_app_tag(tag.clone());
            w.set_app_list_y(0.0);
            w.set_tags_open(false);
            if w.get_source_consent() { w.invoke_search_tag(tag); }
            else { w.set_message("タグを入力欄へ設定しました。WinGetソースの利用条件を確認してタグ検索を実行してください。".into()); }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_search_tag(move |tag| {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() || !w.get_source_consent() || !w.get_has_catalog() {
                    return;
                }
                begin(&w, "WinGetのタグで絞り込んでいます…");
                let b = state.borrow().backend.clone();
                let tag = tag.trim().to_owned();
                spawn(&tx, move || {
                    let result = b.search_tag(&tag);
                    Event::Tag(result, tag)
                });
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_clear_tag(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() {
                    return;
                }
                let mut s = state.borrow_mut();
                s.tag_matches = None;
                s.tag_query.clear();
                s.render(&w);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_toggle_app(move |key, checked| {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() {
                    return;
                }
                let mut s = state.borrow_mut();
                let k = key.to_string();
                if !s
                    .catalog
                    .apps
                    .iter()
                    .any(|a| a.id == k && !matches!(a.method, InstallMethod::Unavailable { .. }))
                {
                    return;
                }
                s.invalidate(&w);
                if checked {
                    s.selected.insert(k);
                } else {
                    s.selected.remove(&k);
                }
                s.render(&w);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_scan_removals(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            if w.get_busy() || !w.get_supported() {
                return;
            }
            begin(&w, "現在の利用者のWindows標準アプリを検出しています…");
            let b = state.borrow().backend.clone();
            spawn(&tx, move || Event::Removals(b.removable_apps()));
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_filter_removals(move || {
            if let Some(w) = weak.upgrade() {
                state.borrow().render(&w);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_toggle_removal(move |key, checked| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            if w.get_busy() {
                return;
            }
            let mut s = state.borrow_mut();
            let key = key.to_string();
            if !s.removals.iter().any(|a| a.key() == key && a.eligible()) {
                return;
            }
            s.invalidate(&w);
            if checked {
                s.selected_removals.insert(key);
            } else {
                s.selected_removals.remove(&key);
            }
            s.render(&w);
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_toggle_driver(move |key, checked| {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() {
                    return;
                }
                let mut s = state.borrow_mut();
                let k = key.strip_prefix("driver-").unwrap_or(&key).to_owned();
                if !s.drivers.iter().any(|d| d.key() == k && d.eligible()) {
                    return;
                }
                s.invalidate(&w);
                if checked {
                    s.selected_drivers.insert(k);
                } else {
                    s.selected_drivers.remove(&k);
                }
                s.render(&w);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_scan_drivers(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() || !w.get_supported() {
                    return;
                }
                begin(
                    &w,
                    "Windows Updateでドライバーを検索しています。数分かかる場合があります…",
                );
                let b = state.borrow().backend.clone();
                spawn(&tx, move || Event::Drivers(b.search()));
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_show_details(move|key|{if let Some(w)=weak.upgrade(){
            if w.get_busy(){return;}
            let mut s=state.borrow_mut();
            let task = if w.get_step() == 5 { s.session.as_ref().and_then(|session|session.tasks.iter().find(|t|t.target.key()==key.as_str())) } else { None };
            let outcome_text = task.map(|t|format!("結果: {}\n{}\n\n", t.outcome.status.label(), t.outcome.message)).unwrap_or_default();
            let target=task.map(|t|t.target.clone()).or_else(||s.catalog.apps.iter().find(|a|a.id==key.as_str()).map(|app|Target::App{app:app.clone()})).or_else(||s.drivers.iter().find(|d|format!("driver-{}",d.key())==key.as_str()).map(|driver|Target::Driver{driver:driver.clone()})).or_else(||s.removals.iter().find(|a|a.key()==key.as_str()).map(|app|Target::Remove{app:app.clone()}));
            match target {
                Some(Target::Remove{app})=>{s.detail_key=None;s.detail_url=None;w.set_detail_can_open(false);w.set_detail_category_editable(false);w.set_detail_text(format!("{outcome_text}{}",app.details()).into());w.set_details_open(true);}
                Some(Target::Driver{driver})=>{w.set_detail_link_label("Windows Updateのカタログを開く".into());s.detail_key=None;w.set_detail_category_editable(false);s.detail_url=Some(format!("https://www.catalog.update.microsoft.com/Search.aspx?q={}",driver.update_id));w.set_detail_can_open(true);w.set_detail_text(format!("{outcome_text}{}\n提供元: {}\n分類: {}\nハードウェアID: {}\n現在の版: {}\n任意更新: {}\n\n利用条件:\n{}",driver.title,driver.provider,driver.class,driver.hardware_id,driver.current_version,driver.optional,driver.eula).into());w.set_details_open(true);}
                Some(Target::App{app})=>{
                    w.set_detail_link_label(if app.id.starts_with("search-") {"WinGetのパッケージ定義を開く"} else {"配布元のページを開く"}.into());
                    s.detail_key=Some(app.id.clone());w.set_detail_category(app.category.clone().into());w.set_detail_category_editable((app.id.starts_with("custom-")||app.id.starts_with("search-"))&&s.catalog.apps.iter().any(|a|a.id==app.id));
                    s.detail_url=if app.website.is_empty(){None}else{Some(app.website.clone())};w.set_detail_can_open(s.detail_url.is_some());w.set_details_open(true);
                    w.set_detail_text(format!("{outcome_text}{}\n{}\n{}\n参照ページ: {}",app.name,app.description,app.source_label(),app.website).into());
                    if matches!(app.method,InstallMethod::Winget{..})&&!w.get_source_consent(){w.set_message("WinGetの詳細を取得するには、アプリ選択画面でソースの利用条件へ同意してください。公式ページは開けます。".into());return;}
                    begin(&w,"取得元・利用条件を確認しています…");let b=s.backend.clone();spawn(&tx,move||Event::Detail(b.app_details(&app), outcome_text));
                }
                None=>{}
            }
        }});
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_change_detail_category(move |category| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            if w.get_busy() || !APP_CATEGORIES.contains(&category.as_str()) {
                return;
            }
            let mut s = state.borrow_mut();
            let Some(key) = s.detail_key.clone() else {
                return;
            };
            if !(key.starts_with("custom-") || key.starts_with("search-")) {
                return;
            }
            let Some(app) = s.catalog.apps.iter_mut().find(|a| a.id == key.as_str()) else {
                return;
            };
            app.category = category.to_string();
            s.invalidate(&w);
            let saved = s
                .backend
                .store
                .save_profile(&s.profile(w.get_update_existing()));
            s.render(&w);
            if saved.is_ok() {
                w.set_message(
                    format!("「{category}」へ移動しました。選択状態は維持しています。").into(),
                );
            }
            message(&w, saved);
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_open_detail_url(move || {
            if let Some(w) = weak.upgrade()
                && let Some(url) = &state.borrow().detail_url
            {
                message(&w, crate::platform::open_url(url));
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_save_profile(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() {
                    return;
                }
                state.borrow_mut().transfer = None;
                w.set_match_available(false);
                w.set_transfer_import(false);
                w.set_transfer_ready(false);
                w.set_transfer_missing(false);
                w.set_transfer_text("「このPCのインストール済みを読み取る」で、WinGetが識別できるソフトを取得します。\n\n現在の選択を書き出す場合は「選択中のアプリを使う」を選んでください。\n読み取り・書き出しでインストールや削除は行いません。".into());
                w.set_transfer_open(true);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_load_profile(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() {
                    return;
                }
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("移行するアプリのJSONを開く")
                    .add_filter("JSON", &["json"])
                    .pick_file()
                {
                    let mut s = state.borrow_mut();
                    let result = (|| -> Result<()> {
                        let transfer = Transfer::read(&path, &s.base)?;
                        w.set_transfer_import(true);
                        show_transfer(&w, &s.base, &transfer)?;
                        s.transfer = Some(transfer);
                        w.set_transfer_open(true);
                        Ok(())
                    })();
                    message(&w, result);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_scan_installed(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() { return; }
                if !w.get_source_consent() {
                    w.set_transfer_text("WinGetソースの利用条件を確認し、同意にチェックを入れてください。識別時にソースへ接続します。".into());
                    return;
                }
                let mut s = state.borrow_mut();
                s.transfer = None;
                w.set_transfer_ready(false);
                w.set_transfer_missing(false);
                w.set_transfer_text("インストール済みアプリを読み取り中です。ソフトのインストール・削除は行いません…".into());
                begin(&w, "インストール済みアプリを読み取っています…");
                let b = s.backend.clone();
                let catalog = s.base.clone();
                spawn(&tx, move || Event::Installed(b.export_installed(&catalog)));
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_export_selection(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() {
                    return;
                }
                let mut s = state.borrow_mut();
                s.transfer = None;
                w.set_transfer_ready(false);
                w.set_transfer_missing(false);
                let result = (|| -> Result<()> {
                    let transfer = Transfer::selection(&s.catalog, &s.selected)?;
                    show_transfer(&w, &s.base, &transfer)?;
                    s.transfer = Some(transfer);
                    Ok(())
                })();
                if let Err(e) = result {
                    w.set_transfer_text(format!("{e:#}").into());
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_open_matches(move || {
            let Some(w) = weak.upgrade() else { return; };
            if w.get_busy() || w.get_transfer_import() { return; }
            let mut s = state.borrow_mut();
            if s.transfer.as_ref().is_none_or(|t| t.detected.is_empty()) { return; }
            w.set_transfer_open(false);
            w.set_match_open(true);
            w.set_match_source_query("".into());
            w.set_match_only_missing(true);
            s.match_index = None;
            let index = s.filtered_match_sources(&w).first().copied();
            s.choose_match_source(&w, index);
            if s.base.apps.is_empty() {
                if w.get_source_consent() {
                    begin(&w, "名前で照合するためのWinGet一覧を取得しています…");
                    let b = s.backend.clone();
                    spawn(&tx, move || Event::Catalog(b.fetch_catalog()));
                } else {
                    w.set_match_detail_text("書き出し画面へ戻ってソースの利用条件へ同意し、WinGet一覧を取得してください。登録名の検索だけでは候補を取得しません。".into());
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_close_matches(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            if w.get_busy() {
                return;
            }
            let s = state.borrow();
            if let Some(transfer) = &s.transfer {
                match show_transfer(&w, &s.base, transfer) {
                    Ok(()) => {
                        w.set_match_open(false);
                        w.set_transfer_open(true);
                    }
                    Err(e) => w.set_match_detail_text(format!("{e:#}").into()),
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_filter_match_sources(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            if w.get_busy() {
                return;
            }
            let mut s = state.borrow_mut();
            let visible = s.filtered_match_sources(&w);
            if s.match_index.is_none_or(|i| !visible.contains(&i)) {
                s.choose_match_source(&w, visible.first().copied());
            } else {
                s.render_matches(&w);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_choose_match_source(move |key| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            if w.get_busy() {
                return;
            }
            if let Ok(index) = key.parse::<usize>() {
                let mut s = state.borrow_mut();
                if s.transfer
                    .as_ref()
                    .is_some_and(|t| index < t.detected.len())
                {
                    s.choose_match_source(&w, Some(index));
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_filter_matches(move || {
            if let Some(w) = weak.upgrade() {
                state.borrow().render_matches(&w);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_add_name_match(move |key| {
            let Some(w) = weak.upgrade() else { return; };
            if w.get_busy() || !w.get_match_open() { return; }
            let mut s = state.borrow_mut();
            let result = (|| -> Result<()> {
                let index = s.match_index.context("登録名を選んでください")?;
                let app = s.base.apps.iter().find(|a| a.id == key.as_str()).context("候補がありません")?.clone();
                let catalog = s.base.clone();
                s.transfer.as_mut().context("読み取り結果がありません")?.add_name_match(index, &app, &catalog)?;
                w.set_match_detail_text(format!("書き出し対象へ追加しました: {}\n{}\n製品・版が違う場合は別候補を追加し直すか、「追加を取り消す」を選んでください。ここでは導入していません。", app.name, app.source_label()).into());
                s.match_detail_key = None;
                s.render_matches(&w);
                Ok(())
            })();
            if let Err(e) = result { w.set_match_detail_text(format!("{e:#}").into()); }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_undo_name_match(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            if w.get_busy() {
                return;
            }
            let mut s = state.borrow_mut();
            let result = (|| -> Result<()> {
                let index = s.match_index.context("登録名を選んでください")?;
                let catalog = s.base.clone();
                s.transfer
                    .as_mut()
                    .context("読み取り結果がありません")?
                    .remove_name_match(index, &catalog)?;
                s.match_detail_key = None;
                w.set_match_detail_text(
                    "名前からの追加を取り消しました。元の自動照合結果は保持しています。".into(),
                );
                s.render_matches(&w);
                Ok(())
            })();
            if let Err(e) = result {
                w.set_match_detail_text(format!("{e:#}").into());
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_match_details(move |key| {
            let Some(w) = weak.upgrade() else { return; };
            if w.get_busy() { return; }
            if !w.get_source_consent() { w.set_match_detail_text("提供元情報の取得にはWinGetソースの利用条件への同意が必要です。書き出し画面で確認してください。".into()); return; }
            let mut s = state.borrow_mut();
            if let Some(app) = s.base.apps.iter().find(|a| a.id == key.as_str()).cloned() {
                s.match_detail_key = Some(key.to_string());
                w.set_match_detail_text("提供元・利用条件を取得しています…".into());
                begin(&w, "WinGet候補の詳細を確認しています…");
                let b = s.backend.clone();
                let key = key.to_string();
                spawn(&tx, move || Event::MatchDetail(b.details(&app), key));
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_relink_import(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() || !w.get_transfer_import() {
                    return;
                }
                let mut s = state.borrow_mut();
                let base = s.base.clone();
                if let Some(transfer) = &mut s.transfer {
                    let result = (|| -> Result<()> {
                        for (id, location) in transfer.missing_files() {
                            let Some(path) = rfd::FileDialog::new()
                                .set_title(format!("移行元: {location}"))
                                .add_filter("Windows installer", &["exe", "msi"])
                                .pick_file()
                            else {
                                break;
                            };
                            if let Some(app) =
                                transfer.profile.custom_apps.iter_mut().find(|a| a.id == id)
                            {
                                if let InstallMethod::Custom { location, .. } = &mut app.method {
                                    *location = path.to_string_lossy().into_owned();
                                }
                                app.validate()?;
                            }
                        }
                        show_transfer(&w, &base, transfer)
                    })();
                    if let Err(e) = result {
                        w.set_transfer_text(format!("{e:#}").into());
                        w.set_transfer_ready(false);
                    }
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_confirm_transfer(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() || !w.get_transfer_ready() || w.get_transfer_missing() { return; }
                let mut s = state.borrow_mut();
                let result = (|| -> Result<()> {
                    let transfer = s.transfer.as_ref().context("読み取り結果がありません")?;
                    if w.get_transfer_import() {
                        let p = transfer.profile.clone();
                        let mut base = s.base.clone();
                        base.apps.extend(s.catalog.apps.iter().filter(|a| a.id.starts_with("custom-") && !p.custom_apps.iter().any(|incoming| incoming.id == a.id)).cloned());
                        let catalog = p.validate(&base)?;
                        s.backend.store.save_profile(&Profile::capture(&catalog, &p.selected_apps, false))?;
                        s.invalidate(&w);
                        s.catalog = catalog;
                        s.selected = p.selected_apps;
                        s.selected_drivers.clear();
                        s.selected_removals.clear();
                        s.query.clear();
                        s.tag_matches = None;
                        s.tag_query.clear();
                        s.category = "選択中".into();
                        w.set_update_existing(false);
                        w.set_app_list_y(0.);
                        w.set_transfer_open(false);
                        s.render(&w);
                        w.set_message("JSONのアプリを選択に反映しました。不要な項目を外し、実行前に取得元・利用条件を確認してください。".into());
                    } else if let Some(path) = rfd::FileDialog::new().set_title("アプリ一覧をJSONへ保存").add_filter("JSON", &["json"]).set_file_name("installed-apps.json").save_file() {
                        transfer.save(&path)?;
                        w.set_transfer_open(false);
                        w.set_message(format!("JSONを保存しました: {}。別PCの「JSONインポート」で読み込めます。", path.display()).into());
                    }
                    Ok(())
                })();
                message(&w, result);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_browse_installer(move || {
            if let Some(w) = weak.upgrade()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Windows installer", &["exe", "msi"])
                    .pick_file()
            {
                w.set_custom_location(path.to_string_lossy().to_string().into());
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_add_custom(move || {
            if let Some(w) = weak.upgrade() {
                let mut s = state.borrow_mut();
                let result = (|| -> Result<()> {
                    let location = w.get_custom_location().trim().to_owned();
                    let args: Vec<String> = serde_json::from_str(w.get_custom_args().as_str())
                        .context("引数をJSONの文字列配列で入力してください。例: []")?;
                    let website = if location.starts_with("https://") {
                        let u = validate_https(&location)?;
                        format!("{}/", u.origin().ascii_serialization())
                    } else {
                        String::new()
                    };
                    let hash = w.get_custom_hash().trim().to_owned();
                    let app = AppEntry {
                        id: format!("custom-{}", uuid::Uuid::new_v4()),
                        name: w.get_custom_name().trim().into(),
                        category: w.get_custom_category().to_string(),
                        subcategory: "独自登録".into(),
                        description:
                            "利用者が登録した対話型インストーラー。実行前に内容を確認してください。"
                                .into(),
                        publisher: "利用者登録".into(),
                        website,
                        dependencies: vec![],
                        method: InstallMethod::Custom {
                            location,
                            args,
                            sha256: if hash.is_empty() { None } else { Some(hash) },
                        },
                    };
                    app.validate()?;
                    s.catalog.apps.push(app);
                    s.invalidate(&w);
                    s.backend
                        .store
                        .save_profile(&s.profile(w.get_update_existing()))?;
                    s.render(&w);
                    w.set_custom_open(false);
                    w.set_custom_name("".into());
                    w.set_custom_location("".into());
                    w.set_custom_args("[]".into());
                    w.set_custom_hash("".into());
                    w.set_custom_category(APP_CATEGORIES[0].into());
                    w.set_message("独自登録を追加しました。まだ選択・実行していません。".into());
                    Ok(())
                })();
                message(&w, result);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_navigate(move |step| {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() || !(0..=5).contains(&step) {
                    return;
                }
                let mut s = state.borrow_mut();
                s.invalidate(&w);
                if step == 4 {
                    let result = start_review(&w, &mut s, &tx);
                    message(&w, result);
                } else {
                    w.set_step(step);
                    s.render(&w);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_resume_installation(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy() || !w.get_supported() {
                    return;
                }
                let mut s = state.borrow_mut();
                if let Some(session) = s.session.clone() {
                    if !session.tasks.iter().any(|t| matches!(t.outcome.status, Status::Pending | Status::Running | Status::RebootRequired)) {
                        w.set_step(5);
                        s.render(&w);
                        w.set_message("前回の結果を表示しています。自動再開する未実行項目はありません。失敗・確認不能・操作待ちは確認後に選び直してください。".into());
                        return;
                    }
                    s.resume = Some(session);
                    w.set_install_consent(false);
                    message(&w, start_review(&w, &mut s, &tx));
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        window.on_run_installation(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_busy()
                    || w.get_details_open()
                    || w.get_about_open()
                    || w.get_custom_open()
                    || w.get_log_open()
                    || w.get_tags_open()
                    || w.get_transfer_open()
                    || w.get_match_open()
                    || !w.get_install_consent()
                    || !w.get_supported()
                    || (w.get_has_removals() && !w.get_removal_consent())
                {
                    return;
                }
                let mut s = state.borrow_mut();
                let result = (|| -> Result<()> {
                    ensure!(s.reviewed, "実行内容と利用条件の確認が完了していません");
                    let session = if let Some(resume) = s.resume.take() {
                        resume
                    } else {
                        let (apps, drivers) = s.selection()?;
                        Session::with_removals(
                            apps,
                            drivers,
                            s.removal_selection(),
                            w.get_update_existing(),
                        )?
                    };
                    s.backend
                        .store
                        .save_profile(&s.profile(w.get_update_existing()))?;
                    s.backend.store.save_session(&session)?;
                    s.cancel.store(false, Ordering::Relaxed);
                    s.session = Some(session.clone());
                    s.log.clear();
                    w.set_step(5);
                    w.set_running(true);
                    begin(&w, "選択した項目の処理を開始しました。");
                    s.render(&w);
                    let b = s.backend.clone();
                    let cancel = s.cancel.clone();
                    let events = tx.clone();
                    spawn(&tx, move || {
                        Event::Finished(runner::execute(&b, session, &cancel, |e| {
                            let _ = events.send(Event::Run(e));
                        }))
                    });
                    Ok(())
                })();
                message(&w, result);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_cancel_installation(move || {
            state.borrow().cancel.store(true, Ordering::Relaxed);
            if let Some(w) = weak.upgrade() {
                w.set_message("中止を要求しました。実行中の処理が終わるまで待っています…".into());
            }
        });
    }
    {
        let weak = window.as_weak();
        let b = backend.clone();
        window.on_open_logs(move || {
            if let Some(w) = weak.upgrade() {
                message(&w, crate::platform::open_folder(&b.store.root.join("logs")));
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_open_release_page(move || {
            if let Some(w) = weak.upgrade() {
                message(
                    &w,
                    crate::platform::open_url(
                        "https://github.com/SHIN-DATA-CENTER/application-pre-installer/releases",
                    ),
                );
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.window().on_close_requested(move || {
            if let Some(w) = weak.upgrade()
                && w.get_running()
            {
                state.borrow().cancel.store(true, Ordering::Relaxed);
                w.set_message(
                    "中止を要求しました。実行中の処理が完了してから閉じてください。".into(),
                );
                return slint::CloseRequestResponse::KeepWindowShown;
            }
            slint::CloseRequestResponse::HideWindow
        });
    }

    let timer = Timer::default();
    let weak = window.as_weak();
    let state_for_timer = state.clone();
    let tx_for_timer = tx.clone();
    timer.start(TimerMode::Repeated,Duration::from_millis(120),move||{
        let Some(w)=weak.upgrade()else{return;};
        for event in rx.try_iter(){
            let mut s=state_for_timer.borrow_mut();
            match event {
                Event::MatchDetail(result, key) => {
                    w.set_busy(false);
                    if s.match_detail_key.as_deref() == Some(key.as_str()) {
                        w.set_match_detail_text(match result { Ok(text) => text, Err(e) => format!("詳細を取得できませんでした。\n{e:#}") }.into());
                    }
                }
                Event::Installed(result) => {
                    w.set_busy(false);
                    match result {
                        Ok(transfer) => match show_transfer(&w, &s.base, &transfer) {
                            Ok(()) => {
                                s.transfer = Some(transfer);
                                w.set_message("読み取りが終わりました。対象と警告を確認し、JSONを保存してください。".into());
                            }
                            Err(e) => w.set_transfer_text(format!("{e:#}").into()),
                        },
                        Err(e) => {
                            w.set_transfer_text(format!("読み取りを完了できませんでした。\n{e:#}").into());
                            w.set_message("インストール済みアプリを読み取れませんでした。エクスポート画面で理由を確認してください。".into());
                        }
                    }
                }
                Event::Environment(result) => {
                    w.set_busy(false);
                    match result {
                        Ok(env) => {
                            w.set_environment_text(env.summary().into());
                            w.set_supported(env.supported());
                            s.environment = Some(*env);
                            w.set_message(if s.backend.demo {
                                "デモモードです。インストールやPCの設定変更は行いません。"
                            } else {
                                "環境を確認しました。次へ進んで必要な項目を選んでください。"
                            }.into());
                        }
                        Err(e) => {
                            s.environment = None;
                            w.set_supported(false);
                            w.set_environment_text(format!("環境確認に失敗しました。\n{e:#}").into());
                            w.set_message("環境確認をやり直してください。".into());
                        }
                    }
                }
                Event::Prepared(result)=>{w.set_busy(false);match result{Ok(text)=>{s.log.push_str(&text);begin(&w,"WinGetの準備後の状態を確認しています…");let b=s.backend.clone();spawn(&tx_for_timer,move||Event::Environment(b.environment().map(Box::new)));},Err(e)=>w.set_message(format!("{e:#}").into())}}
                Event::Catalog(result) => {
                    w.set_busy(false);
                    match result {
                        Ok(cached) => {
                            let count=cached.catalog.apps.len();
                            match s.profile(w.get_update_existing()).validate(&cached.catalog) {
                                Ok(catalog) => { s.base=cached.catalog; s.catalog=catalog; s.tag_matches=None; s.tag_query.clear(); s.catalog_status=format!("WinGetから取得: {count} 件"); s.invalidate(&w); w.set_message(if s.backend.demo {format!("デモ用の架空リスト {count} 件です。実際のダウンロード・導入は行いません。")} else {format!("WinGet一覧 {count} 件を取得しました。選択した項目だけを導入します。")}.into()); },
                                Err(e) => w.set_message(format!("一覧と選択内容の統合に失敗しました: {e:#}").into()),
                            }
                        }
                        Err(e) => {
                            if w.get_match_open() { w.set_match_detail_text(format!("一覧の取得に失敗しました。前回の一覧は保持します。\n{e:#}").into()); }
                            w.set_message(format!("一覧の取得に失敗しました。現在の一覧と選択は保持します。\n{e:#}").into());
                        }
                    }
                }
                Event::Tag(result, tag) => {
                    w.set_busy(false);
                    match result {
                        Ok(apps) => {
                            let count=apps.len();
                            let mut known: BTreeSet<_>=s.catalog.apps.iter().map(|a|a.id.clone()).collect();
                            s.tag_matches=Some(apps.iter().map(|a|a.id.clone()).collect()); s.tag_query=tag;
                            for app in apps {if known.insert(app.id.clone()){s.catalog.apps.push(app);}}
                            w.set_message(format!("タグ検索結果 {count} 件。タグは各パッケージの検索用語で、統一カテゴリーではありません。").into());
                        }
                        Err(e) => w.set_message(format!("タグ検索に失敗しました: {e:#}").into()),
                    }
                }
                Event::Tags(result) => {
                    w.set_busy(false);
                    match result {
                        Ok(index) => {
                            let count = index.tags.len();
                            s.tag_status = format!("取得したタグ {count} 件 / WinGet索引 {}", index.source_version);
                            s.tags = Some(index);
                            w.set_message("タグ一覧を取得しました。選ぶとWinGetでタグ検索します。".into());
                        }
                        Err(e) => {
                            s.tag_status = format!("タグ一覧を取得できません: {e:#}");
                            w.set_message("タグ一覧の取得に失敗しました。前回の一覧とアプリの選択は保持します。手入力のタグ検索も利用できます。".into());
                        }
                    }
                }
                Event::Removals(result) => {
                    w.set_busy(false);
                    match result {
                        Ok(mut apps) => {
                            apps.sort_by_key(|a| (!a.eligible(), a.title().to_lowercase()));
                            let eligible = apps.iter().filter(|a| a.eligible()).count();
                            let blocked = apps.len() - eligible;
                            s.removals = apps; s.selected_removals.clear(); s.invalidate(&w);
                            w.set_removal_summary(format!("削除候補 {eligible} 件 / 対象外 {blocked} 件。現在の利用者のみ・すべて未選択です。").into());
                            w.set_message("検出が完了しました。必要なものだけ選択してください。まだ削除していません。".into());
                        }
                        Err(e) => w.set_message(format!("標準アプリの検出に失敗しました。現在の候補と選択は保持します。\n{e:#}").into()),
                    }
                }
                Event::Drivers(result)=>{w.set_busy(false);match result{Ok(scan)=>{s.drivers=scan.candidates;s.selected_drivers.clear();s.invalidate(&w);let count=s.drivers.len();let problems=s.environment.as_ref().map(|e|e.devices.iter().filter(|d|d.problem_code!=0).count()).unwrap_or(0);w.set_driver_summary(format!("適用候補 {count} 件 / 安全条件で除外 {} 件 / 問題状態のデバイス {problems} 件。すべて未選択です。",scan.excluded).into());w.set_message(if count==0{"対象ドライバーの候補はありません。デバイスの正常動作は別途確認してください。"}else{"ドライバー候補を取得しました。必要な項目を選んでください。"}.into());},Err(e)=>{w.set_driver_summary(format!("検索失敗: {e:#}").into());w.set_message("Windows Updateの通信状態・サービス・組織のポリシーを確認してください。".into());}}}
                Event::Detail(result, outcome_text)=>{w.set_busy(false);let details=match result{Ok(info)=>{if let Some(url)=info.publisher_url{s.detail_url=Some(url);w.set_detail_can_open(true);w.set_detail_link_label("配布元のページを開く".into());} info.text},Err(e)=>format!("取得できませんでした。\n{e:#}\n\n参照ページで配布元・利用条件を確認してください。")};w.set_detail_text(format!("{outcome_text}{details}").into());}
                Event::Review(result)=>{w.set_busy(false);match result{Ok(text)=>{s.reviewed=true;w.set_review_text(text.into());w.set_message("取得元と利用条件を確認し、同意する場合はチェックを入れてください。".into());},Err(e)=>{s.reviewed=false;w.set_review_text(format!("確認を完了できませんでした。\n{e:#}\n\n戻って対象を見直すか、詳細の公式ページから手動導入してください。").into());w.set_message("情報を取得できない項目があるため、導入は開始できません。".into());}}}
                Event::Run(event)=>match event{RunEvent::Changed(session)=>{s.session=Some(session);},RunEvent::Log(text)=>{s.log.push_str(&text);s.log.push('\n');if s.log.len()>200_000{let at=s.log.char_indices().find(|(i,_)|*i>=s.log.len()-150_000).map(|(i,_)|i).unwrap_or(0);s.log=s.log[at..].to_owned();}}},
                Event::Finished(result)=>{w.set_busy(false);w.set_running(false);s.reviewed=false;w.set_install_consent(false);match result{Ok(session)=>{let reboot=session.tasks.iter().any(|t|t.outcome.status==Status::RebootRequired);s.session=Some(session);w.set_message(if reboot{"再起動が必要です。再起動後に前回の処理を確認・再開してください。"}else{"処理が終了しました。結果と詳細ログを確認してください。"}.into());},Err(e)=>w.set_message(format!("処理を停止しました。履歴を保存できない場合は、再実行前に導入状態を確認してください。\n{e:#}").into())}}
            }
            s.render(&w);
        }
    });
    state.borrow().render(&window);
    window.invoke_refresh_environment();
    window.run()?;
    Ok(())
}

fn show_transfer(window: &AppWindow, base: &Catalog, transfer: &Transfer) -> Result<()> {
    window.set_match_available(!window.get_transfer_import() && !transfer.detected.is_empty());
    let mut text = transfer.preview(base)?;
    let missing = transfer.missing_files();
    if !missing.is_empty() {
        text.push_str("\n【見つからないインストーラー】\n別PCへファイルをコピーし、場所を指定してください。\n");
        for (_, location) in &missing {
            text.push_str(&format!("{location}\n"));
        }
    }
    window.set_transfer_text(text.into());
    window.set_transfer_ready(!transfer.profile.selected_apps.is_empty());
    window.set_transfer_missing(window.get_transfer_import() && !missing.is_empty());
    Ok(())
}

fn start_review(window: &AppWindow, state: &mut State, tx: &mpsc::Sender<Event>) -> Result<()> {
    let (apps, drivers) = state.selection()?;
    let removals = state.removal_selection();
    ensure!(
        !apps.is_empty() || !drivers.is_empty() || !removals.is_empty(),
        "少なくとも1件選択してください。削除・ドライバー検索は省略できます。"
    );
    ensure!(
        window.get_source_consent()
            || !apps
                .iter()
                .any(|a| matches!(a.method, InstallMethod::Winget { .. })),
        "アプリ選択画面でWinGetソースの利用条件への同意が必要です"
    );
    state.reviewed = false;
    window.set_install_consent(false);
    window.set_removal_consent(false);
    window.set_has_removals(!removals.is_empty());
    window.set_step(4);
    window.set_confirmation_text(
        format!(
            "{}導入 {} 件（依存含む） / 削除 {} 件 / ドライバー {} 件 / 既存: {}",
            if state.resume.is_some() {
                "前回の処理を再確認 · "
            } else {
                ""
            },
            apps.len(),
            removals.len(),
            drivers.len(),
            if state
                .resume
                .as_ref()
                .map(|s| s.update_existing)
                .unwrap_or_else(|| window.get_update_existing())
            {
                "更新する"
            } else {
                "スキップ"
            }
        )
        .into(),
    );
    window.set_review_text("取得元・利用条件・依存関係を確認しています…".into());
    begin(window, "選択した項目の配布情報を取得しています…");
    let b = state.backend.clone();
    spawn(tx, move || {
        Event::Review((|| {
            ensure!(
                removals.iter().all(RemovableApp::eligible),
                "削除対象を再検出してください"
            );
            let mut text = String::new();
            for app in removals {
                text.push_str(&app.details());
                text.push_str("\n\n────────────────────────\n\n");
            }
            text.push_str(&b.review(&apps, &drivers)?);
            Ok(text)
        })())
    });
    Ok(())
}
