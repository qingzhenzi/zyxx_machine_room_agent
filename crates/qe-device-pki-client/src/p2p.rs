//! P2P 镜像分发：向 device-pki 登记本机 seed / 查询同网段 peer seed（mTLS）。
//!
//! 契约见 `room_agent/docs/p2p-image-distribution.md`：
//! - `POST /machine/p2p/seed`  登记 `(manifest_digest, serial)` 的 blob ticket；
//! - `GET  /machine/p2p/peers?digest=...` 查询未过期的其它设备 seed。
//!
//! ticket 即能力：拿到 ticket 的内网设备可以直接用 qep2p 下载该 blob，
//! 因此本模块只走既有的设备证书 mTLS 通道，不额外引入认证。

use crate::error::{Error, Result};
use crate::transport::{TransportConfig, build_mtls_client};
use serde::{Deserialize, Serialize};

/// `POST /machine/p2p/seed` 的响应。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SeedReply {
    pub ok: bool,
    /// RFC3339 时间戳。
    pub expires_at: String,
}

/// 一条 peer seed（查询响应里的元素）。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PeerSeed {
    pub serial: String,
    pub blob_ticket: String,
    pub size_bytes: u64,
    /// RFC3339 时间戳。
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
struct PeersReply {
    peers: Vec<PeerSeed>,
}

/// `POST /machine/p2p/seed` —— 登记/刷新本设备的镜像 seed。
pub async fn post_seed(
    cfg: &TransportConfig,
    manifest_digest: &str,
    blob_ticket: &str,
    size_bytes: u64,
) -> Result<SeedReply> {
    let client = build_mtls_client(cfg)?;
    let url = format!("{}/machine/p2p/seed", cfg.mtls_endpoint);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "manifest_digest": manifest_digest,
            "blob_ticket": blob_ticket,
            "size_bytes": size_bytes,
        }))
        .send()
        .await
        .map_err(|e| Error::Transport(format!("登记 P2P seed 失败（mTLS）: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Unexpected(format!(
            "POST /machine/p2p/seed 返回 {status}: {body}"
        )));
    }
    resp.json()
        .await
        .map_err(|e| Error::Transport(format!("解析 seed 响应失败: {e}")))
}

/// `GET /machine/p2p/peers?digest=...` —— 查询其它设备的可用 seed。
pub async fn fetch_peers(cfg: &TransportConfig, manifest_digest: &str) -> Result<Vec<PeerSeed>> {
    let client = build_mtls_client(cfg)?;
    let url = format!("{}/machine/p2p/peers", cfg.mtls_endpoint);
    let resp = client
        .get(&url)
        .query(&[("digest", manifest_digest)])
        .send()
        .await
        .map_err(|e| Error::Transport(format!("查询 P2P peers 失败（mTLS）: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Unexpected(format!(
            "GET /machine/p2p/peers 返回 {status}: {body}"
        )));
    }
    let reply: PeersReply = resp
        .json()
        .await
        .map_err(|e| Error::Transport(format!("解析 peers 响应失败: {e}")))?;
    Ok(reply.peers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_reply_parses() {
        let reply: SeedReply =
            serde_json::from_str(r#"{"ok":true,"expires_at":"2026-11-07T12:00:00Z"}"#).unwrap();
        assert!(reply.ok);
        assert_eq!(reply.expires_at, "2026-11-07T12:00:00Z");
    }

    #[test]
    fn peers_reply_parses_contract_fields() {
        let reply: PeersReply = serde_json::from_str(
            r#"{"peers":[{"serial":"abc","blob_ticket":"ticket","size_bytes":1024,
                "updated_at":"2026-11-07T12:00:00Z"}]}"#,
        )
        .unwrap();
        assert_eq!(reply.peers.len(), 1);
        let peer = &reply.peers[0];
        assert_eq!(peer.serial, "abc");
        assert_eq!(peer.blob_ticket, "ticket");
        assert_eq!(peer.size_bytes, 1024);
        assert_eq!(peer.updated_at, "2026-11-07T12:00:00Z");
    }

    #[test]
    fn peers_reply_rejects_missing_field() {
        assert!(serde_json::from_str::<PeersReply>(r#"{"peers":[{"serial":"abc"}]}"#).is_err());
    }
}
