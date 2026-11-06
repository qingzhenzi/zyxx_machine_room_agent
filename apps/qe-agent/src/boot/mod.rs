//! 引导流水线：agent 启动后的阶段化初始化状态机。
//!
//! 顺序（每个阶段失败不中断，只记录状态；除非该阶段是后续阶段的硬依赖）：
//!
//! 1. `ipc`        — IPC socket 就绪（外部步骤已先行，这里只登记）
//! 2. `firewall`   — 应用 nftables 配置（`qe-nftables`）
//! 3. `display`    — 按配置拉起 boot-display（全屏展示层，可选）
//! 4. `checks`     — 系统/虚拟化检查（platform-health + qeaudit + platform-diagnose）
//! 5. `enroll`     — 按配置注册到控制服务器（配置缺失/失败不阻塞）
//! 6. `services`   — 容器、虚拟化、P2P 服务初始化
//!
//! 状态经 IPC `/v1/state` 暴露给 GUI / boot-display。

pub mod checks;
pub mod config;
pub mod display;
pub mod enroll;
pub mod firewall;
pub mod monitor;
pub mod pipeline;
pub mod services;
pub mod state;

pub use pipeline::PipelineHandle;
pub use state::Phase;
