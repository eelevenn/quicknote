//! 仅承载真实平台差异的 `PlatformServices` seam。

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

use crate::ReminderActivation;

/// Windows 注册、通知和激活统一使用的稳定产品身份。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductIdentity {
    /// 用户可见的产品名。
    pub product_name: &'static str,
    /// Windows AUMID。
    pub aumid: &'static str,
    /// 协议名，不包含冒号。
    pub protocol: &'static str,
}

/// QuickNote 的稳定生产身份；不得替换为 benchmark 或 spike 标识。
pub const PRODUCT_IDENTITY: ProductIdentity = ProductIdentity {
    product_name: "QuickNote",
    aumid: "eelevenn.QuickNote",
    protocol: "quicknote",
};

/// 为通知正文或按钮生成只包含稳定 ASCII 字段的协议 URI。
pub fn reminder_protocol_uri(activation: &ReminderActivation) -> String {
    let action = match activation.action {
        crate::ReminderActivationAction::Open => "open",
        crate::ReminderActivationAction::Snooze => "snooze",
    };
    let mut uri = format!(
        "{}://reminder/{action}?activation_key={}&reminder_id={}&trigger_version={}",
        PRODUCT_IDENTITY.protocol,
        activation.activation_key,
        activation.reminder_id,
        activation.trigger_version
    );
    if let Some(minutes) = activation.snooze_minutes {
        uri.push_str(&format!("&snooze_minutes={minutes}"));
    }
    uri
}

/// 解析 QuickNote 通知协议；未知或被篡改的载荷只退化为普通协议激活。
pub fn activation_from_protocol_uri(value: &str) -> ActivationRequest {
    let prefix = format!("{}://reminder/", PRODUCT_IDENTITY.protocol);
    let Some(rest) = value.strip_prefix(&prefix) else {
        return ActivationRequest::ProtocolUri(value.to_owned());
    };
    let Some((action, query)) = rest.split_once('?') else {
        return ActivationRequest::ProtocolUri(value.to_owned());
    };
    let action = match action {
        "open" => crate::ReminderActivationAction::Open,
        "snooze" => crate::ReminderActivationAction::Snooze,
        _ => return ActivationRequest::ProtocolUri(value.to_owned()),
    };
    let mut activation_key = None;
    let mut reminder_id = None;
    let mut trigger_version = None;
    let mut snooze_minutes = None;
    for pair in query.split('&') {
        let Some((key, pair_value)) = pair.split_once('=') else {
            return ActivationRequest::ProtocolUri(value.to_owned());
        };
        match key {
            "activation_key" => activation_key = Some(pair_value.to_owned()),
            "reminder_id" => reminder_id = Uuid::parse_str(pair_value).ok(),
            "trigger_version" => trigger_version = pair_value.parse().ok(),
            "snooze_minutes" => snooze_minutes = pair_value.parse().ok(),
            _ => return ActivationRequest::ProtocolUri(value.to_owned()),
        }
    }
    let (Some(activation_key), Some(reminder_id), Some(trigger_version)) =
        (activation_key, reminder_id, trigger_version)
    else {
        return ActivationRequest::ProtocolUri(value.to_owned());
    };
    if Uuid::parse_str(&activation_key).is_err()
        || (action == crate::ReminderActivationAction::Open && snooze_minutes.is_some())
        || (action == crate::ReminderActivationAction::Snooze && snooze_minutes.is_none())
    {
        return ActivationRequest::ProtocolUri(value.to_owned());
    }
    ActivationRequest::Reminder(ReminderActivation {
        activation_key,
        reminder_id,
        trigger_version,
        action,
        snooze_minutes,
    })
}

/// 全局快捷键使用的平台中立修饰键集合。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShortcutModifiers {
    ctrl: bool,
    alt: bool,
    shift: bool,
}

impl ShortcutModifiers {
    /// 返回组合是否包含 Ctrl。
    pub fn ctrl(self) -> bool {
        self.ctrl
    }

    /// 返回组合是否包含 Alt。
    pub fn alt(self) -> bool {
        self.alt
    }

    /// 返回组合是否包含 Shift。
    pub fn shift(self) -> bool {
        self.shift
    }

    fn is_empty(self) -> bool {
        !self.ctrl && !self.alt && !self.shift
    }
}

/// Windows adapter 可映射的普通快捷键。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShortcutKey {
    /// A-Z 字母。
    Letter(char),
    /// 0-9 数字。
    Digit(u8),
    /// F1-F24 功能键。
    Function(u8),
    /// 空格键。
    Space,
    /// Tab 键。
    Tab,
    /// Enter 键。
    Enter,
    /// Escape 键。
    Escape,
    /// Backspace 键。
    Backspace,
    /// Insert 键。
    Insert,
    /// Delete 键。
    Delete,
    /// Home 键。
    Home,
    /// End 键。
    End,
    /// PageUp 键。
    PageUp,
    /// PageDown 键。
    PageDown,
    /// 左方向键。
    Left,
    /// 右方向键。
    Right,
    /// 上方向键。
    Up,
    /// 下方向键。
    Down,
    /// 分号键。
    Semicolon,
    /// 等号键。
    Equals,
    /// 逗号键。
    Comma,
    /// 减号键。
    Minus,
    /// 句点键。
    Period,
    /// 斜杠键。
    Slash,
    /// 反引号键。
    Backtick,
    /// 左方括号键。
    LeftBracket,
    /// 反斜杠键。
    Backslash,
    /// 右方括号键。
    RightBracket,
    /// 单引号键。
    Quote,
}

/// 已验证且可稳定序列化的全局快捷键。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GlobalShortcut {
    modifiers: ShortcutModifiers,
    key: ShortcutKey,
}

impl GlobalShortcut {
    /// 解析并验证用户输入；不会为无效或冲突组合静默换键。
    pub fn parse(value: &str) -> Result<Self, ShortcutValidationError> {
        let mut modifiers = ShortcutModifiers::default();
        let mut key = None;
        let mut saw_token = false;

        for raw_token in value.split('+') {
            let token = raw_token.trim();
            if token.is_empty() {
                return Err(ShortcutValidationError::new("快捷键包含空片段"));
            }
            saw_token = true;
            match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => set_modifier(&mut modifiers.ctrl, "Ctrl")?,
                "alt" => set_modifier(&mut modifiers.alt, "Alt")?,
                "shift" => set_modifier(&mut modifiers.shift, "Shift")?,
                "win" | "windows" | "meta" | "super" => {
                    return Err(ShortcutValidationError::new(
                        "快捷键不得包含 Windows 徽标键",
                    ));
                }
                _ => {
                    if key.is_some() {
                        return Err(ShortcutValidationError::new(
                            "快捷键必须且只能包含一个普通键",
                        ));
                    }
                    key = Some(parse_shortcut_key(token)?);
                }
            }
        }

        if !saw_token {
            return Err(ShortcutValidationError::new("快捷键不能为空"));
        }
        let key = key.ok_or_else(|| {
            ShortcutValidationError::new("快捷键必须包含一个普通键，不能只有修饰键")
        })?;
        if key == ShortcutKey::Function(12) {
            return Err(ShortcutValidationError::new(
                "F12 保留给系统调试器，不能注册为全局快捷键",
            ));
        }
        if modifiers.is_empty() && !matches!(key, ShortcutKey::Function(1..=11 | 13..=24)) {
            return Err(ShortcutValidationError::new(
                "无修饰快捷键只允许 F1-F11 或 F13-F24",
            ));
        }

        Ok(Self { modifiers, key })
    }

    /// 返回已验证的修饰键集合。
    pub fn modifiers(self) -> ShortcutModifiers {
        self.modifiers
    }

    /// 返回已验证的普通键。
    pub fn key(self) -> ShortcutKey {
        self.key
    }
}

impl Default for GlobalShortcut {
    fn default() -> Self {
        Self {
            modifiers: ShortcutModifiers {
                ctrl: true,
                alt: true,
                shift: false,
            },
            key: ShortcutKey::Letter('Q'),
        }
    }
}

impl fmt::Display for GlobalShortcut {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::with_capacity(4);
        if self.modifiers.ctrl {
            parts.push("Ctrl".to_owned());
        }
        if self.modifiers.alt {
            parts.push("Alt".to_owned());
        }
        if self.modifiers.shift {
            parts.push("Shift".to_owned());
        }
        parts.push(shortcut_key_name(self.key));
        formatter.write_str(&parts.join("+"))
    }
}

/// 用户快捷键不满足安全注册规则。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("快捷键无效：{message}")]
pub struct ShortcutValidationError {
    message: String,
}

impl ShortcutValidationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

fn set_modifier(slot: &mut bool, name: &str) -> Result<(), ShortcutValidationError> {
    if *slot {
        return Err(ShortcutValidationError::new(format!(
            "修饰键 {name} 不能重复"
        )));
    }
    *slot = true;
    Ok(())
}

fn parse_shortcut_key(token: &str) -> Result<ShortcutKey, ShortcutValidationError> {
    let upper = token.to_ascii_uppercase();
    if upper.len() == 1 {
        let value = upper.as_bytes()[0];
        return match value {
            b'A'..=b'Z' => Ok(ShortcutKey::Letter(char::from(value))),
            b'0'..=b'9' => Ok(ShortcutKey::Digit(value - b'0')),
            b';' => Ok(ShortcutKey::Semicolon),
            b'=' => Ok(ShortcutKey::Equals),
            b',' => Ok(ShortcutKey::Comma),
            b'-' => Ok(ShortcutKey::Minus),
            b'.' => Ok(ShortcutKey::Period),
            b'/' => Ok(ShortcutKey::Slash),
            b'`' => Ok(ShortcutKey::Backtick),
            b'[' => Ok(ShortcutKey::LeftBracket),
            b'\\' => Ok(ShortcutKey::Backslash),
            b']' => Ok(ShortcutKey::RightBracket),
            b'\'' => Ok(ShortcutKey::Quote),
            _ => Err(ShortcutValidationError::new("不支持该普通键")),
        };
    }
    if let Some(number) = upper.strip_prefix('F')
        && let Ok(number) = number.parse::<u8>()
        && (1..=24).contains(&number)
    {
        return Ok(ShortcutKey::Function(number));
    }

    match upper.as_str() {
        "SPACE" => Ok(ShortcutKey::Space),
        "TAB" => Ok(ShortcutKey::Tab),
        "ENTER" | "RETURN" => Ok(ShortcutKey::Enter),
        "ESC" | "ESCAPE" => Ok(ShortcutKey::Escape),
        "BACKSPACE" => Ok(ShortcutKey::Backspace),
        "INSERT" | "INS" => Ok(ShortcutKey::Insert),
        "DELETE" | "DEL" => Ok(ShortcutKey::Delete),
        "HOME" => Ok(ShortcutKey::Home),
        "END" => Ok(ShortcutKey::End),
        "PAGEUP" | "PGUP" => Ok(ShortcutKey::PageUp),
        "PAGEDOWN" | "PGDN" => Ok(ShortcutKey::PageDown),
        "LEFT" => Ok(ShortcutKey::Left),
        "RIGHT" => Ok(ShortcutKey::Right),
        "UP" => Ok(ShortcutKey::Up),
        "DOWN" => Ok(ShortcutKey::Down),
        "SEMICOLON" => Ok(ShortcutKey::Semicolon),
        "EQUALS" => Ok(ShortcutKey::Equals),
        "COMMA" => Ok(ShortcutKey::Comma),
        "MINUS" => Ok(ShortcutKey::Minus),
        "PERIOD" => Ok(ShortcutKey::Period),
        "SLASH" => Ok(ShortcutKey::Slash),
        "BACKTICK" => Ok(ShortcutKey::Backtick),
        "LEFTBRACKET" => Ok(ShortcutKey::LeftBracket),
        "BACKSLASH" => Ok(ShortcutKey::Backslash),
        "RIGHTBRACKET" => Ok(ShortcutKey::RightBracket),
        "QUOTE" => Ok(ShortcutKey::Quote),
        _ => Err(ShortcutValidationError::new(format!(
            "无法识别普通键 {token}"
        ))),
    }
}

fn shortcut_key_name(key: ShortcutKey) -> String {
    match key {
        ShortcutKey::Letter(value) => value.to_string(),
        ShortcutKey::Digit(value) => value.to_string(),
        ShortcutKey::Function(value) => format!("F{value}"),
        ShortcutKey::Space => "Space".to_owned(),
        ShortcutKey::Tab => "Tab".to_owned(),
        ShortcutKey::Enter => "Enter".to_owned(),
        ShortcutKey::Escape => "Escape".to_owned(),
        ShortcutKey::Backspace => "Backspace".to_owned(),
        ShortcutKey::Insert => "Insert".to_owned(),
        ShortcutKey::Delete => "Delete".to_owned(),
        ShortcutKey::Home => "Home".to_owned(),
        ShortcutKey::End => "End".to_owned(),
        ShortcutKey::PageUp => "PageUp".to_owned(),
        ShortcutKey::PageDown => "PageDown".to_owned(),
        ShortcutKey::Left => "Left".to_owned(),
        ShortcutKey::Right => "Right".to_owned(),
        ShortcutKey::Up => "Up".to_owned(),
        ShortcutKey::Down => "Down".to_owned(),
        ShortcutKey::Semicolon => ";".to_owned(),
        ShortcutKey::Equals => "=".to_owned(),
        ShortcutKey::Comma => ",".to_owned(),
        ShortcutKey::Minus => "-".to_owned(),
        ShortcutKey::Period => ".".to_owned(),
        ShortcutKey::Slash => "/".to_owned(),
        ShortcutKey::Backtick => "`".to_owned(),
        ShortcutKey::LeftBracket => "[".to_owned(),
        ShortcutKey::Backslash => "\\".to_owned(),
        ShortcutKey::RightBracket => "]".to_owned(),
        ShortcutKey::Quote => "'".to_owned(),
    }
}

/// 平台壳送入共享应用的激活事件。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ActivationRequest {
    /// 显示并聚焦主页。
    ShowMain,
    /// 登录后只启动后台能力，不主动显示窗口。
    BackgroundStartup,
    /// 显示并聚焦快速记录窗。
    ShowQuickCapture,
    /// 全局快捷键触发，由 UI 线程按焦点三态决定最终动作。
    GlobalShortcutPressed,
    /// 无法识别的协议载荷只打开主页，不执行领域动作。
    ProtocolUri(String),
    /// 已验证结构的通知打开或稍后提醒动作。
    Reminder(ReminderActivation),
    /// 系统从挂起状态恢复，需要应用协调本地事实。
    Resumed,
}

/// Windows 通知计划中可与领域提醒对账的稳定键。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct NotificationProjectionKey {
    /// 稳定提醒身份。
    pub reminder_id: Uuid,
    /// 生成通知时的触发版本。
    pub trigger_version: u64,
}

/// 由应用提交、在事务完成后执行的平台投影。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PlatformCommand {
    /// 注册或替换全局快捷键。
    SetGlobalShortcut {
        /// 已通过应用规则验证的快捷键。
        shortcut: GlobalShortcut,
    },
    /// 移除当前全局快捷键。
    ClearGlobalShortcut,
    /// 创建或替换一个带触发版本的系统通知。
    UpsertNotification {
        /// 领域提醒 UUID。
        reminder_id: Uuid,
        /// 防止旧投影覆盖新提醒的单调版本。
        trigger_version: u64,
        /// 对应活跃便签身份，用于协议激活路由。
        note_id: Uuid,
        /// UTC Unix 毫秒触发时刻。
        scheduled_at_ms: i64,
        /// 用户可见标题。
        title: String,
        /// 用户可见正文。
        body: String,
        /// 当前通知固化的稍后提醒分钟数。
        snooze_minutes: u16,
        /// 正文点击和“打开”按钮共享的幂等键。
        open_activation_key: String,
        /// “稍后提醒”按钮使用的幂等键。
        snooze_activation_key: String,
    },
    /// 删除不再对应领域事实的系统通知。
    CancelNotification {
        /// 领域提醒 UUID。
        reminder_id: Uuid,
        /// 只取消对应触发版本。
        trigger_version: u64,
    },
    /// 控制系统托盘入口。
    SetTrayVisible {
        /// 是否保留托盘入口。
        visible: bool,
    },
    /// 控制当前用户的登录后启动设置。
    SetStartupEnabled {
        /// 用户是否请求登录后启动。
        enabled: bool,
    },
    /// 在用户明确点击后交给系统打开外部网页。
    OpenExternalLink {
        /// 已由应用层验证为 http(s) 的地址。
        url: String,
    },
}

/// 平台 adapter 的稳定错误，不向共享核心泄漏 Win32 错误类型。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("平台操作 {operation} 失败：{message}")]
pub struct PlatformError {
    /// 失败的逻辑操作名。
    pub operation: &'static str,
    /// 可记录且可展示的错误说明。
    pub message: String,
}

impl PlatformError {
    /// 从 adapter 内部错误创建稳定错误。
    pub fn new(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            operation,
            message: message.into(),
        }
    }
}

/// 主实例持有该租约期间，平台必须拒绝第二个主实例。
pub trait InstanceLease: Send {}

/// 单实例争用的可观察结果。
pub enum InstanceRole {
    /// 当前进程是主实例，租约必须保活到应用退出。
    Primary(Box<dyn InstanceLease>),
    /// 当前进程已把激活转发给主实例，应立即退出。
    SecondaryForwarded,
}

impl InstanceRole {
    /// 便于启动壳判断是否继续创建数据库和窗口。
    pub fn is_primary(&self) -> bool {
        matches!(self, Self::Primary(_))
    }
}

/// 主实例接收后续平台激活的回调。
pub type ActivationHandler = Arc<dyn Fn(ActivationRequest) + Send + Sync + 'static>;

/// 平台差异的深 seam；UI 和领域命令不直接调用 Win32。
pub trait PlatformServices: Send + Sync {
    /// 返回平台私有且位于本地文件系统的数据目录。
    fn data_directory(&self) -> Result<PathBuf, PlatformError>;

    /// 获取主实例租约，或把激活转发给已经存在的实例。
    fn acquire_single_instance(
        &self,
        initial_activation: ActivationRequest,
        on_activation: ActivationHandler,
    ) -> Result<InstanceRole, PlatformError>;

    /// 应用一个已经在领域事务中提交的平台投影。
    fn apply(&self, command: PlatformCommand) -> Result<(), PlatformError>;

    /// 返回 Windows 当前仍持有的 QuickNote 计划通知，用于重启和 Explorer 恢复对账。
    fn scheduled_notifications(&self) -> Result<Vec<NotificationProjectionKey>, PlatformError>;

    /// 在目标目录先完整写入临时文件，再以平台原子语义替换目标。
    fn write_file_atomically(&self, target: &Path, contents: &[u8]) -> Result<(), PlatformError>;
}

/// 供非 Windows adapter 使用的同目录临时写入实现。
fn portable_atomic_write(target: &Path, contents: &[u8]) -> Result<(), PlatformError> {
    let parent = target
        .parent()
        .ok_or_else(|| PlatformError::new("prepare_atomic_export", "导出目标缺少父目录"))?;
    fs::create_dir_all(parent)
        .map_err(|error| PlatformError::new("create_export_directory", error.to_string()))?;
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| PlatformError::new("prepare_atomic_export", "导出文件名无效"))?;
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    let result = (|| -> Result<(), PlatformError> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| PlatformError::new("create_export_temporary", error.to_string()))?;
        file.write_all(contents)
            .and_then(|_| file.sync_all())
            .map_err(|error| PlatformError::new("write_export_temporary", error.to_string()))?;
        drop(file);
        fs::rename(&temporary, target)
            .map_err(|error| PlatformError::new("replace_export_target", error.to_string()))
    })();
    if result.is_err() {
        // 临时路径由本次调用唯一创建，清理不会触碰用户既有目标。
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// 确定性测试 adapter，不模拟 SQLite 或领域规则。
pub mod test_support {
    use super::{
        ActivationHandler, ActivationRequest, InstanceLease, InstanceRole,
        NotificationProjectionKey, PlatformCommand, PlatformError, PlatformServices,
    };
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct State {
        primary: bool,
        handler: Option<ActivationHandler>,
        commands: Vec<PlatformCommand>,
        scheduled_notifications: BTreeSet<NotificationProjectionKey>,
        next_apply_error: Option<PlatformError>,
        next_file_write_error: Option<PlatformError>,
    }

    /// 可控制数据目录、激活和平台投影的测试 adapter。
    #[derive(Clone)]
    pub struct TestPlatformServices {
        data_directory: PathBuf,
        state: Arc<Mutex<State>>,
    }

    impl TestPlatformServices {
        /// 创建共享同一模拟平台状态的 adapter。
        pub fn new(data_directory: impl Into<PathBuf>) -> Self {
            Self {
                data_directory: data_directory.into(),
                state: Arc::new(Mutex::new(State::default())),
            }
        }

        /// 返回已经提交给平台的命令副本。
        pub fn recorded_commands(&self) -> Result<Vec<PlatformCommand>, PlatformError> {
            self.state
                .lock()
                .map(|state| state.commands.clone())
                .map_err(|error| PlatformError::new("read_test_commands", error.to_string()))
        }

        /// 让下一条平台投影失败，用于验证调用方的恢复语义。
        pub fn fail_next_apply(&self, message: impl Into<String>) -> Result<(), PlatformError> {
            self.state
                .lock()
                .map_err(|error| PlatformError::new("configure_test_failure", error.to_string()))?
                .next_apply_error = Some(PlatformError::new("test_platform_apply", message));
            Ok(())
        }

        /// 模拟 Explorer 丢失全部计划通知，领域事实保持不变。
        pub fn clear_scheduled_notifications(&self) -> Result<(), PlatformError> {
            self.state
                .lock()
                .map_err(|error| PlatformError::new("clear_test_notifications", error.to_string()))?
                .scheduled_notifications
                .clear();
            Ok(())
        }

        /// 让下一次原子文件写入在接触目标前失败。
        pub fn fail_next_file_write(
            &self,
            message: impl Into<String>,
        ) -> Result<(), PlatformError> {
            self.state
                .lock()
                .map_err(|error| PlatformError::new("configure_test_failure", error.to_string()))?
                .next_file_write_error = Some(PlatformError::new("test_atomic_write", message));
            Ok(())
        }
    }

    impl PlatformServices for TestPlatformServices {
        fn data_directory(&self) -> Result<PathBuf, PlatformError> {
            Ok(self.data_directory.clone())
        }

        fn acquire_single_instance(
            &self,
            initial_activation: ActivationRequest,
            on_activation: ActivationHandler,
        ) -> Result<InstanceRole, PlatformError> {
            let mut state = self.state.lock().map_err(|error| {
                PlatformError::new("acquire_single_instance", error.to_string())
            })?;
            if state.primary {
                let handler = state.handler.clone().ok_or_else(|| {
                    PlatformError::new("forward_activation", "主实例缺少激活处理器")
                })?;
                drop(state);
                handler(initial_activation);
                return Ok(InstanceRole::SecondaryForwarded);
            }

            state.primary = true;
            state.handler = Some(on_activation);
            Ok(InstanceRole::Primary(Box::new(TestInstanceLease {
                state: Arc::clone(&self.state),
            })))
        }

        fn apply(&self, command: PlatformCommand) -> Result<(), PlatformError> {
            let mut state = self
                .state
                .lock()
                .map_err(|error| PlatformError::new("apply_test_command", error.to_string()))?;
            if let Some(error) = state.next_apply_error.take() {
                return Err(error);
            }
            match &command {
                PlatformCommand::UpsertNotification {
                    reminder_id,
                    trigger_version,
                    ..
                } => {
                    state
                        .scheduled_notifications
                        .retain(|key| key.reminder_id != *reminder_id);
                    state
                        .scheduled_notifications
                        .insert(NotificationProjectionKey {
                            reminder_id: *reminder_id,
                            trigger_version: *trigger_version,
                        });
                }
                PlatformCommand::CancelNotification {
                    reminder_id,
                    trigger_version,
                } => {
                    state
                        .scheduled_notifications
                        .remove(&NotificationProjectionKey {
                            reminder_id: *reminder_id,
                            trigger_version: *trigger_version,
                        });
                }
                _ => {}
            }
            state.commands.push(command);
            Ok(())
        }

        fn scheduled_notifications(&self) -> Result<Vec<NotificationProjectionKey>, PlatformError> {
            self.state
                .lock()
                .map(|state| state.scheduled_notifications.iter().copied().collect())
                .map_err(|error| PlatformError::new("read_test_notifications", error.to_string()))
        }

        fn write_file_atomically(
            &self,
            target: &std::path::Path,
            contents: &[u8],
        ) -> Result<(), PlatformError> {
            if let Some(error) = self
                .state
                .lock()
                .map_err(|error| PlatformError::new("test_atomic_write", error.to_string()))?
                .next_file_write_error
                .take()
            {
                return Err(error);
            }
            super::portable_atomic_write(target, contents)
        }
    }

    struct TestInstanceLease {
        state: Arc<Mutex<State>>,
    }

    impl InstanceLease for TestInstanceLease {}

    impl Drop for TestInstanceLease {
        fn drop(&mut self) {
            if let Ok(mut state) = self.state.lock() {
                state.primary = false;
                state.handler = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::TestPlatformServices;
    use super::{
        ActivationRequest, GlobalShortcut, PRODUCT_IDENTITY, PlatformCommand, PlatformServices,
        ShortcutKey, activation_from_protocol_uri, reminder_protocol_uri,
    };
    use crate::{ReminderActivation, ReminderActivationAction};
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_adapter_forwards_second_instance_and_records_commands() {
        let adapter = TestPlatformServices::new("test-data");
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&received);
        let primary = adapter
            .acquire_single_instance(
                ActivationRequest::ShowMain,
                Arc::new(move |activation| {
                    sink.lock().expect("记录激活").push(activation);
                }),
            )
            .expect("获取主实例");
        assert!(primary.is_primary());

        let secondary = adapter
            .acquire_single_instance(ActivationRequest::ShowQuickCapture, Arc::new(|_| {}))
            .expect("转发第二实例");
        assert!(!secondary.is_primary());
        assert_eq!(
            *received.lock().expect("读取激活"),
            vec![ActivationRequest::ShowQuickCapture]
        );

        adapter
            .apply(PlatformCommand::SetTrayVisible { visible: true })
            .expect("记录平台命令");
        assert_eq!(
            adapter.recorded_commands().expect("读取平台命令"),
            vec![PlatformCommand::SetTrayVisible { visible: true }]
        );
        assert_eq!(PRODUCT_IDENTITY.protocol, "quicknote");
    }

    #[test]
    fn shortcut_policy_accepts_only_explicit_safe_combinations() {
        let default = GlobalShortcut::parse("ctrl + Alt + q").expect("解析默认快捷键");
        assert_eq!(default, GlobalShortcut::default());
        assert_eq!(default.to_string(), "Ctrl+Alt+Q");

        let bare_function = GlobalShortcut::parse("F13").expect("允许无修饰 F13");
        assert_eq!(bare_function.key(), ShortcutKey::Function(13));
        assert!(bare_function.modifiers().is_empty());

        for rejected in [
            "Q",
            "7",
            "Space",
            "Left",
            "F12",
            "Ctrl+Win+Q",
            "Ctrl+Alt",
            "Ctrl+Q+W",
        ] {
            assert!(
                GlobalShortcut::parse(rejected).is_err(),
                "{rejected} 必须被拒绝"
            );
        }
        assert!(GlobalShortcut::parse("Shift+Space").is_ok());
        assert!(GlobalShortcut::parse("Ctrl+PageDown").is_ok());
        assert!(GlobalShortcut::parse("Alt+F24").is_ok());
    }

    #[test]
    fn reminder_protocol_round_trips_only_the_strict_shape() {
        let activation = ReminderActivation {
            activation_key: uuid::Uuid::now_v7().to_string(),
            reminder_id: uuid::Uuid::now_v7(),
            trigger_version: 7,
            action: ReminderActivationAction::Snooze,
            snooze_minutes: Some(15),
        };
        let uri = reminder_protocol_uri(&activation);
        assert_eq!(
            activation_from_protocol_uri(&uri),
            ActivationRequest::Reminder(activation)
        );
        assert!(matches!(
            activation_from_protocol_uri("quicknote://reminder/open?reminder_id=bad"),
            ActivationRequest::ProtocolUri(_)
        ));
    }
}
