//! QEStudio 房间机器 agent（room-agent）库。
//!
//! 采集主机清单、内置指标、健康结论与可选外部工具（platform-health /
//! platform-diagnose / qeaudit / docker / virsh）的结果，组装成
//! [`report::Report`]（契约见 `docs/machine-telemetry.md` v1.1），再上报给服务端：
//!
//! - **mTLS（默认）**：`POST {DEVICE_PKI_MTLS_URL}/machine/report`，身份来自设备证书
//!   （`<DEVICE_DATA_DIR>/creds/<DEVICE_SLOT>`）；
//! - **Bearer（兼容旧部署）**：`POST {MACHINE_API_BASE}/machine/report` +
//!   `Authorization: Bearer <MACHINE_AGENT_TOKEN>`。
//!
//! 两档采集：
//! - `fast`：只跑内置采集（/proc、df、hwmon），默认 10s；
//! - `full`：额外跑外部工具，默认 300s。
//!
//! 两档间隔可由服务端 `GET /agent/config` 下发（global → area → room → device 合并），
//! 见 [`policy`]；远端拉取失败只用本地 env/默认值，绝不阻塞上报。
//!
//! 设计约束：
//! - 内置采集只用 std（读 `/proc`、`/sys`、`df`），避免平台重依赖；
//! - 外部工具全部可选，单项失败/超时只写进 `sections.agent.tools`，绝不影响上报；
//! - 采集与 payload 组装不触碰网络，便于 `--once --print` 本地排查。
//!
//! 模块划分：
//! - [`cli`] / [`config`]：命令行与环境变量配置（`REPORT_MODE` / 两档间隔）；
//! - [`collect`]：内置采集器（host / cpu / memory / disk / network / thermal）；
//! - [`process`]：带硬超时的子进程执行器（外部工具与 `df` 共用）；
//! - [`external`]：五个可选外部工具的调用与解析（fast 档整体跳过）；
//! - [`health`]：平台健康结论采纳 + 内置阈值兜底；
//! - [`mtls`]：设备凭证加载与 blocking mTLS 客户端；
//! - [`policy`]：启动/运行期拉取远端两档间隔；
//! - [`report`]：契约 payload 类型、校验与 HTTP 上报（mTLS / Bearer）。

#![deny(unsafe_code)]

pub mod cli;
pub mod collect;
pub mod config;
pub mod external;
pub mod health;
pub mod live;
pub mod mtls;
pub mod policy;
pub mod process;
pub mod report;
pub mod run;

pub use run::{run_from_args, run_with_cli};
