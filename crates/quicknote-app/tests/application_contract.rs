//! 只通过应用模块接口验证持久化行为的契约测试。

use quicknote_app::{Application, ApplicationConfig, ApplicationError, Command, CommandResult};
use tempfile::TempDir;

#[test]
fn first_open_and_reopen_preserve_schema_and_settings() {
    let directory = TempDir::new().expect("创建临时目录");
    let app = Application::open(ApplicationConfig::new(directory.path())).expect("首次打开应用");
    let initial = app.snapshot().expect("读取初始快照");

    assert_eq!(initial.schema.application_id, 0x514E_3031);
    assert_eq!(initial.schema.version, 1);
    assert_eq!(initial.active_note_count, 0);
    assert_eq!(initial.current_note_id, None);
    assert_eq!(initial.default_snooze_minutes, 10);

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
