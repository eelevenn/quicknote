use crate::platform::GlobalShortcut;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// 启动应用模块所需的平台中立配置。
#[derive(Clone, Debug)]
pub struct ApplicationConfig {
    data_directory: PathBuf,
}

impl ApplicationConfig {
    /// 使用平台 adapter 提供的私有数据目录创建配置。
    pub fn new(data_directory: impl Into<PathBuf>) -> Self {
        Self {
            data_directory: data_directory.into(),
        }
    }

    /// 返回模块拥有的私有数据目录。
    pub fn data_directory(&self) -> &Path {
        &self.data_directory
    }
}

/// UI 可提交的领域命令；SQLite 细节不会穿过此接口。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// 更新未来通知所使用的默认稍后提醒时长。
    SetDefaultSnoozeMinutes {
        /// 允许 5、10、15、30 或 60 分钟。
        minutes: u16,
    },
}

/// 命令成功后的可观察结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CommandResult {
    /// 命令已经在一个事务中完整提交。
    Applied,
}

/// 数据库身份的只读诊断视图。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaIdentity {
    /// 固定 SQLite `application_id`。
    pub application_id: i32,
    /// 当前已经迁移到的 schema 版本。
    pub version: i32,
}

/// UI 和测试通过应用接口读取的不可变快照。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplicationSnapshot {
    /// 当前数据库身份。
    pub schema: SchemaIdentity,
    /// 当前活跃便签数量。
    pub active_note_count: u64,
    /// 当前便签；没有活跃便签时为空。
    pub current_note_id: Option<Uuid>,
    /// 新通知采用的默认稍后提醒时长。
    pub default_snooze_minutes: u16,
    /// 当前持久配置的全局快捷键。
    pub global_shortcut: GlobalShortcut,
}

/// 主页便签列表使用的稳定摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NoteSummary {
    /// 活跃便签身份。
    pub id: Uuid,
    /// 从正文第一条非空行派生的标题。
    pub title: String,
    /// 是否为持久化的当前便签。
    pub is_current: bool,
}

/// 当前内存修订的自动保存状态。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SaveState {
    /// 没有身份且正文仅含空白，不会创建数据库行。
    BlankDraft,
    /// 最新内存修订正在等待 250 ms 尾随或 1 秒最大等待。
    Scheduled,
    /// 一个修订正在 SQLite 单写者中提交。
    Saving,
    /// 最新内存修订已经真实提交。
    Saved,
    /// 保存失败，正文仍在内存中且可以重试。
    Failed {
        /// 不泄漏 schema 的可展示错误。
        message: String,
    },
}

/// 主页与快速记录共享的编辑状态快照。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EditingSnapshot {
    /// 已持久化的便签身份；空白草稿或首次保存未提交时为空。
    pub note_id: Option<Uuid>,
    /// 当前内存正文。
    pub body: String,
    /// 从当前内存正文实时派生的标题。
    pub title: String,
    /// 当前内存修订。
    pub revision: u64,
    /// 最近真实提交的修订。
    pub saved_revision: u64,
    /// 当前自动保存状态。
    pub save_state: SaveState,
    /// 按最近更新时间排列的活跃便签。
    pub active_notes: Vec<NoteSummary>,
}

/// UI 通过单一应用接口提交的编辑意图。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditorIntent {
    /// 用控件中的完整正文替换内存正文并安排自动保存。
    ReplaceBody(String),
    /// 立即刷新最新修订；失败时调用方必须中止关闭或退出。
    Flush,
    /// 先刷新旧编辑，再原子切换持久化的当前便签。
    SwitchCurrent(Uuid),
    /// 先刷新旧编辑，再进入没有身份的唯一空白草稿。
    NewBlankDraft,
    /// 先刷新当前编辑，再重新打开持久化的当前便签。
    OpenCurrent,
    /// 对保留在内存中的失败修订发起非阻塞重试。
    RetrySave,
}
