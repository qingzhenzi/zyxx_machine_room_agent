//! Phase1 VM 远程控制执行器（libvirt，经 `virt-crate::Backend`）。
//!
//! 契约（控制面 ↔ agent，`vm.command` 下行）：
//!
//! ```json
//! {"request_id":"<uuid>","op":"list|start|shutdown|reboot|destroy|screenshot",
//!  "args":{"name":"vm-name"}}
//! ```
//!
//! - 除 `list` 外 `args.name` 必填；
//! - `list` 返回 `{"vms":[{name,state,vcpus,memory_bytes,vnc}]}`（vnc 可为 null）；
//! - 生命周期命令返回 `{"name","result":"ok"}`；
//! - `screenshot` 返回 PNG（QEMU≥7 原生给 PNG，旧驱动给 PNM，这里统一转 PNG），
//!   base64 后超过 4 MiB 直接报错；
//! - 每命令默认 30s 超时（screenshot 20s），阻塞调用跑在 `spawn_blocking` 里，
//!   panic 只让该条命令失败，不拖垮 agent。
//!
//! 执行结果由调用方通过 `qe_device_pki_client::command::report_command_result`
//! 回传 `POST /machine/command/result`（见 `run.rs` 的 `vm.command` 订阅）。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use virt_crate::{Backend, DomainState, GraphicsInfo};

/// 普通命令超时。
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// 截图超时。
pub const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(20);
/// 截图 base64 字符串上限（4 MiB）。
pub const MAX_SCREENSHOT_BASE64_BYTES: usize = 4 * 1024 * 1024;

/// 下行 `vm.command` 的操作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmOp {
    List,
    Start,
    Shutdown,
    Reboot,
    Destroy,
    Screenshot,
}

impl VmOp {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Start => "start",
            Self::Shutdown => "shutdown",
            Self::Reboot => "reboot",
            Self::Destroy => "destroy",
            Self::Screenshot => "screenshot",
        }
    }

    const fn requires_name(self) -> bool {
        !matches!(self, Self::List)
    }
}

/// `vm.command` 的 `args`。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VmCommandArgs {
    #[serde(default)]
    pub name: Option<String>,
}

/// 一条下行命令。
#[derive(Debug, Clone, Deserialize)]
pub struct VmCommand {
    pub request_id: Uuid,
    pub op: VmOp,
    #[serde(default)]
    pub args: VmCommandArgs,
}

impl VmCommand {
    /// 解析并做入参校验（除 list 外必须有非空 name）。
    pub fn parse(payload: &str) -> Result<Self> {
        let command: Self = serde_json::from_str(payload)
            .context("vm.command payload 不是合法 JSON / 缺字段")?;
        command.validate()?;
        Ok(command)
    }

    /// 入参校验。
    pub fn validate(&self) -> Result<()> {
        if self.op.requires_name() {
            let name = self
                .args
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| anyhow::anyhow!("op={} 需要 args.name", self.op.as_str()))?;
            if name.contains('/') || name.contains("..") {
                bail!("非法 VM 名: {name:?}");
            }
        }
        Ok(())
    }

    /// 执行时使用的 VM 名（list 为空串）。
    fn target_name(&self) -> String {
        self.args
            .name
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_owned()
    }

    fn timeout(&self) -> Duration {
        if self.op == VmOp::Screenshot {
            SCREENSHOT_TIMEOUT
        } else {
            COMMAND_TIMEOUT
        }
    }
}

/// 命令执行结果（回传 device-pki `POST /machine/command/result`）。
#[derive(Debug, Clone)]
pub struct CommandResult {
    pub request_id: Uuid,
    pub op: VmOp,
    pub ok: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}

impl CommandResult {
    fn success(command: &VmCommand, data: Value) -> Self {
        Self {
            request_id: command.request_id,
            op: command.op,
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    fn failure(command: &VmCommand, error: impl Into<String>) -> Self {
        Self {
            request_id: command.request_id,
            op: command.op,
            ok: false,
            data: None,
            error: Some(error.into()),
        }
    }
}

/// 可 mock 的 VM 后端抽象（生产实现见 [`LibvirtExecutor`]）。
pub trait VmExecutor: Send + Sync {
    /// `list` 的 JSON（`{"vms":[...]}`）。
    fn list(&self) -> Result<Value>;
    /// 生命周期动作（已校验 name）。
    fn lifecycle(&self, op: VmOp, name: &str) -> Result<()>;
    /// 截图原始字节（PNG 或 PNM）。
    fn screenshot(&self, name: &str) -> Result<Vec<u8>>;
}

/// 生产实现：每条命令独立连接 libvirt（`virt-crate::Backend`），失败给出可读错误。
pub struct LibvirtExecutor;

impl LibvirtExecutor {
    fn backend() -> Result<Backend> {
        Backend::auto().map_err(|error| {
            anyhow::anyhow!(
                "libvirt/KVM 不可用（确认 libvirtd 在运行、agent 有 qemu:///system 权限）: {error}"
            )
        })
    }
}

impl VmExecutor for LibvirtExecutor {
    fn list(&self) -> Result<Value> {
        let hypervisor = Self::backend()?;
        let domains = hypervisor.list_domains().map_err(|error| {
            anyhow::anyhow!("列出虚拟机失败（libvirt）: {error}")
        })?;
        let vms = domains
            .into_iter()
            .map(|domain| {
                // 控制台信息是可选增强：读不到不影响列表。
                let vnc = hypervisor
                    .graphics_of(&domain.name)
                    .ok()
                    .and_then(|graphics| graphics.into_iter().next())
                    .map(graphics_json);
                json!({
                    "name": domain.name,
                    "state": state_str(domain.state),
                    "vcpus": domain.vcpus,
                    "memory_bytes": domain.max_memory_kib.saturating_mul(1024),
                    "vnc": vnc,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({ "vms": vms }))
    }

    fn lifecycle(&self, op: VmOp, name: &str) -> Result<()> {
        let hypervisor = Self::backend()?;
        let domain = hypervisor
            .domain(name)
            .map_err(|error| anyhow::anyhow!("找不到虚拟机 {name:?}: {error}"))?;
        let result = match op {
            VmOp::Start => domain.start(),
            VmOp::Shutdown => domain.shutdown(),
            VmOp::Reboot => domain.reboot(),
            VmOp::Destroy => domain.destroy(),
            VmOp::List | VmOp::Screenshot => bail!("op={} 不是生命周期命令", op.as_str()),
        };
        result.map_err(|error| anyhow::anyhow!("{} {name:?} 失败: {error}", op.as_str()))
    }

    fn screenshot(&self, name: &str) -> Result<Vec<u8>> {
        let hypervisor = Self::backend()?;
        hypervisor
            .screenshot_of(name)
            .map_err(|error| anyhow::anyhow!("截图 {name:?} 失败: {error}"))
    }
}

fn state_str(state: DomainState) -> &'static str {
    match state {
        DomainState::ShutOff => "shutoff",
        DomainState::Running => "running",
        DomainState::Paused => "paused",
        DomainState::ShuttingDown => "shutting_down",
        DomainState::Crashed => "crashed",
        DomainState::Unknown => "unknown",
        // `DomainState` 是 non_exhaustive：未来 libvirt 新增状态时保持向后兼容。
        _ => "unknown",
    }
}

fn graphics_json(info: GraphicsInfo) -> Value {
    json!({
        "host": info.host,
        "port": info.port,
        // 不主动暴露明文密码；控制面需要时由管理员从宿主机配置获取。
        "password": info.password_value,
    })
}

/// 解析下行 payload 并执行；payload 解析失败返回 `Err`（无法关联 request_id）。
pub async fn execute_payload(executor: Arc<dyn VmExecutor>, payload: &str) -> Result<CommandResult> {
    let command = VmCommand::parse(payload)?;
    Ok(execute(executor, command).await)
}

/// 执行一条已解析命令（带超时、panic 隔离）。
pub async fn execute(executor: Arc<dyn VmExecutor>, command: VmCommand) -> CommandResult {
    let timeout = command.timeout();
    execute_with_timeout(executor, command, timeout).await
}

/// 同 [`execute`]，超时可注入（单测用短超时）。
pub async fn execute_with_timeout(
    executor: Arc<dyn VmExecutor>,
    command: VmCommand,
    timeout: Duration,
) -> CommandResult {
    let op = command.op;
    let name = command.target_name();

    let task = tokio::task::spawn_blocking(move || -> Result<Value> {
        match op {
            VmOp::List => executor.list(),
            VmOp::Screenshot => {
                let raw = executor.screenshot(&name)?;
                screenshot_payload(raw)
            }
            VmOp::Start | VmOp::Shutdown | VmOp::Reboot | VmOp::Destroy => {
                executor.lifecycle(op, &name)?;
                Ok(json!({ "name": name, "result": "ok" }))
            }
        }
    });

    match tokio::time::timeout(timeout, task).await {
        Err(_) => CommandResult::failure(
            &command,
            format!("命令执行超时（{}s）", timeout.as_secs()),
        ),
        Ok(Err(join_error)) if join_error.is_panic() => {
            // 阻塞任务 panic 已被隔离在这里，agent 主进程不受影响。
            CommandResult::failure(&command, format!("命令执行 panic（已隔离）: {join_error}"))
        }
        Ok(Err(join_error)) => {
            CommandResult::failure(&command, format!("命令执行任务失败: {join_error}"))
        }
        Ok(Ok(Ok(data))) => CommandResult::success(&command, data),
        Ok(Ok(Err(error))) => CommandResult::failure(&command, format!("{error:#}")),
    }
}

/// 截图：原生 PNG 直接透传，PNM 转 PNG；base64 后 >4 MiB 拒绝。
pub fn screenshot_payload(raw: Vec<u8>) -> Result<Value> {
    let png_bytes = if is_png(&raw) {
        raw
    } else {
        pnm_to_png(&raw)?
    };
    let image_base64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    if image_base64.len() > MAX_SCREENSHOT_BASE64_BYTES {
        bail!(
            "截图过大：base64 {} 字节 > 4 MiB 上限",
            image_base64.len()
        );
    }
    Ok(json!({
        "format": "png",
        "image_base64": image_base64,
        "size_bytes": png_bytes.len(),
    }))
}

/// PNG 文件签名。
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

fn is_png(raw: &[u8]) -> bool {
    raw.len() >= PNG_MAGIC.len() && raw[..PNG_MAGIC.len()] == PNG_MAGIC
}

/// 把 QEMU 旧驱动返回的 PNM（P6 RGB / P5 灰度）转成 PNG。
fn pnm_to_png(raw: &[u8]) -> Result<Vec<u8>> {
    let (pixels, width, height, color) = parse_pnm(raw)?;
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .context("编码截图 PNG 失败（header）")?;
        writer
            .write_image_data(&pixels)
            .context("编码截图 PNG 失败（像素）")?;
    }
    Ok(out)
}

struct PnmCursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> PnmCursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    /// 跳过空白与 `#` 注释，返回下一个 token。
    fn next_token(&mut self) -> Result<&'a [u8]> {
        loop {
            while self
                .data
                .get(self.position)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.position += 1;
            }
            if self.data.get(self.position) == Some(&b'#') {
                while self.data.get(self.position).is_some_and(|byte| *byte != b'\n') {
                    self.position += 1;
                }
                continue;
            }
            break;
        }
        let start = self.position;
        while self
            .data
            .get(self.position)
            .is_some_and(|byte| !byte.is_ascii_whitespace())
        {
            self.position += 1;
        }
        if start == self.position {
            bail!("PNM 头被截断");
        }
        Ok(&self.data[start..self.position])
    }

    fn next_u32(&mut self, label: &str) -> Result<u32> {
        let token = self.next_token()?;
        let text = std::str::from_utf8(token)
            .with_context(|| format!("PNM {label} 不是 ASCII: {token:?}"))?;
        text.parse::<u32>()
            .with_context(|| format!("PNM {label} 不是整数: {text:?}"))
    }
}

/// 解析 PNM（P5/P6，8-bit）；返回 `(像素, 宽, 高, PNG 色彩类型)`。
fn parse_pnm(raw: &[u8]) -> Result<(Vec<u8>, u32, u32, png::ColorType)> {
    if !raw.starts_with(b"P5") && !raw.starts_with(b"P6") {
        bail!("截图不是 PNG，也不是受支持的 PNM（P5/P6）");
    }
    let is_rgb = raw.starts_with(b"P6");
    let mut cursor = PnmCursor::new(raw);
    let _magic = cursor.next_token()?;
    let width = cursor.next_u32("width")?;
    let height = cursor.next_u32("height")?;
    let max_value = cursor.next_u32("maxval")?;
    if width == 0 || height == 0 {
        bail!("PNM 尺寸非法: {width}x{height}");
    }
    if max_value == 0 || max_value > 255 {
        bail!("PNM maxval={max_value} 不支持（仅 8-bit）");
    }
    // 头结束后恰好一个空白字符（容忍 CRLF）。
    match raw.get(cursor.position) {
        Some(b'\r') => {
            cursor.position += 1;
            if raw.get(cursor.position) == Some(&b'\n') {
                cursor.position += 1;
            }
        }
        Some(byte) if byte.is_ascii_whitespace() => cursor.position += 1,
        _ => bail!("PNM 头与像素数据之间缺少空白"),
    }
    let channels = if is_rgb { 3usize } else { 1usize };
    let expected = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().map(|h| (w, h)))
        .and_then(|(w, h)| w.checked_mul(h))
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or_else(|| anyhow::anyhow!("PNM 尺寸溢出: {width}x{height}"))?;
    let raster = raw
        .get(cursor.position..)
        .ok_or_else(|| anyhow::anyhow!("PNM 像素数据被截断"))?;
    if raster.len() < expected {
        bail!(
            "PNM 像素数据不足：需要 {expected} 字节，实际 {}",
            raster.len()
        );
    }
    let color = if is_rgb {
        png::ColorType::Rgb
    } else {
        png::ColorType::Grayscale
    };
    Ok((raster[..expected].to_vec(), width, height, color))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ---------------- payload 解析 ----------------

    #[test]
    fn parses_list_command() {
        let command = VmCommand::parse(
            r#"{"request_id":"0b6f9d4e-4a41-4b6e-9d0b-4f8b0f3c2a11","op":"list"}"#,
        )
        .unwrap();
        assert_eq!(command.op, VmOp::List);
        assert!(command.args.name.is_none());
    }

    #[test]
    fn parses_lifecycle_command_with_name() {
        let command = VmCommand::parse(
            r#"{"request_id":"0b6f9d4e-4a41-4b6e-9d0b-4f8b0f3c2a11",
                "op":"start","args":{"name":"vm-1"}}"#,
        )
        .unwrap();
        assert_eq!(command.op, VmOp::Start);
        assert_eq!(command.args.name.as_deref(), Some("vm-1"));
    }

    #[test]
    fn rejects_missing_name_for_lifecycle_ops() {
        let payload = r#"{"request_id":"0b6f9d4e-4a41-4b6e-9d0b-4f8b0f3c2a11","op":"start"}"#;
        let error = VmCommand::parse(payload).unwrap_err().to_string();
        assert!(error.contains("args.name"), "{error}");
    }

    #[test]
    fn rejects_bad_payloads() {
        // 未知 op
        assert!(
            VmCommand::parse(
                r#"{"request_id":"0b6f9d4e-4a41-4b6e-9d0b-4f8b0f3c2a11","op":"eject"}"#
            )
            .is_err()
        );
        // request_id 不是 UUID
        assert!(VmCommand::parse(r#"{"request_id":"nope","op":"list"}"#).is_err());
        // 非 JSON
        assert!(VmCommand::parse("not-json").is_err());
        // 非法 VM 名（路径穿越）
        assert!(
            VmCommand::parse(
                r#"{"request_id":"0b6f9d4e-4a41-4b6e-9d0b-4f8b0f3c2a11",
                    "op":"start","args":{"name":"../etc/passwd"}}"#
            )
            .is_err()
        );
    }

    // ---------------- 执行（mock 后端） ----------------

    #[derive(Default)]
    struct MockExecutor {
        panic_on_screenshot: bool,
        calls: Mutex<Vec<(String, String)>>,
        screenshot_bytes: Vec<u8>,
        list_json: Option<Value>,
        delay: Option<Duration>,
    }

    impl VmExecutor for MockExecutor {
        fn list(&self) -> Result<Value> {
            self.calls
                .lock()
                .unwrap()
                .push(("list".into(), String::new()));
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            Ok(self
                .list_json
                .clone()
                .unwrap_or_else(|| json!({"vms": []})))
        }

        fn lifecycle(&self, op: VmOp, name: &str) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push((op.as_str().into(), name.into()));
            Ok(())
        }

        fn screenshot(&self, name: &str) -> Result<Vec<u8>> {
            self.calls
                .lock()
                .unwrap()
                .push(("screenshot".into(), name.into()));
            if self.panic_on_screenshot {
                panic!("mock backend panic");
            }
            Ok(self.screenshot_bytes.clone())
        }
    }

    fn command(payload: &str) -> VmCommand {
        VmCommand::parse(payload).unwrap()
    }

    fn request_id() -> &'static str {
        "0b6f9d4e-4a41-4b6e-9d0b-4f8b0f3c2a11"
    }

    #[tokio::test]
    async fn list_returns_vms_json() {
        let executor = Arc::new(MockExecutor {
            list_json: Some(json!({"vms":[{"name":"vm-1","state":"running"}]})),
            ..Default::default()
        });
        let result = execute(
            executor,
            command(&format!(r#"{{"request_id":"{}","op":"list"}}"#, request_id())),
        )
        .await;
        assert!(result.ok, "{:?}", result.error);
        let data = result.data.unwrap();
        assert_eq!(data["vms"][0]["name"], "vm-1");
        assert_eq!(result.op, VmOp::List);
    }

    #[tokio::test]
    async fn lifecycle_returns_name_and_ok() {
        let executor = Arc::new(MockExecutor::default());
        let result = execute(
            executor.clone(),
            command(&format!(
                r#"{{"request_id":"{}","op":"reboot","args":{{"name":"vm-7"}}}}"#,
                request_id()
            )),
        )
        .await;
        assert!(result.ok);
        let data = result.data.unwrap();
        assert_eq!(data["name"], "vm-7");
        assert_eq!(data["result"], "ok");
        let calls = executor.calls.lock().unwrap();
        assert_eq!(calls.as_slice(), [("reboot".to_string(), "vm-7".to_string())]);
    }

    #[tokio::test]
    async fn screenshot_pnm_is_converted_to_png_base64() {
        // 2x2 的 P6 红色图
        let mut pnm = Vec::new();
        pnm.extend_from_slice(b"P6\n2 2\n255\n");
        for _ in 0..4 {
            pnm.extend_from_slice(&[255, 0, 0]);
        }
        let executor = Arc::new(MockExecutor {
            screenshot_bytes: pnm,
            ..Default::default()
        });
        let result = execute(
            executor,
            command(&format!(
                r#"{{"request_id":"{}","op":"screenshot","args":{{"name":"vm-1"}}}}"#,
                request_id()
            )),
        )
        .await;
        assert!(result.ok, "{:?}", result.error);
        let data = result.data.unwrap();
        assert_eq!(data["format"], "png");
        let encoded = data["image_base64"].as_str().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert!(is_png(&bytes));
        assert!(data["size_bytes"].as_u64().unwrap() > 0);

        // 用 png crate 反解，确认尺寸/像素正确
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buffer = vec![0; reader.output_buffer_size().expect("png buffer size")];
        let info = reader.next_frame(&mut buffer).unwrap();
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(&buffer[..3], &[255, 0, 0]);
    }

    #[tokio::test]
    async fn screenshot_png_passthrough_and_size_limit() {
        // 原生 PNG 透传
        let png_bytes = tiny_png();
        let executor = Arc::new(MockExecutor {
            screenshot_bytes: png_bytes.clone(),
            ..Default::default()
        });
        let result = execute(
            executor,
            command(&format!(
                r#"{{"request_id":"{}","op":"screenshot","args":{{"name":"vm-1"}}}}"#,
                request_id()
            )),
        )
        .await;
        assert!(result.ok);
        assert_eq!(
            result.data.unwrap()["image_base64"].as_str().unwrap(),
            base64::engine::general_purpose::STANDARD.encode(&png_bytes)
        );

        // 超过 4 MiB（base64）拒绝
        let mut big = PNG_MAGIC.to_vec();
        big.resize(MAX_SCREENSHOT_BASE64_BYTES, 0);
        let executor = Arc::new(MockExecutor {
            screenshot_bytes: big,
            ..Default::default()
        });
        let result = execute(
            executor,
            command(&format!(
                r#"{{"request_id":"{}","op":"screenshot","args":{{"name":"vm-1"}}}}"#,
                request_id()
            )),
        )
        .await;
        assert!(!result.ok);
        assert!(
            result.error.unwrap().contains("过大"),
            "应因截图过大失败"
        );
    }

    #[tokio::test]
    async fn panic_is_isolated() {
        let executor = Arc::new(MockExecutor {
            panic_on_screenshot: true,
            screenshot_bytes: tiny_png(),
            ..Default::default()
        });
        let result = execute(
            executor,
            command(&format!(
                r#"{{"request_id":"{}","op":"screenshot","args":{{"name":"vm-1"}}}}"#,
                request_id()
            )),
        )
        .await;
        assert!(!result.ok);
        let error = result.error.unwrap();
        assert!(error.contains("panic"), "{error}");
    }

    #[tokio::test]
    async fn timeout_is_reported() {
        let executor = Arc::new(MockExecutor {
            delay: Some(Duration::from_millis(500)),
            ..Default::default()
        });
        let result = execute_with_timeout(
            executor,
            command(&format!(r#"{{"request_id":"{}","op":"list"}}"#, request_id())),
            Duration::from_millis(20),
        )
        .await;
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("超时"));
    }

    /// 1x1 黑色 PNG（手工构造，测试用）。
    fn tiny_png() -> Vec<u8> {
        // 用 png 编码器生成，避免手算 CRC。
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0, 0, 0]).unwrap();
        }
        out
    }
}
