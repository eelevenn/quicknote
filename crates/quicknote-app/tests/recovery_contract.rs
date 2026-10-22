//! 通过真实 SQLite 与文件故障验证备份、隔离、恢复和永久清除合同。

use quicknote_app::{
    Application, ApplicationConfig, ApplicationError, EditorIntent, NoteAction, RecoverySession,
};
use rusqlite::Connection;
use std::fs;
use tempfile::TempDir;

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
