//! 真实 `qemu-img` 集成测试（不依赖 libvirt）：差分盘 → 快照 → 导出。
//!
//! 目的：验证阶段 1.5 的磁盘命令在真实 qemu-img 上可用，而不是只在 mock 里通过。
//! 本机没有 qemu-img 时自动 skip（CI 环境友好）。测试全部在临时目录进行，
//! 不触碰任何生产镜像/虚拟机。
//!
//! 另有一个 `#[ignore]` 的 libvirt define 测试，需要 `LIBVIRT_URI=qemu:///session`：
//! `cargo test -p qe-agent-node --test vm_disk_qemu_img -- --ignored`。

use std::path::Path;

use qe_agent_node::vm_control::{
    SnapshotAction, create_overlay_disk, qemu_img_export, qemu_img_info_chain, qemu_img_snapshot,
    qemu_img_snapshot_list, sha256_file,
};

fn qemu_img_available() -> bool {
    let bin = std::env::var("QE_QEMU_IMG").unwrap_or_else(|_| "qemu-img".to_owned());
    std::process::Command::new(bin)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// 16 MiB 的稀疏 qcow2 底包（真正分配几乎为 0）。
fn make_base(path: &Path) {
    create_overlay_disk(path, None, 1).expect("创建底包应成功");
}

#[test]
fn qemu_img_overlay_snapshot_export_roundtrip() {
    if !qemu_img_available() {
        eprintln!("跳过：本机没有 qemu-img（可设 QE_QEMU_IMG 指定路径）");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base.qcow2");
    make_base(&base);
    assert!(base.is_file(), "底包应存在");

    // 1) 差分盘：backing file 指向底包
    let overlay = dir.path().join("vms").join("vm-1").join("disk.qcow2");
    create_overlay_disk(&overlay, Some(&base), 2).expect("创建差分盘应成功");

    // 2) disk.info --backing-chain：链长 2，第二层是底包
    let info = qemu_img_info_chain(&overlay).expect("读取磁盘链应成功");
    let chain = info["chain"].as_array().expect("chain 应是数组");
    assert_eq!(chain.len(), 2, "差分盘 + 底包: {info}");
    assert_eq!(info["disks"][0]["format"], "qcow2");
    assert!(
        info["disks"][0]["backing_path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("base.qcow2"),
        "第一层 backing 应指向底包: {info}"
    );
    assert!(
        info["disks"][1]["path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("base.qcow2"),
        "第二层应是底包: {info}"
    );

    // 3) 快照 create/list/restore/delete（qemu-img 内部快照）
    qemu_img_snapshot(&overlay, SnapshotAction::Create, Some("snap1")).expect("创建快照");
    let snapshots = qemu_img_snapshot_list(&overlay).expect("列出快照");
    assert_eq!(snapshots.len(), 1, "{snapshots:?}");
    assert_eq!(snapshots[0]["tag"], "snap1");
    assert!(snapshots[0]["date"].as_str().unwrap_or_default().len() >= 10);

    qemu_img_snapshot(&overlay, SnapshotAction::Restore, Some("snap1")).expect("恢复快照");
    qemu_img_snapshot(&overlay, SnapshotAction::Delete, Some("snap1")).expect("删除快照");
    assert!(
        qemu_img_snapshot_list(&overlay).unwrap().is_empty(),
        "删除后应没有快照"
    );

    // 4) export：convert 成扁平 qcow2 + sha256/size
    let exports = dir.path().join("exports");
    std::fs::create_dir_all(&exports).unwrap();
    let dest = exports.join("vm-1-test.qcow2");
    let (sha256, size_bytes) = qemu_img_export(&overlay, &dest, "qcow2").expect("导出磁盘");
    assert!(dest.is_file());
    assert_eq!(sha256.len(), 64, "sha256 hex: {sha256}");
    assert!(size_bytes > 0);
    assert_eq!(sha256, sha256_file(&dest).unwrap(), "sha256 应可复算");

    // 导出的扁平盘不应再有 backing file
    let exported_chain = qemu_img_info_chain(&dest).expect("读取导出盘信息");
    assert_eq!(
        exported_chain["chain"].as_array().unwrap().len(),
        1,
        "扁平化后 chain 应只有 1 层: {exported_chain}"
    );
}

#[test]
fn create_overlay_disk_rejects_missing_base() {
    if !qemu_img_available() {
        eprintln!("跳过：本机没有 qemu-img");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.qcow2");
    let overlay = dir.path().join("overlay.qcow2");
    let result = create_overlay_disk(&overlay, Some(&missing), 1);
    assert!(result.is_err(), "不存在的 backing file 应失败");
}

/// 真实 libvirt define 测试（qemu:///session，手工触发）：
///
/// ```bash
/// LIBVIRT_URI=qemu:///session cargo test -p qe-agent-node --test vm_disk_qemu_img \
///     -- --ignored create_defined_in_session_uri
/// ```
#[test]
#[ignore = "需要 libvirt/qemu:///session，手工 --ignored 触发；不碰生产域"]
fn create_defined_in_session_uri() {
    use qe_agent_node::vm_control::{CreateArgs, LibvirtExecutor, VmExecutor};
    if std::env::var("LIBVIRT_URI").is_err() {
        eprintln!("跳过：未设置 LIBVIRT_URI（例如 qemu:///session）");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let executor = LibvirtExecutor::new(dir.path());
    let args = CreateArgs {
        name: format!("qe-test-{}", std::process::id()),
        vcpus: 1,
        memory_mib: 256,
        disk_gb: 1,
        base_volume: None,
        base_path: None,
        gpu: Vec::new(),
        start: false,
    };
    let data = executor
        .create(&args)
        .expect("session libvirt define 应成功");
    assert_eq!(data["defined"], true);
    assert_eq!(data["started"], false);
    assert!(Path::new(data["disk_path"].as_str().unwrap()).is_file());
}
