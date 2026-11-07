//! 守护进程能力层：链接进 `qe-agent` 的功能库统一探测与展示。
//!
//! `qeaudit`（安全核查）、`qe-container`（容器）、`virt-*`（虚拟化）、
//! `qep2p`（P2P 传输）都以库形式并入守护进程，进程内直接使用；
//! 这里给出各能力的可用性快照（`qe-agent cli status` 末尾展示），
//! 供运维确认链路是否就绪。
//!
//! 遥测路径上的进程内调用见 `qe-agent-telemetry::external`（安全/容器/虚机采集），
//! 本模块负责能力的**就绪探测**，不做采集。

use std::path::Path;

use serde::Serialize;

/// 单项能力状态。
#[derive(Debug, Clone, Serialize)]
pub struct CapStatus {
    /// 能力名。
    pub name: &'static str,
    /// 是否就绪。
    pub available: bool,
    /// 就绪详情 / 失败原因。
    pub detail: String,
}

/// 探测全部守护进程能力（快照，不阻塞遥测）。
pub fn probe(data_dir: &Path) -> Vec<CapStatus> {
    vec![
        security(),
        containers(),
        virt(),
        transfer(data_dir),
    ]
}

/// 打印能力快照（`qe-agent cli status` 用）。
pub fn print(data_dir: &Path) {
    println!();
    println!("守护进程能力（库内直调）:");
    for cap in probe(data_dir) {
        let mark = if cap.available { "✓" } else { "✗" };
        println!("  {mark} {:<28} {}", cap.name, cap.detail);
    }
}

/// 安全基线核查（qeaudit）：进程内库恒可用，遥测在 full 档采集。
fn security() -> CapStatus {
    CapStatus {
        name: "qeaudit 安全核查",
        available: true,
        detail: format!("进程内 lib v{}", qeaudit::VERSION),
    }
}

/// 容器引擎（qe-container → Docker Engine API）。
fn containers() -> CapStatus {
    let outcome = crate::block_on(async {
        let engine = qe_container::Engine::connect_from_env().await?;
        engine.info().await
    });
    match outcome {
        Ok(info) => CapStatus {
            name: "qe-container 容器引擎",
            available: true,
            detail: format!(
                "已连接（engine {}，{} 个容器）",
                info.version, info.containers
            ),
        },
        Err(error) => CapStatus {
            name: "qe-container 容器引擎",
            available: false,
            detail: format!("{error}（遥测回退 docker CLI）"),
        },
    }
}

/// 虚拟化（virt-libvirt → libvirtd）。
fn virt() -> CapStatus {
    use virt_core::Hypervisor;

    // 一次连接搞定探测：连不上就是不可用；连上了顺带报版本和实际 URI。
    match virt_libvirt::LibvirtHypervisor::connect_best() {
        Ok((hv, uri)) => {
            let version = hv
                .version_string()
                .unwrap_or_else(|_| "版本未知".to_owned());
            CapStatus {
                name: "virt-libvirt 虚拟化",
                available: true,
                detail: format!("已连接（libvirt {version}，{uri}）"),
            }
        }
        Err(error) => CapStatus {
            name: "virt-libvirt 虚拟化",
            available: false,
            detail: format!("libvirt 不可用（{error}）；遥测回退 virsh CLI"),
        },
    }
}

/// P2P 传输（qep2p）：守护进程持有节点身份，供内网文件传输使用。
fn transfer(data_dir: &Path) -> CapStatus {
    let state_dir = data_dir.join("p2p");
    match qep2p::load_or_create_identity(&state_dir) {
        Ok(id) => CapStatus {
            name: "qep2p P2P 传输",
            available: true,
            detail: format!("节点身份就绪（{}…，{}）", qep2p::short_id(&id), state_dir.display()),
        },
        Err(error) => CapStatus {
            name: "qep2p P2P 传输",
            available: false,
            detail: format!("{error}"),
        },
    }
}
