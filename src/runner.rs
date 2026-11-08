use crate::{
    model::*,
    platform::{Backend, DriverProvider, Installer},
    uninstall::AppRemover,
};
use anyhow::Result;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug)]
pub enum RunEvent {
    Changed(Session),
    Log(String),
}

pub fn execute(
    backend: &Backend,
    session: Session,
    cancel: &AtomicBool,
    emit: impl FnMut(RunEvent),
) -> Result<Session> {
    execute_with(backend, &backend.store, session, cancel, emit)
}

fn execute_with(
    backend: &(impl Installer + DriverProvider + AppRemover),
    store: &crate::store::Store,
    mut session: Session,
    cancel: &AtomicBool,
    mut emit: impl FnMut(RunEvent),
) -> Result<Session> {
    session.validate()?;
    store.save_session(&session)?;
    let mut log = |message: String| {
        let _ = store.log(&session.id, &message);
        emit(RunEvent::Log(message));
    };
    log(format!(
        "実行開始: {} 件 / セッション {}",
        session.tasks.len(),
        session.id
    ));
    // A crashed/rebooted task is re-inspected. Never blindly replay an installer.
    for task in &mut session.tasks {
        if matches!(
            task.outcome.status,
            Status::Running | Status::RebootRequired
        ) {
            task.outcome = match &task.target {
                Target::App { app } => match backend.installed(app) {
                    Ok(true) if !session.update_existing => Outcome::new(
                        Status::Success,
                        "再開時にインストール済み状態を確認しました",
                    ),
                    _ => Outcome::new(
                        Status::Unknown,
                        "中断前の処理完了を確認できません。確認後に再選択してください",
                    ),
                },
                Target::Driver { driver } => backend.verify(driver).unwrap_or_else(|e| {
                    Outcome::new(Status::Unknown, format!("再検査に失敗しました: {e:#}"))
                }),
                Target::Remove { app } => backend.verify_removal(app).unwrap_or_else(|e| {
                    Outcome::new(
                        Status::Unknown,
                        format!("削除状態の再検査に失敗しました: {e:#}"),
                    )
                }),
            };
        }
    }
    store.save_session(&session)?;
    emit(RunEvent::Changed(session.clone()));

    // Removals run before installations so a selected new app is never removed afterwards.
    for index in 0..session.tasks.len() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if session.tasks[index].outcome.status != Status::Pending {
            continue;
        }
        let Target::Remove { app } = &session.tasks[index].target else {
            continue;
        };
        let app = app.clone();
        session.tasks[index].outcome = Outcome::new(Status::Running, "削除条件を再確認しています");
        store.save_session(&session)?;
        emit(RunEvent::Changed(session.clone()));
        session.tasks[index].outcome = backend
            .remove_app(&app)
            .unwrap_or_else(|e| Outcome::new(Status::Failed, format!("{e:#}")));
        let task = &session.tasks[index];
        let message = format!(
            "削除 / {}: {} / {}",
            app.title(),
            task.outcome.status.label(),
            task.outcome.message
        );
        store.log(&session.id, &message)?;
        emit(RunEvent::Log(message));
        store.save_session(&session)?;
        emit(RunEvent::Changed(session.clone()));
    }

    let pending_drivers: Vec<_> = session
        .tasks
        .iter()
        .filter(|t| t.outcome.status == Status::Pending)
        .filter_map(|t| match &t.target {
            Target::Driver { driver } => Some(driver.clone()),
            _ => None,
        })
        .collect();
    let mut stop_for_reboot = false;
    if !pending_drivers.is_empty() && !cancel.load(Ordering::Relaxed) {
        // Persist intent before requesting UAC; a crash cannot turn uncertainty into a duplicate run.
        for t in &mut session.tasks {
            if matches!(t.target, Target::Driver { .. }) && t.outcome.status == Status::Pending {
                t.outcome = Outcome::new(Status::Running, "管理者処理で適合性を再確認します");
            }
        }
        store.save_session(&session)?;
        emit(RunEvent::Changed(session.clone()));
        let result = backend.install(&pending_drivers, cancel, &mut |progress| {
            for task in &mut session.tasks {
                if let Target::Driver { driver } = &task.target {
                    if let Some(o) = progress.outcomes.get(&driver.key()) {
                        task.outcome = o.clone();
                    } else if progress.running.as_deref() == Some(&driver.key()) {
                        task.outcome =
                            Outcome::new(Status::Running, "ダウンロード・インストール中");
                    }
                }
            }
            // If journaling fails, the outer save will fail before any app starts.
            let _ = store.save_session(&session);
            emit(RunEvent::Changed(session.clone()));
        });
        match result {
            Ok(progress) => {
                stop_for_reboot = progress
                    .outcomes
                    .values()
                    .any(|o| o.status == Status::RebootRequired);
                for t in &mut session.tasks {
                    if let Target::Driver { driver } = &t.target {
                        if let Some(outcome) = progress.outcomes.get(&driver.key()) {
                            t.outcome = outcome.clone();
                        } else if t.outcome.status == Status::Running {
                            t.outcome =
                                Outcome::new(Status::Pending, "再起動または中止のため未実行");
                        }
                    }
                }
            }
            Err(e) => {
                for t in &mut session.tasks {
                    if matches!(t.target, Target::Driver { .. })
                        && t.outcome.status == Status::Running
                    {
                        t.outcome = Outcome::new(
                            Status::Unknown,
                            format!("管理者処理の完了を確認できません: {e:#}"),
                        );
                    }
                }
            }
        }
        store.save_session(&session)?;
        emit(RunEvent::Changed(session.clone()));
    }
    if !stop_for_reboot {
        for index in 0..session.tasks.len() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if session.tasks[index].outcome.status != Status::Pending {
                continue;
            }
            let app = match &session.tasks[index].target {
                Target::App { app } => app.clone(),
                _ => continue,
            };
            if let Some(reason) = dependency_block(&session, &app) {
                session.tasks[index].outcome = Outcome::new(Status::AwaitingUser, reason);
            } else {
                session.tasks[index].outcome = Outcome::new(Status::Running, "処理中");
                store.save_session(&session)?;
                emit(RunEvent::Changed(session.clone()));
                let id = session.id.clone();
                let result = backend.install_app(&app, session.update_existing, &mut |message| {
                    let _ = store.log(&id, &message);
                    emit(RunEvent::Log(message));
                });
                session.tasks[index].outcome =
                    result.unwrap_or_else(|e| Outcome::new(Status::Failed, format!("{e:#}")));
            }
            let t = &session.tasks[index];
            let message = format!(
                "{}: {} / {}",
                t.target.name(),
                t.outcome.status.label(),
                t.outcome.message
            );
            store.log(&session.id, &message)?;
            emit(RunEvent::Log(message));
            store.save_session(&session)?;
            emit(RunEvent::Changed(session.clone()));
            if t.outcome.status == Status::RebootRequired {
                break;
            }
        }
    }
    store.save_session(&session)?;
    let message = if cancel.load(Ordering::Relaxed) {
        "中止要求を反映しました。未実行項目は保存されています"
    } else if session
        .tasks
        .iter()
        .any(|t| t.outcome.status == Status::RebootRequired)
    {
        "利用者による再起動が必要です。再起動後に「前回の処理を再開」を選んでください"
    } else {
        "処理が終了しました。操作待ち・確認不能・失敗の項目を確認してください"
    };
    store.log(&session.id, message)?;
    emit(RunEvent::Log(message.into()));
    Ok(session)
}

fn dependency_block(session: &Session, app: &AppEntry) -> Option<String> {
    let results: BTreeMap<_, _> = session
        .tasks
        .iter()
        .map(|t| (t.target.key(), &t.outcome.status))
        .collect();
    for dependency in &app.dependencies {
        if !results
            .get(dependency)
            .is_some_and(|s| s.satisfies_dependency())
        {
            return Some(format!(
                "依存項目 {dependency} が完了していないため、導入を保留しました"
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Backend;
    use crate::store::Store;
    use std::{collections::BTreeSet, fs, sync::atomic::AtomicBool};

    #[derive(Default)]
    struct FakeInstaller {
        calls: std::cell::RefCell<Vec<String>>,
        failed: BTreeSet<String>,
        present: BTreeSet<String>,
        reboot: Option<String>,
        removed: std::cell::RefCell<Vec<String>>,
    }
    impl Installer for FakeInstaller {
        fn installed(&self, app: &AppEntry) -> Result<bool> {
            Ok(self.present.contains(&app.id))
        }
        fn install_app(
            &self,
            app: &AppEntry,
            _: bool,
            _: &mut dyn FnMut(String),
        ) -> Result<Outcome> {
            self.calls.borrow_mut().push(app.id.clone());
            if self.failed.contains(&app.id) {
                anyhow::bail!("simulated network failure");
            }
            Ok(Outcome::new(
                if self.reboot.as_deref() == Some(&app.id) {
                    Status::RebootRequired
                } else {
                    Status::Success
                },
                "fake installer",
            ))
        }
    }
    impl DriverProvider for FakeInstaller {
        fn search(&self) -> Result<crate::platform::DriverScan> {
            Ok(Default::default())
        }
        fn verify(&self, _: &DriverCandidate) -> Result<Outcome> {
            Ok(Outcome::new(Status::Unknown, "not installed"))
        }
        fn install(
            &self,
            _: &[DriverCandidate],
            _: &AtomicBool,
            _: &mut dyn FnMut(&crate::platform::DriverProgress),
        ) -> Result<crate::platform::DriverProgress> {
            anyhow::bail!("unexpected driver installation");
        }
    }
    impl AppRemover for FakeInstaller {
        fn remove_app(&self, app: &crate::uninstall::RemovableApp) -> Result<Outcome> {
            self.removed.borrow_mut().push(app.family.clone());
            if self.failed.contains(&app.family) {
                anyhow::bail!("simulated removal failure");
            }
            Ok(Outcome::new(Status::Success, "fake removal"))
        }
        fn verify_removal(&self, app: &crate::uninstall::RemovableApp) -> Result<Outcome> {
            Ok(Outcome::new(
                if self.present.contains(&app.family) {
                    Status::Unknown
                } else {
                    Status::Success
                },
                "fake reinspection",
            ))
        }
    }

    #[test]
    fn removal_failure_continues_and_interrupted_removal_is_inspected_without_replay() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(root.join("logs")).unwrap();
        let store = Store { root: root.clone() };
        let removals = Backend {
            store: store.clone(),
            demo: true,
        }
        .removable_apps()
        .unwrap();
        let backend = FakeInstaller {
            failed: BTreeSet::from([removals[0].family.clone()]),
            ..Default::default()
        };
        let mut session = Session::with_removals(
            Catalog::fixture().apps[..1].to_vec(),
            vec![],
            removals[..2].to_vec(),
            false,
        )
        .unwrap();
        session.tasks[1].outcome.status = Status::Running;
        let result =
            execute_with(&backend, &store, session, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(result.tasks[0].outcome.status, Status::Failed);
        assert_eq!(result.tasks[1].outcome.status, Status::Success);
        assert_eq!(result.tasks[2].outcome.status, Status::Success);
        assert_eq!(*backend.removed.borrow(), vec![removals[0].family.clone()]);
        let result =
            execute_with(&backend, &store, result, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(backend.removed.borrow().len(), 1);
        assert_eq!(result.tasks[1].outcome.status, Status::Success);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failure_blocks_dependents_but_continues_independent_apps() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(root.join("logs")).unwrap();
        let mut apps = Catalog::fixture().apps[..3].to_vec();
        apps[1].dependencies = vec![apps[0].id.clone()];
        let backend = FakeInstaller {
            failed: BTreeSet::from([apps[0].id.clone()]),
            ..Default::default()
        };
        let expected_calls = vec![apps[0].id.clone(), apps[2].id.clone()];
        let session = Session::new(apps, vec![], false).unwrap();
        let result = execute_with(
            &backend,
            &Store { root: root.clone() },
            session,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(result.tasks[0].outcome.status, Status::Failed);
        assert_eq!(result.tasks[1].outcome.status, Status::AwaitingUser);
        assert_eq!(result.tasks[2].outcome.status, Status::Success);
        assert_eq!(*backend.calls.borrow(), expected_calls);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reboot_pauses_queue_and_resume_checks_state_before_continuing() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(root.join("logs")).unwrap();
        let apps = Catalog::fixture().apps[..2].to_vec();
        let first = apps[0].id.clone();
        let backend = FakeInstaller {
            reboot: Some(first.clone()),
            ..Default::default()
        };
        let store = Store { root: root.clone() };
        let session = Session::new(apps, vec![], false).unwrap();
        let result =
            execute_with(&backend, &store, session, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(result.tasks[0].outcome.status, Status::RebootRequired);
        assert_eq!(result.tasks[1].outcome.status, Status::Pending);
        let after_reboot = FakeInstaller {
            present: BTreeSet::from([first]),
            ..Default::default()
        };
        let result = execute_with(
            &after_reboot,
            &store,
            store.load_session().unwrap().unwrap(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(result.tasks[0].outcome.status, Status::Success);
        assert_eq!(result.tasks[1].outcome.status, Status::Success);
        assert_eq!(after_reboot.calls.borrow().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interrupted_upgrade_is_not_success_just_because_old_version_exists() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(root.join("logs")).unwrap();
        let apps = Catalog::fixture().apps[..1].to_vec();
        let backend = FakeInstaller {
            present: BTreeSet::from([apps[0].id.clone()]),
            ..Default::default()
        };
        let mut session = Session::new(apps, vec![], true).unwrap();
        session.tasks[0].outcome.status = Status::Running;
        let result = execute_with(
            &backend,
            &Store { root: root.clone() },
            session,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(result.tasks[0].outcome.status, Status::Unknown);
        assert!(backend.calls.borrow().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn resume_never_replays_successful_or_uncertain_tasks() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(root.join("logs")).unwrap();
        let backend = Backend {
            store: Store { root: root.clone() },
            demo: true,
        };
        let catalog = Catalog::fixture();
        let mut session = Session::new(catalog.apps[..3].to_vec(), vec![], false).unwrap();
        session.tasks[0].outcome = Outcome::new(Status::Success, "done");
        session.tasks[1].outcome = Outcome::new(Status::Running, "interrupted");
        let result = execute(&backend, session, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(result.tasks[0].outcome.status, Status::Success);
        assert_eq!(result.tasks[1].outcome.status, Status::Unknown);
        assert_eq!(result.tasks[2].outcome.status, Status::Unknown);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cancellation_leaves_unstarted_tasks_pending() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(root.join("logs")).unwrap();
        let b = Backend {
            store: Store { root: root.clone() },
            demo: true,
        };
        let s = Session::new(Catalog::fixture().apps[..1].to_vec(), vec![], false).unwrap();
        let result = execute(&b, s, &AtomicBool::new(true), |_| {}).unwrap();
        assert_eq!(result.tasks[0].outcome.status, Status::Pending);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn dependencies_require_verified_completion() {
        let mut apps = Catalog::fixture().apps[..2].to_vec();
        apps[1].dependencies = vec![apps[0].id.clone()];
        let mut s = Session::new(apps, vec![], false).unwrap();
        assert!(
            dependency_block(
                &s,
                match &s.tasks[1].target {
                    Target::App { app } => app,
                    _ => unreachable!(),
                }
            )
            .is_some()
        );
        s.tasks[0].outcome.status = Status::Success;
        assert!(
            dependency_block(
                &s,
                match &s.tasks[1].target {
                    Target::App { app } => app,
                    _ => unreachable!(),
                }
            )
            .is_none()
        );
    }
}
