use std::path::PathBuf;
use thiserror::Error;

/// 应用模块向 UI 和测试公开的稳定错误分类。
#[derive(Debug, Error)]
pub enum ApplicationError {
    /// 调用方提交了不满足领域规则的命令。
    #[error("命令无效：{message}")]
    InvalidCommand {
        /// 面向调用方的规则说明。
        message: String,
    },

    /// 数据目录或 SQLite 操作失败。
    #[error("存储操作 {operation} 失败：{message}")]
    Storage {
        /// 不包含表名的逻辑操作名。
        operation: &'static str,
        /// 底层错误的可记录说明。
        message: String,
    },

    /// 数据库不属于 QuickNote，必须避免静默覆盖。
    #[error("数据库身份不匹配：期望 {expected:#x}，实际 {found:#x}")]
    DatabaseIdentity {
        /// 当前客户端固定的身份。
        expected: i32,
        /// 数据库中发现的身份。
        found: i32,
    },

    /// 数据库 schema 比当前客户端更新，只允许升级客户端后再写入。
    #[error("数据库 schema 版本 {found} 高于客户端支持版本 {supported}")]
    UnsupportedSchema {
        /// 数据库中的未来版本。
        found: i32,
        /// 当前客户端最高支持版本。
        supported: i32,
    },

    /// 迁移前备份失败，因此迁移没有开始。
    #[error("迁移前备份失败：{message}")]
    MigrationBackup {
        /// 备份失败说明。
        message: String,
    },

    /// 迁移事务已经回滚，备份路径可用于诊断或恢复。
    #[error("schema 从版本 {from} 迁移到 {to} 失败：{message}")]
    Migration {
        /// 迁移前版本。
        from: i32,
        /// 目标版本。
        to: i32,
        /// 已验证的迁移前备份；新空库没有备份。
        backup_path: Option<PathBuf>,
        /// 迁移步骤失败说明。
        message: String,
    },

    /// 后台单写者不可用，调用方可以展示可重试错误。
    #[error("SQLite 单写者不可用：{message}")]
    WriterUnavailable {
        /// 通道或后台线程错误说明。
        message: String,
    },
}

impl ApplicationError {
    /// 将内部 SQLite 错误折叠为不泄漏表结构的可观察错误。
    pub(crate) fn storage(operation: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Storage {
            operation,
            message: error.to_string(),
        }
    }
}
