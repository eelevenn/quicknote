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
}
