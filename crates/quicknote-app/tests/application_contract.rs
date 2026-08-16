//! 只通过应用模块接口验证持久化行为的契约测试。

use quicknote_app::{
    Application, ApplicationConfig, ApplicationError, Command, CommandResult, EditorIntent,
    SaveState,
};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[test]
fn first_open_and_reopen_preserve_schema_and_settings() {
    let directory = TempDir::new().expect("创建临时目录");
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("首次打开应用");
    let initial = app.snapshot().expect("读取初始快照");

    assert_eq!(initial.schema.application_id, 0x514E_3031);
    assert_eq!(initial.schema.version, 2);
    assert_eq!(initial.active_note_count, 0);
    assert_eq!(initial.current_note_id, None);
    assert_eq!(initial.default_snooze_minutes, 10);
    assert_eq!(initial.global_shortcut.to_string(), "Ctrl+Alt+Q");

    let result = app
        .execute(Command::SetDefaultSnoozeMinutes { minutes: 30 })
        .expect("更新设置");
    assert_eq!(result, CommandResult::Applied);
    drop(app);

    let reopened =
        Application::open(ApplicationConfig::new(directory.path())).expect("重新打开应用");
    let persisted = reopened.snapshot().expect("读取持久化快照");
    assert_eq!(persisted.schema, initial.schema);
    assert_eq!(persisted.default_snooze_minutes, 30);
}

#[test]
fn blank_draft_stays_identity_free_and_first_nonblank_save_is_atomic() {
    let directory = TempDir::new().expect("创建临时目录");
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");

    let blank = app
        .edit(EditorIntent::ReplaceBody(" \n\t".to_owned()))
        .expect("编辑空白草稿");
    assert_eq!(blank.note_id, None);
    assert!(matches!(blank.save_state, SaveState::BlankDraft));
    app.edit(EditorIntent::Flush).expect("刷新空白草稿");
    assert_eq!(app.snapshot().expect("读取空白结果").active_note_count, 0);

    app.edit(EditorIntent::ReplaceBody(
        "\n  第一条非空标题  \n正文".to_owned(),
    ))
    .expect("编辑非空正文");
    let saved = app.edit(EditorIntent::Flush).expect("刷新首次保存");
    assert!(saved.note_id.is_some());
    assert_eq!(saved.title, "第一条非空标题");
    assert_eq!(saved.revision, saved.saved_revision);
    assert!(matches!(saved.save_state, SaveState::Saved));
    let persisted = app.snapshot().expect("读取首次保存结果");
    assert_eq!(persisted.active_note_count, 1);
    assert_eq!(persisted.current_note_id, saved.note_id);
}

#[test]
fn autosave_commits_latest_revision_after_trailing_debounce() {
    let directory = TempDir::new().expect("创建临时目录");
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
    for body in ["版", "版本", "版本 A", "版本 B", "最终版本"] {
        app.edit(EditorIntent::ReplaceBody(body.to_owned()))
            .expect("连续编辑");
        thread::sleep(Duration::from_millis(40));
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let editing = app.editing_snapshot().expect("轮询自动保存");
        if matches!(editing.save_state, SaveState::Saved)
            && editing.revision == editing.saved_revision
        {
            break;
        }
        assert!(Instant::now() < deadline, "自动保存应在期限内完成");
        thread::sleep(Duration::from_millis(20));
    }
    drop(app);

    let reopened =
        Application::open(ApplicationConfig::new(directory.path())).expect("重新打开应用");
    assert_eq!(
        reopened.editing_snapshot().expect("读取恢复正文").body,
        "最终版本"
    );
}

#[test]
fn continuous_typing_still_commits_within_the_one_second_maximum_wait() {
    let directory = TempDir::new().expect("创建临时目录");
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
    let started = Instant::now();
    let mut revision = 0_u64;
    let mut observed_commit_while_typing = false;

    while started.elapsed() < Duration::from_millis(1_350) {
        revision += 1;
        app.edit(EditorIntent::ReplaceBody(format!(
            "连续输入版本 {revision}"
        )))
        .expect("连续输入");
        let snapshot = app.editing_snapshot().expect("读取连续输入状态");
        observed_commit_while_typing |= snapshot.saved_revision > 0;
        thread::sleep(Duration::from_millis(40));
    }

    assert!(
        observed_commit_while_typing,
        "持续输入超过一秒时必须在停止输入前产生一次真实提交"
    );
    let saved = app.edit(EditorIntent::Flush).expect("刷新连续输入最终版本");
    assert_eq!(saved.revision, saved.saved_revision);
    let expected = format!("连续输入版本 {revision}");
    drop(app);

    let reopened =
        Application::open(ApplicationConfig::new(directory.path())).expect("重新打开应用");
    assert_eq!(
        reopened.editing_snapshot().expect("读取最终正文").body,
        expected
    );
}

#[test]
fn homepage_switch_flushes_old_body_and_reuses_the_same_current_note() {
    let directory = TempDir::new().expect("创建临时目录");
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
    app.edit(EditorIntent::ReplaceBody("便签 A".to_owned()))
        .expect("编辑 A");
    let note_a = app
        .edit(EditorIntent::Flush)
        .expect("保存 A")
        .note_id
        .expect("A 已有身份");
    app.edit(EditorIntent::NewBlankDraft).expect("新建空白草稿");
    app.edit(EditorIntent::ReplaceBody("便签 B".to_owned()))
        .expect("编辑 B");
    let note_b = app
        .edit(EditorIntent::Flush)
        .expect("保存 B")
        .note_id
        .expect("B 已有身份");

    app.edit(EditorIntent::SwitchCurrent(note_a))
        .expect("主页切换到 A");
    app.edit(EditorIntent::ReplaceBody("便签 A 已更新".to_owned()))
        .expect("修改 A");
    let switched = app
        .edit(EditorIntent::SwitchCurrent(note_b))
        .expect("切换前应刷新 A");
    assert_eq!(switched.note_id, Some(note_b));
    assert_eq!(switched.body, "便签 B");
    assert_eq!(
        app.edit(EditorIntent::SwitchCurrent(note_a))
            .expect("重新打开 A")
            .body,
        "便签 A 已更新"
    );
}

#[test]
fn invalid_command_is_observable_and_does_not_change_state() {
    let directory = TempDir::new().expect("创建临时目录");
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");

    let error = app
        .execute(Command::SetDefaultSnoozeMinutes { minutes: 7 })
        .expect_err("不支持的稍后提醒时长必须失败");
    assert!(matches!(error, ApplicationError::InvalidCommand { .. }));
    assert_eq!(
        app.snapshot()
            .expect("读取失败后的快照")
            .default_snooze_minutes,
        10
    );
}
