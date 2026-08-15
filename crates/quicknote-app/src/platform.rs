//! 仅承载真实平台差异的 `PlatformServices` seam。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

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

/// 平台壳送入共享应用的激活事件。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ActivationRequest {
    /// 显示并聚焦主页。
    ShowMain,
    /// 显示并聚焦快速记录窗。
    ShowQuickCapture,
    /// 保留协议载荷，后续纵向切片再解析领域动作。
    ProtocolUri(String),
    /// 系统从挂起状态恢复，需要应用协调本地事实。
    Resumed,
}

/// 由应用提交、在事务完成后执行的平台投影。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PlatformCommand {
    /// 注册或替换全局快捷键。
    SetGlobalShortcut {
        /// 已通过应用规则验证的快捷键表示。
        accelerator: String,
    },
    /// 移除当前全局快捷键。
    ClearGlobalShortcut,
    /// 创建或替换一个带触发版本的系统通知。
    UpsertNotification {
        /// 领域提醒 UUID。
        reminder_id: Uuid,
        /// 防止旧投影覆盖新提醒的单调版本。
        trigger_version: u64,
        /// UTC Unix 毫秒触发时刻。
        scheduled_at_ms: i64,
        /// 用户可见标题。
        title: String,
        /// 用户可见正文。
        body: String,
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
}

/// 确定性测试 adapter，不模拟 SQLite 或领域规则。
pub mod test_support {
    use super::{
        ActivationHandler, ActivationRequest, InstanceLease, InstanceRole, PlatformCommand,
        PlatformError, PlatformServices,
    };
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct State {
        primary: bool,
        handler: Option<ActivationHandler>,
        commands: Vec<PlatformCommand>,
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
            self.state
                .lock()
                .map_err(|error| PlatformError::new("apply_test_command", error.to_string()))?
                .commands
                .push(command);
            Ok(())
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
    use super::{ActivationRequest, PRODUCT_IDENTITY, PlatformCommand, PlatformServices};
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
}
