//! 会话管理：每路流一个独立 `gst-launch-1.0` 进程，可被管理 API 强杀。
//!
//! KWin 采集会额外拉起一个 `lan-video kwin-capture` 子进程持有 Wayland 连接，
//! 拿到 PipeWire node 后再启动 GStreamer；强杀时两个子进程一起结束。

use crate::pipeline::{
    Capture, PipelineConfig, build_audio_pipeline, build_sender_pipeline, resolve,
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::HashMap;
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "linux")]
use std::sync::mpsc;
#[cfg(target_os = "linux")]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(1);

/// control_token 生命周期：24 小时（超过后 authorize 拒绝）。
const TOKEN_TTL_MS: u128 = 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub target: String,
    pub rtp_port: u16,
    pub capture: String,
    pub encoder: String,
    pub pipeline: String,
    pub audio_port: Option<u16>,
    pub audio_pipeline: Option<String>,
    pub sync: bool,
    /// 是否 RTP-over-TCP 发送。
    pub tcp: bool,
    /// 控制通道/文件接口的会话 token（创建时返回）。
    pub control_token: String,
    /// SRTP 媒体加密密钥（None = 明文 RTP）。
    pub media_secret: Option<String>,
    pub log_path: String,
    pub pid: u32,
    pub created_ms: u128,
    /// control_token 过期时间（epoch ms）；到期后 authorize 拒绝。
    pub token_expires_ms: u128,
    pub state: String,
}

struct Entry {
    info: SessionInfo,
    children: Vec<Child>,
}

pub struct SessionManager {
    entries: Mutex<HashMap<String, Entry>>,
    dir: PathBuf,
}

impl SessionManager {
    pub fn new() -> Result<Self> {
        let dir = crate::state_dir().join("sessions");
        fs::create_dir_all(&dir).with_context(|| format!("创建目录 {}", dir.display()))?;
        Ok(Self {
            entries: Mutex::new(HashMap::new()),
            dir,
        })
    }

    /// 启动一路会话；返回会话元数据（含 pid / 日志路径 / 实际管线）。
    pub fn create(&self, cfg: &PipelineConfig) -> Result<SessionInfo> {
        let resolved = resolve(cfg)?;
        let id = format!(
            "{:x}-{:x}",
            now_ms(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let log_path = self.dir.join(format!("{id}.log"));
        let log =
            File::create(&log_path).with_context(|| format!("创建日志 {}", log_path.display()))?;

        let mut children: Vec<Child> = Vec::new();
        // 媒体加密：默认自动随机生成 SRTP 密钥；显式传 "plain" 才明文（仅测试/回环）。
        let media_secret = match cfg.media_secret.as_deref() {
            Some("plain") => None,
            Some("") | None => Some(crate::random_token()),
            Some(s) => Some(s.to_string()),
        };
        let effective = match &cfg.capture {
            #[cfg(target_os = "linux")]
            Capture::KWin { pointer, window } => {
                let (node, helper) =
                    spawn_kwin_helper(pointer.unwrap_or(2), window.as_deref(), &log_path)?;
                children.push(helper);
                let mut cfg = cfg.clone();
                cfg.capture = Capture::PipeWire {
                    path: Some(node),
                    fd: None,
                };
                cfg.media_secret = media_secret.clone();
                cfg
            }
            #[cfg(not(target_os = "linux"))]
            Capture::KWin { .. } => {
                bail!("KWin 采集仅支持 Linux");
            }
            _ => {
                let mut cfg = cfg.clone();
                cfg.media_secret = media_secret.clone();
                cfg
            }
        };

        let pipeline = build_sender_pipeline(&effective)?;
        let gst =
            crate::which("gst-launch-1.0").context("未找到 gst-launch-1.0，请先安装 GStreamer")?;
        let gst_log = log.try_clone()?;
        let gst_child = Command::new(&gst)
            .args(["-e", "-q"])
            .args(crate::tokenize_pipeline(&pipeline))
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(gst_log)
            .spawn()
            .context("启动 gst-launch-1.0 失败")?;
        let pid = gst_child.id();
        children.push(gst_child);

        // 可选音频：独立 gst 进程 + 独立 RTP 端口；sync 模式已合进视频管线。
        let audio_pipeline = match cfg.audio_port {
            Some(_) if !cfg.sync => {
                let mut audio_cfg = cfg.clone();
                audio_cfg.media_secret = media_secret.clone();
                let audio = build_audio_pipeline(&audio_cfg)?;
                let out = File::options()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                    .with_context(|| format!("打开日志 {}", log_path.display()))?;
                let err = out.try_clone()?;
                let child = Command::new(&gst)
                    .args(["-e", "-q"])
                    .args(crate::tokenize_pipeline(&audio))
                    .stdin(Stdio::null())
                    .stdout(out)
                    .stderr(err)
                    .spawn()
                    .context("启动音频 gst-launch-1.0 失败")?;
                children.push(child);
                Some(audio)
            }
            _ => None,
        };

        let info = SessionInfo {
            id: id.clone(),
            target: cfg.target.clone(),
            rtp_port: cfg.port,
            capture: cfg.capture.label().to_string(),
            encoder: resolved.encoder.label().to_string(),
            pipeline,
            audio_port: cfg.audio_port,
            audio_pipeline,
            sync: cfg.sync,
            tcp: cfg.tcp,
            control_token: crate::random_token(),
            media_secret,
            log_path: log_path.to_string_lossy().into_owned(),
            pid,
            created_ms: now_ms(),
            token_expires_ms: now_ms() + TOKEN_TTL_MS,
            state: "running".to_string(),
        };

        self.entries.lock().expect("session mutex").insert(
            id,
            Entry {
                info: info.clone(),
                children,
            },
        );
        Ok(info)
    }

    pub fn get(&self, id: &str) -> Option<SessionInfo> {
        self.entries
            .lock()
            .expect("session mutex")
            .get(id)
            .map(|entry| entry.info.clone())
    }

    pub fn authorize(&self, id: &str, token: &str) -> bool {
        let Some(info) = self.get(id) else {
            return false;
        };
        // token 生命周期：到期即拒。
        if now_ms() > info.token_expires_ms {
            return false;
        }
        if token.is_empty() {
            return false;
        }
        let a = info.control_token.as_bytes();
        let b = token.as_bytes();
        if a.len() != b.len() {
            return false;
        }
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= x ^ y;
        }
        diff == 0
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let mut map = self.entries.lock().expect("session mutex");
        for entry in map.values_mut() {
            let exited = entry
                .children
                .iter_mut()
                .any(|child| matches!(child.try_wait(), Ok(Some(_))));
            if exited {
                entry.info.state = "exited".to_string();
            }
        }
        let mut list: Vec<SessionInfo> = map.values().map(|e| e.info.clone()).collect();
        list.sort_by_key(|s| s.created_ms);
        list
    }

    /// 强杀：先 SIGINT 让 GStreamer/helper 收尾，超时再 SIGKILL。
    pub fn kill(&self, id: &str) -> Result<bool> {
        let entry = self.entries.lock().expect("session mutex").remove(id);
        let Some(entry) = entry else {
            return Ok(false);
        };
        for mut child in entry.children {
            terminate(&mut child);
        }
        Ok(true)
    }

    pub fn kill_all(&self) {
        let entries: Vec<Entry> = self
            .entries
            .lock()
            .expect("session mutex")
            .drain()
            .map(|(_, entry)| entry)
            .collect();
        for entry in entries {
            for mut child in entry.children {
                terminate(&mut child);
            }
        }
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        self.kill_all();
    }
}

/// 拉起 `lan-video kwin-capture`，等待它输出 `NODE=<id>`。
#[cfg(target_os = "linux")]
fn spawn_kwin_helper(pointer: u32, window: Option<&str>, log_path: &Path) -> Result<(u32, Child)> {
    let exe = std::env::current_exe().context("获取当前可执行文件")?;
    let err = File::options()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("打开日志 {}", log_path.display()))?;
    let mut args = vec![
        "kwin-capture".to_string(),
        "--pointer".to_string(),
        pointer.to_string(),
    ];
    if let Some(window) = window {
        args.push("--window".to_string());
        args.push(window.to_string());
    }
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(err)
        .spawn()
        .context("启动 kwin-capture helper 失败")?;

    let stdout = child.stdout.take().context("helper stdout 不可用")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(rest) = line.strip_prefix("NODE=") {
                if let Ok(node) = rest.trim().parse::<u32>() {
                    let _ = tx.send(node);
                }
            }
        }
    });

    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(node) => Ok((node, child)),
        Err(_) => {
            terminate(&mut child);
            bail!(
                "KWin 采集启动超时：检查 ~/.local/share/applications/lan-video-kwin.desktop \
                 与 kbuildsycoca6，或 KWin 未暴露 zkde_screencast_unstable_v1"
            );
        }
    }
}

fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status();
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::Encoder;
    use std::time::Duration;

    fn cfg() -> PipelineConfig {
        PipelineConfig {
            target: "127.0.0.1".into(),
            port: 45999,
            capture: Capture::Test,
            encoder: Encoder::Vah264,
            width: 640,
            height: 360,
            fps: 30,
            bitrate_kbps: 1000,
            audio_port: None,
            audio_source: crate::pipeline::AudioSource::System,
            audio_device: None,
            sync: false,
            tcp: false,
            media_secret: Some("plain".into()),
        }
    }

    #[test]
    fn start_and_kill_session() {
        if !crate::gst_available("vah264enc") {
            return;
        }
        let manager = SessionManager::new().unwrap();
        let info = manager.create(&cfg()).unwrap();
        let mut running = false;
        for _ in 0..30 {
            if manager
                .list()
                .iter()
                .any(|s| s.id == info.id && s.state == "running")
            {
                running = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(running, "会话未进入 running：{:?}", manager.list());
        assert!(manager.kill(&info.id).unwrap());
        assert!(manager.list().is_empty());
    }

    /// 控制鉴权：正确 token 通过、错误/空 token 拒绝。
    #[test]
    fn authorize_accepts_valid_and_rejects_bad_token() {
        if !crate::gst_available("vah264enc") {
            return;
        }
        let manager = SessionManager::new().unwrap();
        let info = manager.create(&cfg()).unwrap();
        assert!(
            manager.authorize(&info.id, &info.control_token),
            "正确 token 应通过"
        );
        assert!(
            !manager.authorize(&info.id, "wrong-token"),
            "错误 token 应拒绝"
        );
        assert!(!manager.authorize(&info.id, ""), "空 token 应拒绝");
        assert!(
            !manager.authorize("no-such-id", &info.control_token),
            "不存在的会话应拒绝"
        );
        let _ = manager.kill(&info.id);
    }

    /// token 生命周期：过期后 authorize 拒绝（模拟 token_expires_ms 已过）。
    #[test]
    fn authorize_rejects_expired_token() {
        if !crate::gst_available("vah264enc") {
            return;
        }
        let manager = SessionManager::new().unwrap();
        let info = manager.create(&cfg()).unwrap();
        assert!(manager.authorize(&info.id, &info.control_token));
        // 把过期时间改成过去。
        {
            let mut map = manager.entries.lock().unwrap();
            if let Some(entry) = map.get_mut(&info.id) {
                entry.info.token_expires_ms = now_ms().saturating_sub(1);
            }
        }
        assert!(
            !manager.authorize(&info.id, &info.control_token),
            "过期 token 应拒绝"
        );
        let _ = manager.kill(&info.id);
    }
}
