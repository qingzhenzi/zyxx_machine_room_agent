//! 应用内「安装 / 初始化 daemon」支持。
//!
//! GUI 以普通用户运行；安装 daemon 需要 root，因此通过 `pkexec` 调用打包资源里的
//! `resources/agent-setup.sh`（systemd 单元/env 模板同目录）。本模块只做：
//! - `agent_setup_probe`：只读环境探测（systemd/pkexec/已安装状态/组权限）；
//! - `agent_setup_install`：pkexec 安装+可选启动 daemon；
//! - `agent_setup_service`：start/stop/restart。
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};
use tokio::process::Command;

const DEFAULT_SOCKET: &str = "/run/qe-agent/agent.sock";
const SETUP_SCRIPT: &str = "resources/agent-setup.sh";
/// 嵌入安装助手与模板：安装脚本只复制 GUI 二进制，资源不能依赖 bundle 目录。
const EMBEDDED_SETUP_SCRIPT: &str = include_str!("../resources/agent-setup.sh");
const EMBEDDED_UNIT: &str = include_str!("../resources/qe-agent.service");
const EMBEDDED_ENV: &str = include_str!("../resources/qe-agent.env.example");

#[derive(Debug, Clone, Serialize)]
pub struct SetupProbe {
    pub platform: String,
    pub systemd: bool,
    pub pkexec: Option<String>,
    pub agent_bin: Option<String>,
    pub unit_installed: bool,
    pub service_active: bool,
    pub service_enabled: bool,
    pub group_exists: bool,
    pub user_in_group: bool,
    pub current_user: String,
    pub socket_path: String,
    pub setup_script: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupResult {
    pub ok: bool,
    pub code: Option<i32>,
    pub command: String,
    pub stdout: String,
    pub stderr: String,
}

fn which(bin: &str) -> Option<String> {
    let output = std::process::Command::new("which").arg(bin).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn command_ok(program: &str, args: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn user_in_group(user: &str, group: &str) -> bool {
    let output = std::process::Command::new("id")
        .args(["-nG", user])
        .output();
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .any(|item| item == group),
        _ => false,
    }
}

fn first_existing(paths: &[PathBuf]) -> Option<String> {
    paths
        .iter()
        .find(|path| path.is_file())
        .map(|path| path.display().to_string())
}

fn resolve_setup_script(app: &AppHandle) -> Option<PathBuf> {
    if let Some(path) = app
        .path()
        .resolve(SETUP_SCRIPT, BaseDirectory::Resource)
        .ok()
        .filter(|path| path.is_file())
    {
        return Some(path);
    }
    for candidate in [
        PathBuf::from("/usr/share/qe-agent-client/agent-setup.sh"),
        PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".local/share/qe-agent-client/agent-setup.sh"),
    ] {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // 开发模式（cargo run / tauri dev）资源目录可能未复制，回退到源码树。
    let fallback = Path::new(env!("CARGO_MANIFEST_DIR")).join(SETUP_SCRIPT);
    fallback.is_file().then_some(fallback)
}

/// 把嵌入的安装脚本与模板写到临时目录（pkexec 运行期间存在，结束后删除）。
fn materialize_setup() -> Result<PathBuf, String> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.subsec_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("qe-agent-setup-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建安装临时目录失败: {error}"))?;
    let script = dir.join("agent-setup.sh");
    {
        let mut file =
            std::fs::File::create(&script).map_err(|error| format!("写入安装脚本失败: {error}"))?;
        file.write_all(EMBEDDED_SETUP_SCRIPT.as_bytes())
            .map_err(|error| format!("写入安装脚本失败: {error}"))?;
    }
    let _ = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700));
    std::fs::write(dir.join("qe-agent.service"), EMBEDDED_UNIT)
        .map_err(|error| format!("写入 systemd 模板失败: {error}"))?;
    std::fs::write(dir.join("qe-agent.env.example"), EMBEDDED_ENV)
        .map_err(|error| format!("写入 env 模板失败: {error}"))?;
    Ok(script)
}

fn probe_impl(app: &AppHandle) -> SetupProbe {
    let user = current_user();
    let systemd = Path::new("/run/systemd/system").exists();
    let setup_script = resolve_setup_script(app);
    let agent_bin = first_existing(&[
        PathBuf::from("/usr/local/bin/qe-agent"),
        PathBuf::from("/usr/bin/qe-agent"),
    ])
    .or_else(|| {
        std::env::var("QE_AGENT_BIN")
            .ok()
            .filter(|path| Path::new(path).is_file())
    });
    SetupProbe {
        platform: std::env::consts::OS.to_owned(),
        systemd,
        pkexec: which("pkexec"),
        agent_bin,
        unit_installed: Path::new("/etc/systemd/system/qe-agent.service").is_file(),
        service_active: command_ok("systemctl", &["is-active", "--quiet", "qe-agent"]),
        service_enabled: command_ok("systemctl", &["is-enabled", "--quiet", "qe-agent"]),
        group_exists: command_ok("getent", &["group", "qe-agent"]),
        user_in_group: user_in_group(&user, "qe-agent"),
        current_user: user,
        socket_path: DEFAULT_SOCKET.to_owned(),
        setup_script: setup_script
            .map(|path| path.display().to_string())
            .or_else(|| Some("embedded".to_owned())),
    }
}

#[tauri::command]
pub fn agent_setup_probe(app: AppHandle) -> SetupProbe {
    probe_impl(&app)
}

/// 首次启动/初始化动画需要显示主窗口（窗口在 tauri.conf 中默认隐藏）。
#[tauri::command]
pub fn agent_show_main_window(app: AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

async fn run_helper(
    action: &str,
    extra: &[String],
    setup_script: &Path,
    cleanup: Option<PathBuf>,
) -> Result<SetupResult, String> {
    let pkexec = which("pkexec").ok_or_else(|| {
        "未找到 pkexec（PolicyKit）；请安装 polkit 或用 sudo 手动执行安装脚本".to_owned()
    })?;
    let mut args: Vec<String> = vec![
        "/bin/sh".to_owned(),
        setup_script.display().to_string(),
        action.to_owned(),
    ];
    args.extend(extra.iter().cloned());
    let command_text = format!("{pkexec} {}", args.join(" "));
    let child = Command::new(&pkexec)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("启动 pkexec 失败: {error}"))?;
    let output = tokio::time::timeout(Duration::from_secs(300), child.wait_with_output())
        .await
        .map_err(|_| "安装/服务操作超时（300s）".to_owned())?
        .map_err(|error| format!("等待 pkexec 失败: {error}"))?;
    if let Some(dir) = cleanup {
        let _ = std::fs::remove_dir_all(dir);
    }
    Ok(SetupResult {
        ok: output.status.success(),
        code: output.status.code(),
        command: command_text,
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

#[tauri::command]
pub async fn agent_setup_install(
    app: AppHandle,
    agent_bin: String,
    gui_user: Option<String>,
    start: Option<bool>,
    allow_poweroff: Option<bool>,
    fast_interval_secs: Option<u64>,
    full_interval_secs: Option<u64>,
    live_interval_ms: Option<u64>,
    live_push: Option<bool>,
) -> Result<SetupResult, String> {
    let bin = Path::new(agent_bin.trim());
    if !bin.is_absolute() || !bin.is_file() {
        return Err(format!("agent 二进制必须是存在的绝对路径：{}", agent_bin));
    }
    if let Some(fast) = fast_interval_secs {
        if !(1..=60).contains(&fast) {
            return Err("遥测 fast 间隔必须在 1-60 秒之间".to_owned());
        }
    }
    if let Some(full) = full_interval_secs {
        if !(60..=3600).contains(&full) {
            return Err("遥测 full 间隔必须在 60-3600 秒之间".to_owned());
        }
    }
    if let (Some(fast), Some(full)) = (fast_interval_secs, full_interval_secs) {
        if full < fast {
            return Err("遥测 full 间隔不能小于 fast 间隔".to_owned());
        }
    }
    if let Some(live) = live_interval_ms {
        if live > 5_000 {
            return Err("实时指标间隔必须在 0-5000 毫秒之间（0 关闭本地采样）".to_owned());
        }
    }
    let (setup_script, cleanup) = match resolve_setup_script(&app) {
        Some(path) => (path, None),
        None => {
            let path = materialize_setup()?;
            let dir = path.parent().map(Path::to_path_buf);
            (path, dir)
        }
    };
    let mut extra = vec!["--agent-bin".to_owned(), bin.display().to_string()];
    if let Some(user) = gui_user.filter(|user| !user.trim().is_empty()) {
        extra.push("--gui-user".to_owned());
        extra.push(user);
    }
    if start.unwrap_or(true) {
        extra.push("--start".to_owned());
    }
    match allow_poweroff {
        Some(true) => extra.push("--allow-poweroff".to_owned()),
        Some(false) => extra.push("--no-allow-poweroff".to_owned()),
        None => {}
    }
    if let Some(fast) = fast_interval_secs {
        extra.push("--fast-interval".to_owned());
        extra.push(fast.to_string());
    }
    if let Some(full) = full_interval_secs {
        extra.push("--full-interval".to_owned());
        extra.push(full.to_string());
    }
    if let Some(live) = live_interval_ms {
        extra.push("--live-interval-ms".to_owned());
        extra.push(live.to_string());
    }
    match live_push {
        Some(true) => extra.push("--live-push".to_owned()),
        Some(false) => extra.push("--no-live-push".to_owned()),
        None => {}
    }
    run_helper("install", &extra, &setup_script, cleanup).await
}

#[tauri::command]
pub async fn agent_setup_enroll(app: AppHandle, link: String) -> Result<SetupResult, String> {
    let link = link.trim();
    if !(link.starts_with("https://") || link.starts_with("http://")) {
        return Err("注册链接必须是 http(s) URL".to_owned());
    }
    let (setup_script, cleanup) = match resolve_setup_script(&app) {
        Some(path) => (path, None),
        None => {
            let path = materialize_setup()?;
            let dir = path.parent().map(Path::to_path_buf);
            (path, dir)
        }
    };
    run_helper(
        "enroll",
        &["--link".to_owned(), link.to_owned()],
        &setup_script,
        cleanup,
    )
    .await
}

#[tauri::command]
pub async fn agent_setup_service(app: AppHandle, action: String) -> Result<SetupResult, String> {
    let action = action.trim().to_ascii_lowercase();
    if !["start", "stop", "restart"].contains(&action.as_str()) {
        return Err("action 只能是 start/stop/restart".to_owned());
    }
    let (setup_script, cleanup) = match resolve_setup_script(&app) {
        Some(path) => (path, None),
        None => {
            let path = materialize_setup()?;
            let dir = path.parent().map(Path::to_path_buf);
            (path, dir)
        }
    };
    run_helper(&action, &[], &setup_script, cleanup).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_current_platform() {
        let probe = SetupProbe {
            platform: std::env::consts::OS.to_owned(),
            systemd: false,
            pkexec: None,
            agent_bin: None,
            unit_installed: false,
            service_active: false,
            service_enabled: false,
            group_exists: false,
            user_in_group: false,
            current_user: current_user(),
            socket_path: DEFAULT_SOCKET.to_owned(),
            setup_script: None,
        };
        assert!(!probe.platform.is_empty());
        assert!(!probe.current_user.is_empty());
        assert_eq!(probe.socket_path, DEFAULT_SOCKET);
    }
}
