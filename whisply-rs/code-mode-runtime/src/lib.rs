mod cell_actor;
mod runtime;
mod service;
mod session_runtime;
mod v8_init;

pub(crate) type TaskFailureHandler = std::sync::Arc<dyn Fn(String) + Send + Sync>;

pub use service::InProcessCodeModeSession;
pub use v8_init::V8JitMode;
pub use v8_init::initialize_v8;
pub use whisply_code_mode_protocol::*;
