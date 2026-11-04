use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::SecretKey;
use qep2p::{Node, NodeConfig, RelayChoice};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("qep2p-v2-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cfg(state_dir: &Path, inbox: Option<PathBuf>) -> NodeConfig {
    let mut cfg = NodeConfig::new(state_dir);
    cfg.relay = RelayChoice::Disabled;
    cfg.mdns = false;
    cfg.inbox = inbox;
    cfg
}

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn share_and_download() -> Result<()> {
    let base = temp_dir("share");
    let provider = Node::start(cfg(&base.join("a"), None)).await?;
    let downloader = Node::start(cfg(&base.join("b"), None)).await?;

    let src = base.join("payload.bin");
    let data = payload(300_000);
    std::fs::write(&src, &data)?;

    let shared = provider.share_path(&src).await?;
    assert_eq!(shared.size, data.len() as u64);

    let out = base.join("out");
    let got = downloader.download(&shared.ticket, &out).await?;
    assert_eq!(got.bytes, data.len() as u64);
    assert_eq!(std::fs::read(&got.path)?, data, "下载内容不一致");

    provider.shutdown().await?;
    downloader.shutdown().await?;
    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_to_inbox_and_keep_old_file() -> Result<()> {
    let base = temp_dir("push");
    let inbox = base.join("inbox");
    let receiver = Node::start(cfg(&base.join("recv"), Some(inbox.clone()))).await?;
    let sender = Node::start(cfg(&base.join("send"), None)).await?;

    let src = base.join("report.bin");
    let data = payload(200_000);
    std::fs::write(&src, &data)?;

    let first = sender
        .send_file(receiver.addr(), &src, Duration::from_secs(30))
        .await
        .context("第一次推送")?;
    assert_eq!(first.bytes, data.len() as u64);
    let first_path = inbox.join("report.bin");
    assert_eq!(std::fs::read(&first_path)?, data);

    // 再推一次同名文件，不能覆盖，应出现 "report (1).bin"。
    let second = sender
        .send_file(receiver.addr(), &src, Duration::from_secs(30))
        .await
        .context("第二次推送")?;
    let second_path = inbox.join("report (1).bin");
    assert!(second_path.exists(), "第二次应改名保存：{second:?}");
    assert_eq!(std::fs::read(&second_path)?, data);

    receiver.shutdown().await?;
    sender.shutdown().await?;
    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn allowlist_rejects_unknown_sender() -> Result<()> {
    let base = temp_dir("allow");

    let stranger = SecretKey::generate().public();
    let mut recv_cfg = cfg(&base.join("recv"), Some(base.join("inbox")));
    recv_cfg.allow = vec![stranger];
    let receiver = Node::start(recv_cfg).await?;
    let sender = Node::start(cfg(&base.join("send"), None)).await?;

    let src = base.join("blocked.bin");
    std::fs::write(&src, payload(1024))?;

    let err = sender
        .send_file(receiver.addr(), &src, Duration::from_secs(10))
        .await
        .expect_err("不在 allowlist 的节点必须被拒绝");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("拒绝") || msg.contains("closed") || msg.contains("not allowed"),
        "错误信息不对：{msg}"
    );
    assert!(!base.join("inbox/blocked.bin").exists());

    receiver.shutdown().await?;
    sender.shutdown().await?;
    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ping_control_channel() -> Result<()> {
    let base = temp_dir("ping");
    let receiver = Node::start(cfg(&base.join("recv"), None)).await?;
    let sender = Node::start(cfg(&base.join("send"), None)).await?;

    let outcome = sender
        .ping(
            receiver.addr(),
            3,
            Duration::from_millis(10),
            Duration::from_secs(10),
        )
        .await?;
    assert_eq!(outcome.rtts.len(), 3);
    assert!(outcome.rtts.iter().all(|d| d.as_millis() < 5000));

    receiver.shutdown().await?;
    sender.shutdown().await?;
    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}
