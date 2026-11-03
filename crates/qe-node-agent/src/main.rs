//! 房间节点 agent 入口（独立二进制 `qe-node-agent`）。
//!
//! 运行逻辑见 [`qe_node_agent::run`] / [`qe_node_agent::run_with_config`]；
//! 统一入口 `qe-agent run --role node` 复用同一实现。
//! 启动流程、策略优先级与导入逻辑的说明保留在库的模块文档中。

#![deny(unsafe_code)]

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    eprintln!(
        "提示：qe-node-agent 已并入统一入口，请改用 `qe-agent run --role node`（旧二进制将在 P4 退场）"
    );
    qe_node_agent::run().await
}
