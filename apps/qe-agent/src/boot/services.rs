//! 阶段 6：服务初始化（容器 / 虚拟化 / P2P）。
//!
//! * **P2P 真正启动**：在专用 tokio runtime 线程里 `qep2p::Node::start`，
//!   局域网 mDNS 发现开启；节点句柄存入 [`P2P_HANDLE`] 供后续复用
//!   （文件分享 / boot-display 推送等）。
//! * **容器 / 虚拟化深度探测**：真实连接 Docker Engine API / libvirtd，
//!   报告版本与连接 URI；失败只标记状态，不阻塞 boot。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::json;

use crate::boot::state::SharedState;

/// 已启动的 P2P 节点（进程内单例）。
static P2P_HANDLE: OnceLock<Mutex<Option<Arc<qep2p::Node>>>> = OnceLock::new();

fn p2p_slot() -> &'static Mutex<Option<Arc<qep2p::Node>>> {
    P2P_HANDLE.get_or_init(|| Mutex::new(None))
}

/// 取已启动的 P2P 节点（未启动返回 `None`）。
pub fn p2p_node() -> Option<Arc<qep2p::Node>> {
    p2p_slot().lock().ok().and_then(|guard| guard.clone())
}

pub fn run(state: &SharedState, data_dir: &Path) {
    state.begin(crate::boot::Phase::Services);

    let mut summary: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    // 1) 虚拟化：真实连接 libvirtd。
    let virt_detail = probe_virt();
    match &virt_detail {
        Ok((uri, version)) => summary.push(format!("libvirt 就绪（{uri}，{version}）")),
        Err(error) => failed.push(format!("libvirt: {error}")),
    }

    // 2) 容器：真实连接 Docker Engine API。
    let container_detail = probe_container();
    match &container_detail {
        Ok((version, count)) => summary.push(format!("容器引擎就绪（{version}，{count} 个容器）")),
        Err(error) => failed.push(format!("容器引擎: {error}")),
    }

    // 3) P2P：真正启动节点（mDNS 局域网发现）。
    match start_p2p(data_dir) {
        Ok(short_id) => summary.push(format!("P2P 节点已启动（{short_id}）")),
        Err(error) => failed.push(format!("P2P: {error}")),
    }

    let detail = serde_json::json!({
        "libvirt": virt_detail.as_ref().ok().map(|(uri, v)| json!({ "uri": uri, "version": v })),
        "container": container_detail.as_ref().ok().map(|(v, c)| json!({ "version": v, "containers": c })),
        "p2p": p2p_node().map(|node| json!({ "id": qep2p::short_id(&node.id()) })),
    });

    if failed.is_empty() {
        state.finish(
            crate::boot::Phase::Services,
            "ok",
            summary.join("；"),
            Some(detail),
        );
    } else {
        state.finish(
            crate::boot::Phase::Services,
            "failed",
            format!("{}（{}）", summary.join("；"), failed.join("；")),
            Some(detail),
        );
    }
}

/// 连接 libvirtd，返回 `(uri, version)`。
fn probe_virt() -> Result<(String, String), String> {
    use virt_core::Hypervisor;
    let (hv, uri) =
        virt_libvirt::LibvirtHypervisor::connect_best().map_err(|error| format!("{error:#}"))?;
    let version = hv
        .version_string()
        .unwrap_or_else(|_| "版本未知".to_owned());
    Ok((uri, version))
}

/// 连接 Docker Engine API，返回 `(version, container_count)`。
fn probe_container() -> Result<(String, i64), String> {
    crate::block_on(async {
        let engine = qe_container::Engine::connect_from_env()
            .await
            .map_err(|error| format!("{error:#}"))?;
        let info = engine.info().await.map_err(|error| format!("{error:#}"))?;
        Ok((info.version, info.containers))
    })
}

/// 启动 qep2p 节点（专用 runtime 线程；幂等，重复调用返回已有节点 ID）。
fn start_p2p(data_dir: &Path) -> Result<String, String> {
    if let Some(node) = p2p_node() {
        return Ok(qep2p::short_id(&node.id()));
    }

    let state_dir: PathBuf = data_dir.join("p2p");
    let config = qep2p::NodeConfig::new(&state_dir);
    let handle = std::thread::Builder::new()
        .name("qe-agent-p2p".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("P2P runtime 启动失败");
            runtime.block_on(async move { qep2p::Node::start(config).await })
        })
        .map_err(|error| format!("启动 P2P 线程失败: {error}"))?;

    let node = handle
        .join()
        .map_err(|_| "P2P 线程 panic".to_owned())?
        .map_err(|error| format!("{error:#}"))?;
    let short = qep2p::short_id(&node.id());

    if let Ok(mut guard) = p2p_slot().lock() {
        *guard = Some(Arc::new(node));
    }
    Ok(short)
}
