//! 只通过应用模块接口验证持久化行为的契约测试。

use quicknote_app::platform::test_support::TestPlatformServices;
use quicknote_app::platform::{PlatformCommand, PlatformServices};
use quicknote_app::{
    Application, ApplicationConfig, ApplicationError, Command, CommandResult, EditorIntent,
    ExportBundle, NoteAction, NoteLifecycle, ReminderActivation, ReminderActivationAction,
    ReminderActivationOutcome, ReminderCoordinationReason, ReminderStatus, SaveState,
};
use std::fs;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

#[test]
fn first_open_and_reopen_preserve_schema_and_settings() {
    let directory = TempDir::new().expect("创建临时目录");
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("首次打开应用");
    let initial = app.snapshot().expect("读取初始快照");

    assert_eq!(initial.schema.application_id, 0x514E_3031);
    assert_eq!(initial.schema.version, 3);
    assert_eq!(initial.active_note_count, 0);
    assert_eq!(initial.current_note_id, None);
    assert_eq!(initial.archived_note_count, 0);
    assert_eq!(initial.trashed_note_count, 0);
    assert_eq!(initial.default_snooze_minutes, 10);
    assert_eq!(initial.global_shortcut.to_string(), "Ctrl+Alt+Q");
    assert!(!initial.startup_enabled);

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

#[test]
fn lifecycle_uses_stable_successors_and_never_skips_archive() {
    let directory = TempDir::new().expect("创建临时目录");
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
    app.edit(EditorIntent::ReplaceBody("便签 A".to_owned()))
        .expect("编辑 A");
    let note_a = app
        .edit(EditorIntent::Flush)
        .expect("保存 A")
        .note_id
        .expect("A 已有身份");
    app.edit(EditorIntent::NewBlankDraft).expect("新建 B 草稿");
    app.edit(EditorIntent::ReplaceBody("便签 B".to_owned()))
        .expect("编辑 B");
    let note_b = app
        .edit(EditorIntent::Flush)
        .expect("保存 B")
        .note_id
        .expect("B 已有身份");

    let after_b = app
        .apply_note_action(NoteAction::Archive(note_b))
        .expect("归档当前 B");
    assert_eq!(after_b.note_id, Some(note_a));
    assert_eq!(after_b.body, "便签 A");

    app.apply_note_action(NoteAction::Archive(note_a))
        .expect("归档最后一张活跃便签");
    assert_eq!(app.snapshot().expect("读取空当前").current_note_id, None);
    app.apply_note_action(NoteAction::Unarchive(note_a))
        .expect("无当前时取消归档 A");
    assert_eq!(
        app.snapshot().expect("读取 A 当前").current_note_id,
        Some(note_a)
    );
    app.apply_note_action(NoteAction::Unarchive(note_b))
        .expect("有当前时取消归档 B");
    assert_eq!(
        app.snapshot().expect("当前仍为 A").current_note_id,
        Some(note_a)
    );

    let error = app
        .apply_note_action(NoteAction::MoveToTrash(note_b))
        .expect_err("活跃便签不能直接删除");
    assert!(matches!(error, ApplicationError::InvalidCommand { .. }));
    app.apply_note_action(NoteAction::Archive(note_b))
        .expect("先归档 B");
    app.apply_note_action(NoteAction::MoveToTrash(note_b))
        .expect("再移入回收站");
    assert_eq!(
        app.note(note_b).expect("读取回收站 B").lifecycle,
        NoteLifecycle::Trashed
    );
    app.apply_note_action(NoteAction::RestoreFromTrash(note_b))
        .expect("恢复只回到归档");
    assert_eq!(
        app.note(note_b).expect("读取恢复 B").lifecycle,
        NoteLifecycle::Archived
    );
    assert_eq!(
        app.snapshot().expect("恢复不切换当前").current_note_id,
        Some(note_a)
    );
}

#[test]
fn search_is_literal_case_insensitive_and_excludes_trash() {
    let directory = TempDir::new().expect("创建临时目录");
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
    app.edit(EditorIntent::ReplaceBody(
        "Alpha 标题\n含有中文针脚".to_owned(),
    ))
    .expect("编辑活跃便签");
    let active = app
        .edit(EditorIntent::Flush)
        .expect("保存活跃便签")
        .note_id
        .expect("活跃便签身份");
    app.edit(EditorIntent::NewBlankDraft).expect("新建归档候选");
    app.edit(EditorIntent::ReplaceBody("Beta\nNeedle in body".to_owned()))
        .expect("编辑归档候选");
    let archived = app
        .edit(EditorIntent::Flush)
        .expect("保存归档候选")
        .note_id
        .expect("归档候选身份");
    app.apply_note_action(NoteAction::Archive(archived))
        .expect("归档候选");

    let english = app.search("nEeDlE").expect("英文大小写不敏感搜索");
    assert_eq!(english.len(), 1);
    assert_eq!(english[0].id, archived);
    assert_eq!(english[0].lifecycle, NoteLifecycle::Archived);
    let chinese = app.search("中文针脚").expect("中文正文子串搜索");
    assert_eq!(chinese.len(), 1);
    assert_eq!(chinese[0].id, active);

    app.apply_note_action(NoteAction::MoveToTrash(archived))
        .expect("归档便签进入回收站");
    assert!(app.search("needle").expect("搜索排除回收站").is_empty());
}

#[test]
fn json_and_markdown_exports_preserve_domain_state_and_source() {
    let directory = TempDir::new().expect("创建临时目录");
    let platform = TestPlatformServices::new(directory.path());
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
    let body = "# 中文标题\n\n- **原样** `syntax`\n";
    app.edit(EditorIntent::ReplaceBody(body.to_owned()))
        .expect("编辑 Markdown");
    let note_id = app
        .edit(EditorIntent::Flush)
        .expect("保存 Markdown")
        .note_id
        .expect("便签身份");

    let json_path = directory.path().join("exports").join("quicknote.json");
    app.export_json(&platform, &json_path).expect("导出 JSON");
    let json = fs::read_to_string(&json_path).expect("读取 JSON");
    let bundle: ExportBundle = serde_json::from_str(&json).expect("解析导出合同");
    assert_eq!(bundle.format_version, 1);
    assert_eq!(bundle.current_note_id, Some(note_id));
    assert_eq!(bundle.notes[0].body, body);
    assert!(!json.contains("outbox"));
    assert!(!json.contains("activation_receipts"));

    let markdown_path = directory.path().join("exports").join("note.md");
    app.export_markdown(&platform, note_id, &markdown_path)
        .expect("导出 Markdown");
    let markdown = fs::read_to_string(markdown_path).expect("读取 Markdown");
    assert!(markdown.starts_with("---\nquicknote_export_version: 1\n"));
    assert!(markdown.ends_with(body));
    assert!(!markdown.contains('\r'));

    let protected_path = directory.path().join("exports").join("protected.json");
    fs::write(&protected_path, "原目标内容").expect("创建既有目标");
    platform
        .fail_next_file_write("模拟磁盘满")
        .expect("注入原子写入失败");
    app.export_json(&platform, &protected_path)
        .expect_err("写入失败必须可观察");
    assert_eq!(
        fs::read_to_string(protected_path).expect("读取失败后的目标"),
        "原目标内容"
    );
}

#[test]
fn startup_setting_is_projected_before_it_is_persisted() {
    let directory = TempDir::new().expect("创建临时目录");
    let platform = TestPlatformServices::new(directory.path());
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");

    app.configure_startup(&platform, true).expect("启用自启动");
    assert!(app.snapshot().expect("读取自启动设置").startup_enabled);
    platform
        .fail_next_apply("模拟注册失败")
        .expect("注入平台失败");
    app.configure_startup(&platform, false)
        .expect_err("平台失败不得改写设置");
    assert!(app.snapshot().expect("旧设置应保留").startup_enabled);
}

#[test]
fn archived_search_result_can_be_read_without_changing_current_note() {
    let directory = TempDir::new().expect("创建临时目录");
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
    app.edit(EditorIntent::ReplaceBody("保留当前".to_owned()))
        .expect("创建当前便签");
    let current = app
        .edit(EditorIntent::Flush)
        .expect("保存当前便签")
        .note_id
        .expect("当前身份");
    app.edit(EditorIntent::NewBlankDraft).expect("新建归档便签");
    app.edit(EditorIntent::ReplaceBody("只读结果".to_owned()))
        .expect("编辑归档便签");
    let archived = app
        .edit(EditorIntent::Flush)
        .expect("保存归档便签")
        .note_id
        .expect("归档身份");
    app.apply_note_action(NoteAction::Archive(archived))
        .expect("归档并继任当前");

    let document = app.note(archived).expect("只读打开归档结果");
    assert_eq!(document.lifecycle, NoteLifecycle::Archived);
    assert_eq!(document.body, "只读结果");
    assert_eq!(
        app.snapshot().expect("当前未改变").current_note_id,
        Some(current)
    );
}

#[test]
fn future_deadline_creates_one_independent_reminder() {
    let directory = TempDir::new().expect("创建临时目录");
    let platform = TestPlatformServices::new(directory.path());
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
    app.edit(EditorIntent::ReplaceBody("带截止时间的便签".to_owned()))
        .expect("编辑便签");
    let note_id = app
        .edit(EditorIntent::Flush)
        .expect("保存便签")
        .note_id
        .expect("便签身份");
    let due_at_ms = current_time_ms() + 60_000;

    let timing = app
        .set_due_at(&platform, note_id, Some(due_at_ms))
        .expect("设置未来截止时间");
    assert_eq!(timing.due_at_ms, Some(due_at_ms));
    assert_eq!(
        timing
            .reminder
            .as_ref()
            .map(|reminder| reminder.scheduled_at_ms),
        Some(due_at_ms)
    );

    let without_deadline = app
        .set_due_at(&platform, note_id, None)
        .expect("清除截止时间");
    assert_eq!(without_deadline.due_at_ms, None);
    assert!(without_deadline.reminder.is_some(), "提醒必须保持独立");
    let without_reminder = app
        .set_reminder(&platform, note_id, None)
        .expect("独立清除提醒");
    assert_eq!(without_reminder.due_at_ms, None);
    assert!(without_reminder.reminder.is_none());

    let past_due = current_time_ms() - 60_000;
    let past = app
        .set_due_at(&platform, note_id, Some(past_due))
        .expect("截止时间允许位于过去");
    assert_eq!(past.due_at_ms, Some(past_due));
    assert!(past.reminder.is_none(), "过去截止时间不得补建提醒");
}

#[test]
fn platform_failure_keeps_reminder_fact_and_retries_after_restart() {
    let directory = TempDir::new().expect("创建临时目录");
    let platform = TestPlatformServices::new(directory.path());
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
    app.edit(EditorIntent::ReplaceBody("平台失败仍保留".to_owned()))
        .expect("编辑便签");
    let note_id = app
        .edit(EditorIntent::Flush)
        .expect("保存便签")
        .note_id
        .expect("便签身份");
    platform
        .fail_next_apply("模拟通知平台不可用")
        .expect("注入平台失败");

    let timing = app
        .set_reminder(&platform, note_id, Some(current_time_ms() + 60_000))
        .expect("平台失败不得回滚提醒事实");
    assert!(timing.reminder.is_some());
    assert!(timing.platform_sync_pending);
    assert!(timing.platform_sync_error.is_some());
    drop(app);

    thread::sleep(Duration::from_millis(1_050));
    let reopened = Application::open(ApplicationConfig::new(directory.path())).expect("重启应用");
    let coordination = reopened
        .coordinate_reminders(&platform, None, ReminderCoordinationReason::Startup)
        .expect("重启后重试 outbox");
    assert_eq!(coordination.pending_projection_count, 0);
    assert_eq!(
        platform
            .scheduled_notifications()
            .expect("读取测试平台计划")
            .len(),
        1
    );
}

#[test]
fn notification_actions_are_versioned_idempotent_and_freeze_snooze_duration() {
    let directory = TempDir::new().expect("创建临时目录");
    let platform = TestPlatformServices::new(directory.path());
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
    app.edit(EditorIntent::ReplaceBody("稍后提醒语义".to_owned()))
        .expect("编辑便签");
    let note_id = app
        .edit(EditorIntent::Flush)
        .expect("保存便签")
        .note_id
        .expect("便签身份");
    app.set_reminder(&platform, note_id, Some(current_time_ms() + 60_000))
        .expect("设置提醒");
    let first = latest_upsert(&platform);
    let (
        reminder_id,
        trigger_version,
        open_activation_key,
        snooze_activation_key,
        first_snooze_minutes,
    ) = match first {
        PlatformCommand::UpsertNotification {
            reminder_id,
            trigger_version,
            open_activation_key,
            snooze_activation_key,
            snooze_minutes,
            ..
        } => (
            reminder_id,
            trigger_version,
            open_activation_key,
            snooze_activation_key,
            snooze_minutes,
        ),
        other => panic!("应得到提醒计划，实际为 {other:?}"),
    };
    assert_eq!(first_snooze_minutes, 10);
    app.execute(Command::SetDefaultSnoozeMinutes { minutes: 30 })
        .expect("修改未来通知默认值");

    let action_time = current_time_ms();
    let snooze = ReminderActivation {
        activation_key: snooze_activation_key,
        reminder_id,
        trigger_version,
        action: ReminderActivationAction::Snooze,
        snooze_minutes: Some(first_snooze_minutes),
    };
    let outcome = app
        .handle_reminder_activation(&platform, snooze.clone())
        .expect("稍后提醒");
    let scheduled_at_ms = match outcome {
        ReminderActivationOutcome::Snoozed {
            note_id: snoozed_note,
            scheduled_at_ms,
        } => {
            assert_eq!(snoozed_note, note_id);
            scheduled_at_ms
        }
        other => panic!("稍后提醒结果无效：{other:?}"),
    };
    assert!(scheduled_at_ms >= action_time + 10 * 60_000);
    assert!(scheduled_at_ms <= current_time_ms() + 10 * 60_000);
    match latest_upsert(&platform) {
        PlatformCommand::UpsertNotification { snooze_minutes, .. } => {
            assert_eq!(snooze_minutes, 30, "新生成通知采用新默认值")
        }
        other => panic!("应得到新提醒计划，实际为 {other:?}"),
    }
    assert_eq!(
        app.handle_reminder_activation(&platform, snooze)
            .expect("重复动作幂等"),
        ReminderActivationOutcome::Ignored
    );
    let old_open = ReminderActivation {
        activation_key: open_activation_key,
        reminder_id,
        trigger_version,
        action: ReminderActivationAction::Open,
        snooze_minutes: None,
    };
    assert_eq!(
        app.handle_reminder_activation(&platform, old_open)
            .expect("旧版本打开动作无效"),
        ReminderActivationOutcome::Ignored
    );
    assert!(
        app.note_timing(note_id)
            .expect("读取新提醒")
            .reminder
            .is_some()
    );

    let current_open = match latest_upsert(&platform) {
        PlatformCommand::UpsertNotification {
            reminder_id,
            trigger_version,
            open_activation_key,
            ..
        } => ReminderActivation {
            activation_key: open_activation_key,
            reminder_id,
            trigger_version,
            action: ReminderActivationAction::Open,
            snooze_minutes: None,
        },
        other => panic!("应得到当前提醒计划，实际为 {other:?}"),
    };
    app.apply_note_action(NoteAction::Archive(note_id))
        .expect("归档永久清除提醒");
    assert_eq!(
        app.handle_reminder_activation(&platform, current_open)
            .expect("归档后的旧动作无效"),
        ReminderActivationOutcome::Ignored
    );
    assert_eq!(
        app.note(note_id).expect("归档事实保留").lifecycle,
        NoteLifecycle::Archived
    );
}

#[test]
fn due_reminder_is_missed_unless_note_was_already_focused() {
    let directory = TempDir::new().expect("创建临时目录");
    let platform = TestPlatformServices::new(directory.path());
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
    app.edit(EditorIntent::ReplaceBody("聚焦到点".to_owned()))
        .expect("编辑便签");
    let note_id = app
        .edit(EditorIntent::Flush)
        .expect("保存便签")
        .note_id
        .expect("便签身份");
    app.set_reminder(&platform, note_id, Some(current_time_ms() + 150))
        .expect("设置短提醒");
    app.coordinate_reminders(
        &platform,
        Some(note_id),
        ReminderCoordinationReason::Continuous,
    )
    .expect("建立聚焦观察");
    thread::sleep(Duration::from_millis(190));
    app.coordinate_reminders(
        &platform,
        Some(note_id),
        ReminderCoordinationReason::Continuous,
    )
    .expect("聚焦跨过提醒时间");
    assert!(
        app.note_timing(note_id)
            .expect("读取聚焦结果")
            .reminder
            .is_none()
    );

    app.set_reminder(&platform, note_id, Some(current_time_ms() + 120))
        .expect("设置错过候选");
    app.coordinate_reminders(&platform, None, ReminderCoordinationReason::Continuous)
        .expect("取消焦点");
    thread::sleep(Duration::from_millis(160));
    app.coordinate_reminders(&platform, None, ReminderCoordinationReason::Continuous)
        .expect("无焦点跨过提醒时间");
    assert_eq!(
        app.note_timing(note_id)
            .expect("读取错过结果")
            .reminder
            .expect("保留错过提醒")
            .status,
        ReminderStatus::Missed
    );
    assert!(
        app.respond_to_due_reminder(&platform, note_id)
            .expect("主动打开响应")
    );
    assert!(
        app.note_timing(note_id)
            .expect("提醒已响应")
            .reminder
            .is_none()
    );
}

#[test]
fn reconciliation_recreates_missing_windows_schedule_without_duplicates() {
    let directory = TempDir::new().expect("创建临时目录");
    let platform = TestPlatformServices::new(directory.path());
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
    app.edit(EditorIntent::ReplaceBody("Explorer 对账".to_owned()))
        .expect("编辑便签");
    let note_id = app
        .edit(EditorIntent::Flush)
        .expect("保存便签")
        .note_id
        .expect("便签身份");
    app.set_reminder(&platform, note_id, Some(current_time_ms() + 60_000))
        .expect("设置提醒");
    platform
        .clear_scheduled_notifications()
        .expect("模拟 Explorer 丢失计划");

    let coordination = app
        .coordinate_reminders(&platform, None, ReminderCoordinationReason::Resume)
        .expect("恢复时重新对账");
    assert_eq!(coordination.pending_projection_count, 0);
    assert_eq!(
        platform
            .scheduled_notifications()
            .expect("读取重建计划")
            .len(),
        1
    );
    app.coordinate_reminders(&platform, None, ReminderCoordinationReason::Startup)
        .expect("重复对账");
    assert_eq!(
        platform
            .scheduled_notifications()
            .expect("重复对账无并列计划")
            .len(),
        1
    );
}

fn latest_upsert(platform: &TestPlatformServices) -> PlatformCommand {
    platform
        .recorded_commands()
        .expect("读取平台命令")
        .into_iter()
        .rev()
        .find(|command| matches!(command, PlatformCommand::UpsertNotification { .. }))
        .expect("至少有一条计划通知命令")
}

fn current_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时间晚于 Unix epoch")
        .as_millis()
        .try_into()
        .expect("当前时间可表示为 i64 毫秒")
}
