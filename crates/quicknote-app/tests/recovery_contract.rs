//! 通过真实 SQLite 与文件故障验证备份、隔离、恢复和永久清除合同。

use quicknote_app::{
    Application, ApplicationConfig, ApplicationError, EditorIntent, NoteAction, RecoverySession,
};
use rusqlite::Connection;
use std::fs;
use tempfile::TempDir;

/// 到期夹具使用绝对时间，不依赖等待测试机器的时钟。
fn expire_trash(directory: &TempDir) {
    let database = Connection::open(directory.path().join("quicknote.db")).unwrap();
    database
        .execute(
            "UPDATE notes SET trashed_at_ms = 0 WHERE lifecycle = 'trashed'",
            [],
        )
        .unwrap();
}

#[test]
fn permanent_clear_scrubs_notification_content_and_receipts_in_database_and_backup() {
    use quicknote_app::platform::{
        PlatformCommand, PlatformServices, test_support::TestPlatformServices,
    };
    use quicknote_app::{ReminderActivation, ReminderActivationAction, ReminderCoordinationReason};

    // 手动删除和到期维护必须执行相同的正文清理合同。
    for expired in [false, true] {
        let directory = TempDir::new().unwrap();
        let platform = TestPlatformServices::new(directory.path());
        let app = Application::open(ApplicationConfig::new(directory.path())).unwrap();
        app.edit(EditorIntent::ReplaceBody("待永久清除的私密正文".into()))
            .unwrap();
        let note = app.edit(EditorIntent::Flush).unwrap().note_id.unwrap();
        app.set_reminder(&platform, note, Some(4_000_000_000_000))
            .unwrap();
        let command = platform.recorded_commands().unwrap().pop().unwrap();
        if let PlatformCommand::UpsertNotification {
            reminder_id,
            trigger_version,
            snooze_activation_key,
            snooze_minutes,
            ..
        } = command
        {
            app.handle_reminder_activation(
                &platform,
                ReminderActivation {
                    activation_key: snooze_activation_key,
                    reminder_id,
                    trigger_version,
                    action: ReminderActivationAction::Snooze,
                    snooze_minutes: Some(snooze_minutes),
                },
            )
            .unwrap();
        } else {
            panic!("应收到提醒计划");
        }
        app.apply_note_action(NoteAction::Archive(note)).unwrap();
        app.apply_note_action(NoteAction::MoveToTrash(note))
            .unwrap();
        if expired {
            expire_trash(&directory);
            assert_eq!(app.maintain_trash().unwrap(), 1);
        } else {
            app.apply_note_action(NoteAction::PermanentlyDelete(note))
                .unwrap();
        }
        // 备份状态请求也是单写者屏障，保证强制快照已完成。
        assert!(app.backup_status().unwrap().warning.is_none());
        let paths = [
            directory.path().join("quicknote.db"),
            backups(&directory)[0].clone(),
        ];
        for path in paths {
            let database = Connection::open(path).unwrap();
            for query in [
                "SELECT COUNT(*) FROM reminder_outbox WHERE projection_kind = 'schedule'",
                "SELECT COUNT(*) FROM activation_receipts",
                "SELECT COUNT(*) FROM notes",
            ] {
                assert_eq!(
                    database
                        .query_row(query, [], |row| row.get::<_, i64>(0))
                        .unwrap(),
                    0,
                    "{query}"
                );
            }
            let payloads: String = database
                .query_row(
                    "SELECT COALESCE(group_concat(payload_json), '') FROM reminder_outbox",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!payloads.contains("私密正文"));
        }
        // 不含正文的取消意图仍须到达平台，不能把旧计划遗留给系统。
        app.coordinate_reminders(&platform, None, ReminderCoordinationReason::Continuous)
            .unwrap();
        assert!(platform.scheduled_notifications().unwrap().is_empty());
    }
}

#[cfg(windows)]
#[test]
fn locked_backup_defers_startup_purge_but_keeps_healthy_notes_available() {
    use std::os::windows::fs::OpenOptionsExt;
    let directory = TempDir::new().unwrap();
    let config = ApplicationConfig::new(directory.path());
    let app = Application::open(config.clone()).unwrap();
    app.edit(EditorIntent::ReplaceBody("到期但暂不能清理".into()))
        .unwrap();
    let expired = app.edit(EditorIntent::Flush).unwrap().note_id.unwrap();
    app.apply_note_action(NoteAction::Archive(expired)).unwrap();
    app.apply_note_action(NoteAction::MoveToTrash(expired))
        .unwrap();
    app.edit(EditorIntent::ReplaceBody("仍然可编辑的健康便签".into()))
        .unwrap();
    app.edit(EditorIntent::Flush).unwrap();
    app.create_backup().unwrap();
    drop(app);
    expire_trash(&directory);
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&backups(&directory)[0])
        .unwrap();
    let reopened = Application::open(config).expect("维护失败不得阻断健康库启动");
    assert_eq!(
        reopened.editing_snapshot().unwrap().body,
        "仍然可编辑的健康便签"
    );
    assert!(reopened.note(expired).is_ok());
    assert!(reopened.backup_status().unwrap().warning.is_some());
    drop(locked);
    assert_eq!(reopened.maintain_trash().unwrap(), 1);
    assert!(reopened.backup_status().unwrap().warning.is_none());
}

#[test]
fn failed_backup_retries_on_restart_without_another_edit() {
    let directory = TempDir::new().unwrap();
    let config = ApplicationConfig::new(directory.path());
    let app = Application::open(config.clone()).unwrap();
    fs::write(directory.path().join("backups"), b"blocked").unwrap();
    app.edit(EditorIntent::ReplaceBody("等待下次启动重试备份".into()))
        .unwrap();
    app.edit(EditorIntent::Flush).unwrap();
    assert!(app.backup_status().unwrap().warning.is_some());
    drop(app);
    // 目标仍不可写时重启也不能把失败状态误报为空闲或成功。
    let blocked = Application::open(config.clone()).unwrap();
    assert!(blocked.backup_status().unwrap().warning.is_some());
    drop(blocked);
    fs::remove_file(directory.path().join("backups")).unwrap();
    let reopened = Application::open(config).unwrap();
    let status = reopened.backup_status().unwrap();
    assert!(
        status.last_success_at_ms.is_some(),
        "重启必须重试尚未成功的备份"
    );
    assert!(status.warning.is_none());
    assert_eq!(backups(&directory).len(), 1);
}

#[test]
fn current_switch_snooze_and_response_each_create_the_first_daily_backup() {
    use quicknote_app::platform::{PlatformCommand, test_support::TestPlatformServices};
    use quicknote_app::{ReminderActivation, ReminderActivationAction};

    for operation in ["switch", "snooze", "respond"] {
        let directory = TempDir::new().unwrap();
        let config = ApplicationConfig::new(directory.path());
        let platform = TestPlatformServices::new(directory.path());
        let app = Application::open(config.clone()).unwrap();
        app.edit(EditorIntent::ReplaceBody("前一天的便签".into()))
            .unwrap();
        let first = app.edit(EditorIntent::Flush).unwrap().note_id.unwrap();
        app.set_reminder(&platform, first, Some(4_000_000_000_000))
            .unwrap();
        let projection = platform.recorded_commands().unwrap().pop().unwrap();
        app.edit(EditorIntent::NewBlankDraft).unwrap();
        app.edit(EditorIntent::ReplaceBody("另一张便签".into()))
            .unwrap();
        app.edit(EditorIntent::Flush).unwrap();
        drop(app);
        // 保留合法的昨日快照，避免通过等待自然日切换测试备份门禁。
        let old = backups(&directory).pop().unwrap();
        let yesterday = chrono::Local::now() - chrono::Duration::days(1);
        let old_name = directory.path().join(format!(
            "backups/daily-{}-fixture.db",
            yesterday.format("%Y-%m-%d")
        ));
        fs::rename(old, &old_name).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&old_name)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(
                std::time::SystemTime::now() - std::time::Duration::from_secs(86_400),
            ))
            .unwrap();
        if operation == "respond" {
            let database = Connection::open(directory.path().join("quicknote.db")).unwrap();
            database
                .execute("UPDATE reminders SET scheduled_at_ms = 0", [])
                .unwrap();
        }
        let app = Application::open(config).unwrap();
        assert!(app.backup_status().unwrap().last_success_at_ms.is_some());
        assert_eq!(backups(&directory), vec![old_name]);
        match operation {
            "switch" => {
                app.edit(EditorIntent::SwitchCurrent(first)).unwrap();
            }
            "snooze" => {
                if let PlatformCommand::UpsertNotification {
                    reminder_id,
                    trigger_version,
                    snooze_activation_key,
                    snooze_minutes,
                    ..
                } = projection
                {
                    app.handle_reminder_activation(
                        &platform,
                        ReminderActivation {
                            activation_key: snooze_activation_key,
                            reminder_id,
                            trigger_version,
                            action: ReminderActivationAction::Snooze,
                            snooze_minutes: Some(snooze_minutes),
                        },
                    )
                    .unwrap();
                } else {
                    panic!("应收到提醒计划");
                }
            }
            _ => {
                assert!(app.respond_to_due_reminder(&platform, first).unwrap());
            }
        }
        assert!(app.backup_status().unwrap().warning.is_none());
        assert_eq!(backups(&directory).len(), 2, "{operation} 必须创建当日快照");
    }
}

/// 返回管理目录中的备份文件，不读取活动 WAL。
fn backups(directory: &TempDir) -> Vec<std::path::PathBuf> {
    fs::read_dir(directory.path().join("backups"))
        .expect("备份目录")
        .map(|entry| entry.expect("目录项").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "db"))
        .collect()
}

#[test]
fn daily_backup_is_a_consistent_snapshot_and_reopen_does_not_replace_it() {
    let directory = TempDir::new().unwrap();
    let config = ApplicationConfig::new(directory.path());
    let app = Application::open(config.clone()).unwrap();
    app.edit(EditorIntent::ReplaceBody("第一份中文正文".into()))
        .unwrap();
    app.edit(EditorIntent::Flush).unwrap();
    assert!(app.backup_status().unwrap().warning.is_none());
    let files = backups(&directory);
    assert_eq!(files.len(), 1);
    app.edit(EditorIntent::ReplaceBody("当天第二次编辑".into()))
        .unwrap();
    app.edit(EditorIntent::Flush).unwrap();
    drop(app);
    let app = Application::open(config).unwrap();
    app.edit(EditorIntent::ReplaceBody("重开后继续编辑".into()))
        .unwrap();
    app.edit(EditorIntent::Flush).unwrap();
    assert!(app.backup_status().unwrap().warning.is_none());
    assert_eq!(backups(&directory), files);
    let snapshot = Connection::open(&files[0]).unwrap();
    let body: String = snapshot
        .query_row("SELECT body FROM notes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(body, "第一份中文正文");
}

#[test]
fn backup_failure_keeps_saved_body_and_explicit_retry_recovers() {
    let directory = TempDir::new().unwrap();
    let app = Application::open(ApplicationConfig::new(directory.path())).unwrap();
    // 用普通文件占据备份目录，模拟不可写目标，不影响数据库目录。
    fs::write(directory.path().join("backups"), b"blocked").unwrap();
    app.edit(EditorIntent::ReplaceBody("备份失败也必须保存".into()))
        .unwrap();
    let saved = app.edit(EditorIntent::Flush).unwrap();
    assert!(app.backup_status().unwrap().warning.is_some());
    assert_eq!(
        app.note(saved.note_id.unwrap()).unwrap().body,
        "备份失败也必须保存"
    );
    fs::remove_file(directory.path().join("backups")).unwrap();
    assert!(app.create_backup().unwrap().warning.is_none());
    assert_eq!(backups(&directory).len(), 1);
}

#[test]
fn corrupt_database_is_preserved_and_recovery_remains_required_across_restarts() {
    let directory = TempDir::new().unwrap();
    let config = ApplicationConfig::new(directory.path());
    let app = Application::open(config.clone()).unwrap();
    app.edit(EditorIntent::ReplaceBody("可以恢复的便签".into()))
        .unwrap();
    app.edit(EditorIntent::Flush).unwrap();
    assert!(app.create_backup().unwrap().warning.is_none());
    drop(app);
    fs::write(
        directory.path().join("quicknote.db"),
        b"damaged database bytes",
    )
    .unwrap();
    assert!(matches!(
        Application::open(config.clone()),
        Err(ApplicationError::DatabaseCorrupt { .. })
    ));
    let session = RecoverySession::prepare(&config).unwrap();
    assert_eq!(
        fs::read(session.quarantine_directory.join("quicknote.db")).unwrap(),
        b"damaged database bytes"
    );
    assert!(matches!(
        Application::open(config.clone()),
        Err(ApplicationError::RecoveryRequired)
    ));
    drop(session);
    let session = RecoverySession::prepare(&config).unwrap();
    assert!(!session.backups.is_empty());
    session.restore(&session.backups[0].path).unwrap();
    let restored = Application::open(config).unwrap();
    assert_eq!(restored.editing_snapshot().unwrap().body, "可以恢复的便签");
    assert!(session.quarantine_directory.join("quicknote.db").exists());
}

#[test]
fn invalid_backup_is_not_offered_and_empty_database_requires_explicit_choice() {
    let directory = TempDir::new().unwrap();
    let config = ApplicationConfig::new(directory.path());
    fs::create_dir(directory.path().join("backups")).unwrap();
    fs::write(directory.path().join("backups/bad.db"), b"not sqlite").unwrap();
    fs::write(directory.path().join("quicknote.db"), b"corrupt").unwrap();
    let session = RecoverySession::prepare(&config).unwrap();
    assert!(session.backups.is_empty());
    assert!(
        session
            .restore(&directory.path().join("backups/bad.db"))
            .is_err()
    );
    assert!(!directory.path().join("quicknote.db").exists());
    session.create_empty().unwrap();
    let app = Application::open(config).unwrap();
    assert_eq!(app.snapshot().unwrap().active_note_count, 0);
    assert!(session.quarantine_directory.join("quicknote.db").exists());
}

#[test]
fn permanent_clear_removes_every_old_snapshot_and_publishes_a_clean_one() {
    let directory = TempDir::new().unwrap();
    let app = Application::open(ApplicationConfig::new(directory.path())).unwrap();
    app.edit(EditorIntent::ReplaceBody("不应从备份复活".into()))
        .unwrap();
    let note = app.edit(EditorIntent::Flush).unwrap().note_id.unwrap();
    app.create_backup().unwrap();
    // 中断后留下的快照临时文件也属于必须清除的历史。
    fs::copy(
        &backups(&directory)[0],
        directory.path().join("backups/.interrupted.tmp"),
    )
    .unwrap();
    let old = backups(&directory);
    app.apply_note_action(NoteAction::Archive(note)).unwrap();
    app.apply_note_action(NoteAction::MoveToTrash(note))
        .unwrap();
    app.apply_note_action(NoteAction::PermanentlyDelete(note))
        .unwrap();
    assert!(app.backup_status().unwrap().warning.is_none());
    assert!(old.iter().all(|path| !path.exists()));
    assert!(!directory.path().join("backups/.interrupted.tmp").exists());
    let fresh = backups(&directory);
    assert_eq!(fresh.len(), 1);
    let backup = Connection::open(&fresh[0]).unwrap();
    assert_eq!(
        backup
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[cfg(windows)]
#[test]
fn locked_backup_aborts_permanent_clear_without_losing_the_note() {
    let directory = TempDir::new().unwrap();
    let app = Application::open(ApplicationConfig::new(directory.path())).unwrap();
    app.edit(EditorIntent::ReplaceBody("删除失败时必须保留".into()))
        .unwrap();
    let note = app.edit(EditorIntent::Flush).unwrap().note_id.unwrap();
    app.apply_note_action(NoteAction::Archive(note)).unwrap();
    app.apply_note_action(NoteAction::MoveToTrash(note))
        .unwrap();
    app.create_backup().unwrap();
    // Windows 文件句柄不共享删除权限，确定性模拟旧备份暂时无法清理。
    use std::os::windows::fs::OpenOptionsExt;
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&backups(&directory)[0])
        .unwrap();
    assert!(
        app.apply_note_action(NoteAction::PermanentlyDelete(note))
            .is_err()
    );
    assert_eq!(
        app.note(note).unwrap().lifecycle,
        quicknote_app::NoteLifecycle::Trashed
    );
    drop(locked);
    app.apply_note_action(NoteAction::PermanentlyDelete(note))
        .unwrap();
    assert!(app.note(note).is_err());
}

#[test]
fn backup_rotation_enforces_daily_weekly_and_thirty_day_limits() {
    let directory = TempDir::new().unwrap();
    let app = Application::open(ApplicationConfig::new(directory.path())).unwrap();
    app.edit(EditorIntent::ReplaceBody("轮换快照".into()))
        .unwrap();
    app.edit(EditorIntent::Flush).unwrap();
    app.create_backup().unwrap();
    let original = backups(&directory)[0].clone();
    let now = std::time::SystemTime::now();
    for age in 1..=35 {
        let path = directory
            .path()
            .join(format!("backups/daily-fixture-{age}.db"));
        fs::copy(&original, &path).unwrap();
        let date = now - std::time::Duration::from_secs(age * 86_400);
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(date))
            .unwrap();
    }
    assert!(app.create_backup().unwrap().warning.is_none());
    let retained = backups(&directory);
    assert!((7..=11).contains(&retained.len()));
    for path in retained {
        assert!(
            fs::metadata(path)
                .unwrap()
                .modified()
                .unwrap()
                .elapsed()
                .unwrap()
                .as_secs()
                <= 30 * 86_400
        );
    }
}
