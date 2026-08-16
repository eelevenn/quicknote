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
    /// 当前归档便签数量。
    pub archived_note_count: u64,
    /// 当前回收站便签数量。
    pub trashed_note_count: u64,
    /// 当前便签；没有活跃便签时为空。
    pub current_note_id: Option<Uuid>,
    /// 新通知采用的默认稍后提醒时长。
    pub default_snooze_minutes: u16,
    /// 当前持久配置的全局快捷键。
    pub global_shortcut: GlobalShortcut,
    /// 是否在当前用户登录后启动 QuickNote。
    pub startup_enabled: bool,
}

/// 已持久化便签的严格生命周期。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteLifecycle {
    /// 可编辑且可成为当前便签。
    Active,
    /// 只读，可取消归档或移入回收站。
    Archived,
    /// 只读，可恢复到归档或永久清除。
    Trashed,
}

/// 主页便签列表使用的稳定摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NoteSummary {
    /// 便签身份。
    pub id: Uuid,
    /// 从正文第一条非空行派生的标题。
    pub title: String,
    /// 是否为持久化的当前便签。
    pub is_current: bool,
    /// 摘要所属生命周期。
    pub lifecycle: NoteLifecycle,
    /// 最近正文更新时间，用于稳定排序和紧凑元数据。
    pub updated_at_ms: i64,
    /// 用户语义上的截止时间。
    pub due_at_ms: Option<i64>,
    /// 当前单一提醒；归档与回收站便签恒为空。
    pub reminder: Option<ReminderSnapshot>,
}

/// 提醒的用户语义状态，不反映横幅是否实际可见。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderStatus {
    /// 未来时刻仍等待一次系统通知尝试。
    Scheduled,
    /// 触发时间已经过去，等待用户主动打开便签响应。
    Missed,
}

/// 一张活跃便签最多拥有的单一提醒快照。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReminderSnapshot {
    /// 稳定提醒身份；稍后提醒不会创建新身份。
    pub id: Uuid,
    /// UTC Unix 毫秒触发时间。
    pub scheduled_at_ms: i64,
    /// 当前提醒语义状态。
    pub status: ReminderStatus,
    /// 每次替换或稍后提醒都会递增的触发版本。
    pub trigger_version: u64,
    /// 领域事实尚未完全投影到 Windows。
    pub platform_sync_pending: bool,
    /// 最近一次平台失败的可展示说明。
    pub platform_sync_error: Option<String>,
}

/// 便签的截止时间与提醒组成的独立时间视图。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NoteTiming {
    /// 对应活跃便签身份。
    pub note_id: Uuid,
    /// 截止时间允许位于过去，也可以为空。
    pub due_at_ms: Option<i64>,
    /// 提醒必须位于未来；到点后变为错过提醒。
    pub reminder: Option<ReminderSnapshot>,
    /// 包括清除提醒在内的 Windows 投影仍在 outbox 中等待。
    pub platform_sync_pending: bool,
    /// 该便签最近一次平台投影失败说明。
    pub platform_sync_error: Option<String>,
}

/// 通知动作携带的领域意图。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderActivationAction {
    /// 打开便签并响应提醒。
    Open,
    /// 从动作发生时延后同一个提醒。
    Snooze,
}

/// 协议激活中的版本化提醒动作。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReminderActivation {
    /// 每个通知按钮固化的幂等键。
    pub activation_key: String,
    /// 稳定提醒身份。
    pub reminder_id: Uuid,
    /// 生成通知时的触发版本。
    pub trigger_version: u64,
    /// 打开或稍后提醒动作。
    pub action: ReminderActivationAction,
    /// 稍后提醒通知生成时固化的分钟数；打开动作为空。
    pub snooze_minutes: Option<u16>,
}

/// 幂等处理通知动作后的领域结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ReminderActivationOutcome {
    /// 重复、过时或非活跃便签动作没有改变事实。
    Ignored,
    /// 打开动作已响应提醒并把目标设为当前便签。
    Opened {
        /// 应显示并聚焦的活跃便签。
        note_id: Uuid,
    },
    /// 稍后提醒已重排同一个提醒，且不会打开窗口。
    Snoozed {
        /// 被重新安排的活跃便签。
        note_id: Uuid,
        /// 新的 UTC Unix 毫秒触发时间。
        scheduled_at_ms: i64,
    },
}

/// 提醒协调发生的生命周期阶段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReminderCoordinationReason {
    /// 应用完成冷启动后的首次协调。
    Startup,
    /// 正常前台或后台持续运行期间的周期协调。
    Continuous,
    /// 检测到系统从休眠恢复后的立即协调。
    Resume,
}

/// 一轮提醒协调的 UI 可观察摘要。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReminderCoordination {
    /// 仍等待平台投影的 outbox 条目数。
    pub pending_projection_count: u64,
    /// 本轮平台读取或写入失败；领域事实仍已保留。
    pub platform_error: Option<String>,
}

/// 一张未永久清除便签的领域文档。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NoteDocument {
    /// 稳定便签身份。
    pub id: Uuid,
    /// Markdown 原文，是正文的规范表示。
    pub body: String,
    /// 从正文派生的标题。
    pub title: String,
    /// 当前生命周期。
    pub lifecycle: NoteLifecycle,
    /// 单调内容修订。
    pub content_revision: u64,
    /// UTC Unix 毫秒创建时间。
    pub created_at_ms: i64,
    /// UTC Unix 毫秒更新时间。
    pub updated_at_ms: i64,
    /// UTC Unix 毫秒归档时间。
    pub archived_at_ms: Option<i64>,
    /// UTC Unix 毫秒移入回收站时间。
    pub trashed_at_ms: Option<i64>,
    /// 用户语义上的截止时间。
    pub due_at_ms: Option<i64>,
}

/// 主页三个生命周期分区的一致快照。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LibrarySnapshot {
    /// 按最近更新时间排序的活跃便签。
    pub active: Vec<NoteSummary>,
    /// 按归档时间排序的归档便签。
    pub archived: Vec<NoteSummary>,
    /// 按移入回收站时间排序的回收站便签。
    pub trashed: Vec<NoteSummary>,
}

/// 用户发起的合法生命周期转换。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoteAction {
    /// 活跃便签进入归档；当前便签使用稳定规则选择继任者。
    Archive(Uuid),
    /// 归档便签回到活跃；只有当前为空时才成为当前。
    Unarchive(Uuid),
    /// 归档便签进入回收站。
    MoveToTrash(Uuid),
    /// 回收站便签只恢复到归档。
    RestoreFromTrash(Uuid),
    /// 永久清除回收站便签及其备份历史。
    PermanentlyDelete(Uuid),
}

/// 搜索返回的生命周期感知结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    /// 命中的便签身份。
    pub id: Uuid,
    /// 派生标题。
    pub title: String,
    /// 只可能是活跃或归档。
    pub lifecycle: NoteLifecycle,
    /// 查询是否命中完整正文。
    pub matched_in_body: bool,
    /// 正文超过 1 MiB，不再属于性能保证范围，但没有截断。
    pub exceeds_performance_guarantee: bool,
}

/// Markdown 预览中只能由明确点击打开的外部链接。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MarkdownLink {
    /// 链接可见文字。
    pub label: String,
    /// 原始目标地址。
    pub url: String,
}

/// 安全 Markdown 预览，不包含可执行 HTML。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MarkdownPreview {
    /// 保留块级语义的安全纯文本。
    pub text: String,
    /// 预览中发现的 http(s) 链接。
    pub links: Vec<MarkdownLink>,
}

/// JSON 导出中的用户语义设置。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExportSettings {
    /// 持久化的全局快捷键规范串。
    pub global_shortcut: String,
    /// 是否在登录后启动。
    pub startup_enabled: bool,
    /// 默认稍后提醒分钟数。
    pub default_snooze_minutes: u16,
}

/// JSON 导出中的提醒事实；不包含 outbox 或动作收据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExportReminder {
    /// 稳定提醒身份。
    pub id: Uuid,
    /// 对应便签身份。
    pub note_id: Uuid,
    /// UTC Unix 毫秒触发时间。
    pub scheduled_at_ms: i64,
    /// `scheduled` 或 `missed` 用户语义状态。
    pub status: String,
    /// 单调触发版本。
    pub trigger_version: u64,
}

/// 版本化、无损的完整 JSON 导出契约。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExportBundle {
    /// 固定导出格式版本。
    pub format_version: u32,
    /// 生成快照的 UTC Unix 毫秒。
    pub exported_at_ms: i64,
    /// 导出时的当前活跃便签。
    pub current_note_id: Option<Uuid>,
    /// 全部未永久清除便签。
    pub notes: Vec<NoteDocument>,
    /// 全部领域提醒事实。
    pub reminders: Vec<ExportReminder>,
    /// 用户语义设置。
    pub settings: ExportSettings,
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
