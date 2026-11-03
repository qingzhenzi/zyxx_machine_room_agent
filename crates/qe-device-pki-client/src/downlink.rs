//! 下行消息（服务端 → 设备）的拉取、处理与游标持久化。
//!
//! 与服务端 `qe_device_pki::messaging` 对称：
//! - 服务端按 kind 分发上行消息给业务；本模块按 kind 分发下行消息给设备侧业务
//! - **拉取不等于投递**：本地 handler 全部成功后才 ack，否则下次还会拿到同一批
//! - 游标 `last_id` 落盘（0600），重启后从上次位置继续
//!
//! 典型用法（设备侧）：
//!
//! ```ignore
//! let d = downlink::Dispatcher::new();
//! d.subscribe("config", |m| async move { /* 应用配置 */ Ok(()) });
//! d.subscribe_all(|m| async move { println!("{}: {}", m.kind, m.payload); Ok(()) });
//! downlink::poll_once(&cfg, &d, &state_path).await?;
//! ```

use crate::error::{Error, Result};
use crate::transport::{DownlinkMessage, TransportConfig, ack_downlink, fetch_downlink};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, RwLock};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 设备侧的下行消息处理器。任何 `Fn(DownlinkMessage) -> impl Future` 都满足。
///
/// 约定：**幂等**。因为服务端是至少一次投递，处理失败时消息会再次下发。
pub trait MessageHandler: Send + Sync + 'static {
    fn handle(&self, msg: DownlinkMessage) -> BoxFuture<'static, Result<()>>;
}

impl<F, Fut> MessageHandler for F
where
    F: Fn(DownlinkMessage) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    fn handle(&self, msg: DownlinkMessage) -> BoxFuture<'static, Result<()>> {
        Box::pin((self)(msg))
    }
}

/// 按 `kind` 分发的处理器注册表（`"*"` 是通配）。
pub struct Dispatcher {
    handlers: RwLock<HashMap<String, Vec<Arc<dyn MessageHandler>>>>,
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Dispatcher {
    pub fn new() -> Self {
        Self {
            handlers: RwLock::new(HashMap::new()),
        }
    }

    pub fn subscribe<H: MessageHandler>(&self, kind: &str, handler: H) {
        self.subscribe_arc(kind, Arc::new(handler));
    }

    pub fn subscribe_all<H: MessageHandler>(&self, handler: H) {
        self.subscribe_arc("*", Arc::new(handler));
    }

    fn subscribe_arc(&self, kind: &str, handler: Arc<dyn MessageHandler>) {
        self.handlers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(kind.to_owned())
            .or_default()
            .push(handler);
    }

    /// 分发一条消息。`Err` 表示处理失败（调用方应保留游标，下次重投）。
    pub async fn dispatch(&self, msg: DownlinkMessage) -> Result<()> {
        let targets: Vec<Arc<dyn MessageHandler>> = {
            let map = self.handlers.read().unwrap_or_else(|e| e.into_inner());
            let mut v = Vec::new();
            if let Some(list) = map.get(&msg.kind) {
                v.extend(list.iter().cloned());
            }
            if let Some(list) = map.get("*") {
                v.extend(list.iter().cloned());
            }
            v
        };
        if targets.is_empty() {
            // 没有订阅方：按"已知悉"处理（ack 掉），否则这批消息会永远卡在队列里
            tracing::debug!(id = msg.id, kind = %msg.kind, "没有订阅的下行类型，跳过");
            return Ok(());
        }
        for handler in targets {
            handler.handle(msg.clone()).await?;
        }
        Ok(())
    }
}

// ---------------- 游标持久化 ----------------

/// 已处理到的下行游标。
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, Default)]
pub struct DownlinkState {
    pub last_id: i64,
}

pub fn state_path(slot: &str, data_dir: Option<&Path>) -> Result<PathBuf> {
    let base = match data_dir {
        Some(d) => d.to_path_buf(),
        None => crate::trust::default_data_dir()?,
    };
    Ok(base.join(format!("downlink-{slot}.json")))
}

pub fn load_state(path: &Path) -> Result<DownlinkState> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).map_err(Into::into),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DownlinkState::default()),
        Err(e) => Err(Error::Storage(e.to_string())),
    }
}

pub fn save_state(state: &DownlinkState, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(state)?;
    crate::trust::write_private(path, &json)?;
    Ok(())
}

// ---------------- 拉取 + 处理 + ACK ----------------

/// 拉一批下行消息，全部处理成功后 ack 并推进本地游标。
///
/// 返回本次处理的条数。任何一条 handler 失败 → **不 ack**（下次还会拿到），
/// 游标也不推进，保证至少一次。
pub async fn poll_once(
    cfg: &TransportConfig,
    dispatcher: &Dispatcher,
    state_file: &Path,
    batch: i64,
) -> Result<usize> {
    let mut state = load_state(state_file)?;
    let messages = fetch_downlink(cfg, state.last_id, batch).await?;
    if messages.is_empty() {
        return Ok(0);
    }
    let count = messages.len();

    let mut max_id = state.last_id;
    for msg in messages {
        max_id = max_id.max(msg.id);
        if let Err(e) = dispatcher.dispatch(msg.clone()).await {
            tracing::error!(
                id = msg.id,
                kind = %msg.kind,
                error = %e,
                "下行消息处理失败，不 ack，下轮重投"
            );
            return Err(e);
        }
    }

    // 处理成功才 ack + 推进游标（与上行"成功才推水位"同一套原则）
    let acked = ack_downlink(cfg, max_id).await?;
    state.last_id = max_id;
    save_state(&state, state_file)?;
    tracing::debug!(
        acked,
        last_id = max_id,
        processed = count,
        "下行消息已处理并 ack"
    );
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn msg(id: i64, kind: &str) -> DownlinkMessage {
        DownlinkMessage {
            id,
            kind: kind.into(),
            payload: "{}".into(),
            created_at: None,
        }
    }

    #[tokio::test]
    async fn dispatch_routes_by_kind_and_wildcard() {
        let d = Dispatcher::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = Arc::clone(&hits);
        d.subscribe("config", move |_m: DownlinkMessage| {
            let h = Arc::clone(&h);
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        let all = Arc::new(AtomicUsize::new(0));
        let a = Arc::clone(&all);
        d.subscribe_all(move |_m: DownlinkMessage| {
            let a = Arc::clone(&a);
            async move {
                a.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });

        d.dispatch(msg(1, "config")).await.unwrap();
        d.dispatch(msg(2, "other")).await.unwrap();
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "只有 config 命中专用 handler"
        );
        assert_eq!(all.load(Ordering::SeqCst), 2, "通配收到全部");
    }

    #[tokio::test]
    async fn missing_kind_is_skipped_not_fatal() {
        let d = Dispatcher::new();
        // 没有订阅方也要 ack 掉，否则这批消息永远卡在队列里
        assert!(d.dispatch(msg(1, "unknown")).await.is_ok());
    }

    #[tokio::test]
    async fn handler_failure_bubbles_up() {
        let d = Dispatcher::new();
        d.subscribe("bad", |_m: DownlinkMessage| async {
            Err(Error::Unexpected("坏消息".into()))
        });
        assert!(d.dispatch(msg(1, "bad")).await.is_err());
    }

    #[test]
    fn state_roundtrips_with_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        let state = DownlinkState { last_id: 42 };
        save_state(&state, &path).unwrap();
        assert_eq!(load_state(&path).unwrap().last_id, 42);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "游标文件也要 0600");
        }

        // 缺失文件 = 从 0 开始
        assert_eq!(
            load_state(&dir.path().join("none.json")).unwrap().last_id,
            0
        );
    }
}
