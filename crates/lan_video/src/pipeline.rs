//! GStreamer 管线构造。
//!
//! 低延迟约定：
//! - 无 B 帧、CBR、关键帧间隔 ≤ 2s；
//! - `udpsink sync=false`、`rtpjitterbuffer latency=0`；
//! - `queue` 只留 1 帧且 `leaky=upstream`（丢旧保新），绝不排队；
//! - 采集到编码尽量走 DMA-BUF / D3D11 零拷贝（不要出现 videoconvert/download）。

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Capture {
    /// 合成测试画面（无授权，用于链路验证）。
    Test,
    /// KWin screencast / portal 提供的 PipeWire 节点。
    #[serde(rename = "pipewire")]
    PipeWire { path: Option<u32>, fd: Option<i32> },
    /// X11（Wayland 下只能抓 XWayland 窗口，不推荐）。
    X11 {
        display: Option<String>,
        /// 窗口 XID；None 抓整个根窗口。
        #[serde(default)]
        xid: Option<u64>,
        /// 是否把鼠标指针画进画面（ximagesrc show-pointer）。
        /// 默认 false：viewer 本地画软件光标，避免压缩把光标画出黑框/黑边。
        #[serde(default)]
        show_pointer: bool,
    },
    /// KDE KWin screencast（免授权弹窗；由会话 helper 提供 PipeWire node）。
    /// `window` 为窗口 UUID；None 抓整个输出。
    #[serde(rename = "kwin")]
    KWin {
        pointer: Option<u32>,
        #[serde(default)]
        window: Option<String>,
    },
    /// Windows Graphics Capture。
    Wgc {
        monitor: Option<u32>,
        /// 窗口 HWND；None 抓显示器。
        #[serde(default)]
        window: Option<u64>,
    },
    /// Windows DXGI Desktop Duplication。
    Dxgi,
    /// 直连 VNC 服务器（QEMU/libvirt 的 `<graphics type='vnc'>`）。
    ///
    /// 走 GStreamer 的 `rfbsrc`（gst-plugins-bad），因此无头虚拟机也能采：
    /// 不依赖宿主机有桌面会话，也不需要先把画面渲染到某个窗口里。
    Vnc {
        /// VNC 服务器地址；QEMU 在别的机器上时填那台的 IP。
        host: String,
        /// VNC 端口；QEMU 的规则是 5900 + display 号。
        port: u16,
        /// VNC 密码（对应 XML 里 `graphics passwd='...'`）。
        #[serde(default)]
        password: Option<String>,
    },
}

impl Default for Capture {
    fn default() -> Self {
        Self::Test
    }
}

impl Capture {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::PipeWire { .. } => "pipewire",
            Self::X11 { .. } => "x11",
            Self::KWin { .. } => "kwin",
            Self::Wgc { .. } => "wgc",
            Self::Dxgi => "dxgi",
            Self::Vnc { .. } => "vnc",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Encoder {
    Auto,
    Vah264,
    Vah265,
    Nvh264,
    Nvh265,
    Mfh264,
    X264,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::Auto
    }
}

impl Encoder {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vah264 => "vah264",
            Self::Vah265 => "vah265",
            Self::Nvh264 => "nvh264",
            Self::Nvh265 => "nvh265",
            Self::Mfh264 => "mfh264",
            Self::X264 => "x264",
        }
    }

    fn is_h265(self) -> bool {
        matches!(self, Self::Vah265 | Self::Nvh265)
    }
}

/// 音频通道语义：系统声音 / 麦克风 / 指定设备。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioSource {
    System,
    Mic,
}

impl Default for AudioSource {
    fn default() -> Self {
        Self::System
    }
}

impl AudioSource {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "system" => Ok(Self::System),
            "mic" | "microphone" => Ok(Self::Mic),
            other => bail!("未知 audio_source '{other}'（system/mic）"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineConfig {
    pub capture: Capture,
    pub encoder: Encoder,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub target: String,
    pub port: u16,
    /// 音频 RTP 端口；None 表示不传音频。
    pub audio_port: Option<u16>,
    /// 音频通道：系统声音 / 麦克风。
    pub audio_source: AudioSource,
    /// 具体设备名；设置后优先于 audio_source。
    pub audio_device: Option<String>,
    /// A/V 同步：音视频进同一 gst 进程，用 rtpbin + RTCP SR/NTP 对齐。
    pub sync: bool,
    /// RTP over TCP（rtpstreampay + tcpserversink），适合反向不可达/需要可靠传输。
    pub tcp: bool,
    /// 媒体流 SRTP 密钥（30~46 字节的十六进制串，实际密钥取前 30 字节）。
    /// Some 时媒体走 SRTP 加密+认证；None 时明文 RTP（仅测试/回环）。
    pub media_secret: Option<String>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            capture: Capture::Test,
            encoder: Encoder::Auto,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 12000,
            target: "127.0.0.1".to_string(),
            port: 5000,
            audio_port: None,
            audio_source: AudioSource::System,
            audio_device: None,
            sync: false,
            tcp: false,
            media_secret: None,
        }
    }
}

/// 解析后真正要用的采集方式 / 编码器（Auto 已展开）。
#[derive(Clone, Debug)]
pub struct Resolved {
    pub capture: Capture,
    pub encoder: Encoder,
}

pub fn resolve(cfg: &PipelineConfig) -> Result<Resolved> {
    let capture = resolve_capture(&cfg.capture)?;
    let encoder = resolve_encoder(cfg.encoder)?;
    Ok(Resolved { capture, encoder })
}

fn resolve_capture(capture: &Capture) -> Result<Capture> {
    match capture {
        Capture::Test => Ok(capture.clone()),
        Capture::PipeWire { path, fd } => {
            if !crate::gst_available("pipewiresrc") {
                bail!("缺少 pipewiresrc（安装 gst-plugin-pipewire / pipewire）");
            }
            if path.is_none() && fd.is_none() {
                bail!(
                    "pipewire 采集需要 node path 或 fd（来自 KWin screencast / xdg-desktop-portal）"
                );
            }
            Ok(capture.clone())
        }
        Capture::X11 { .. } => {
            if !crate::gst_available("ximagesrc") {
                bail!("缺少 ximagesrc");
            }
            Ok(capture.clone())
        }
        Capture::KWin { .. } => {
            if !crate::gst_available("pipewiresrc") {
                bail!("缺少 pipewiresrc（安装 gst-plugin-pipewire / pipewire）");
            }
            Ok(capture.clone())
        }
        Capture::Wgc { .. } => {
            if !crate::gst_available("d3d11screencapturesrc") {
                bail!("缺少 d3d11screencapturesrc（仅 Windows）");
            }
            Ok(capture.clone())
        }
        Capture::Dxgi => {
            if !crate::gst_available("dxgiscreencapsrc") {
                bail!("缺少 dxgiscreencapsrc（仅 Windows）");
            }
            Ok(capture.clone())
        }
        Capture::Vnc {
            host,
            port,
            password,
        } => {
            if !crate::gst_available("rfbsrc") {
                bail!("缺少 rfbsrc（gst-plugins-bad，需 libvncclient）");
            }
            validate_vnc_host(host)?;
            if *port == 0 {
                bail!("VNC port 不能为 0");
            }
            if let Some(password) = password {
                validate_vnc_password(password)?;
            }
            Ok(capture.clone())
        }
    }
}

/// 校验 VNC 主机名。
///
/// 管线最终是拼成字符串交给 `gst-launch-1.0` 的，host 里出现空白或 `!`
/// 就能凭空插入额外的 GStreamer 元素，所以这里按字符白名单收口。
fn validate_vnc_host(host: &str) -> Result<()> {
    if host.trim().is_empty() {
        bail!("VNC host 不能为空");
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'))
    {
        bail!("VNC host 含非法字符：{host:?}（只允许字母、数字、. - _ :）");
    }
    Ok(())
}

/// 校验 VNC 密码能否安全塞进 `gst-launch` 的双引号参数。
fn validate_vnc_password(password: &str) -> Result<()> {
    if password.contains('"') || password.contains('\\') {
        bail!("VNC 密码不能包含双引号或反斜杠（gst-launch 参数转义限制）");
    }
    Ok(())
}

fn resolve_encoder(encoder: Encoder) -> Result<Encoder> {
    if encoder != Encoder::Auto {
        let element = encoder_element_name(encoder);
        if !crate::gst_available(element) {
            bail!("编码器 {element} 不可用");
        }
        return Ok(encoder);
    }
    for candidate in [
        Encoder::Vah264,
        Encoder::Nvh264,
        Encoder::Mfh264,
        Encoder::X264,
    ] {
        if crate::gst_available(encoder_element_name(candidate)) {
            return Ok(candidate);
        }
    }
    bail!("没有可用的 H.264 编码器（vah264enc/nvh264enc/mfh264enc/x264enc）")
}

fn encoder_element_name(encoder: Encoder) -> &'static str {
    match encoder {
        Encoder::Vah264 => "vah264enc",
        Encoder::Vah265 => "vah265enc",
        Encoder::Nvh264 => "nvh264enc",
        Encoder::Nvh265 => "nvh265enc",
        Encoder::Mfh264 => "mfh264enc",
        Encoder::X264 => "x264enc",
        Encoder::Auto => "auto",
    }
}

fn video_caps(cfg: &PipelineConfig, encoder: Encoder) -> String {
    let format = if matches!(encoder, Encoder::X264) {
        ",format=I420"
    } else {
        ""
    };
    format!(
        "video/x-raw,width={},height={},framerate={}/1{format}",
        cfg.width, cfg.height, cfg.fps
    )
}

fn scaled_caps(cfg: &PipelineConfig, encoder: Encoder) -> String {
    let format = if matches!(encoder, Encoder::X264) {
        ",format=I420"
    } else {
        ""
    };
    format!(
        "video/x-raw,width={},height={}{format}",
        cfg.width, cfg.height
    )
}

fn key_int(cfg: &PipelineConfig) -> u32 {
    // 1 秒一个关键帧：丢包/运动模糊恢复更快
    cfg.fps.max(1)
}

fn encoder_segment(encoder: Encoder, cfg: &PipelineConfig) -> String {
    let kbps = cfg.bitrate_kbps.max(500);
    match encoder {
        Encoder::Vah264 => format!(
            "vah264enc rate-control=cbr bitrate={kbps} key-int-max={}",
            key_int(cfg)
        ),
        Encoder::Vah265 => format!(
            "vah265enc rate-control=cbr bitrate={kbps} key-int-max={}",
            key_int(cfg)
        ),
        Encoder::Nvh264 => {
            format!("nvh264enc bitrate={kbps} rc-mode=cbr zerolatency=true preset=llhq")
        }
        Encoder::Nvh265 => {
            format!("nvh265enc bitrate={kbps} rc-mode=cbr zerolatency=true preset=llhq")
        }
        Encoder::Mfh264 => "mfh264enc low-latency=true".to_string(),
        Encoder::X264 => format!(
            "x264enc tune=zerolatency speed-preset=superfast bitrate={kbps} key-int-max={}",
            key_int(cfg)
        ),
        Encoder::Auto => unreachable!("Auto 已解析"),
    }
}

/// 视频链（到 RTP payloader，不含 udpsink）。
fn build_video_prefix(cfg: &PipelineConfig) -> Result<String> {
    if cfg.width == 0 || cfg.height == 0 || cfg.fps == 0 {
        bail!("width/height/fps 必须大于 0");
    }
    let resolved = resolve(cfg)?;
    let encoder = resolved.encoder;
    let mut parts: Vec<String> = Vec::new();

    match &resolved.capture {
        Capture::Test => {
            parts.push("videotestsrc is-live=true pattern=ball".into());
            parts.push(video_caps(cfg, encoder));
        }
        Capture::PipeWire { path, fd } => {
            let mut src = "pipewiresrc".to_string();
            if let Some(path) = path {
                src.push_str(&format!(" path={path}"));
            } else if let Some(fd) = fd {
                src.push_str(&format!(" fd={fd}"));
            }
            parts.push(src);
            // pipewiresrc 没有默认 caps，必须显式声明，否则会协商失败。
            parts.push("video/x-raw".into());
            match encoder {
                Encoder::Vah264 | Encoder::Vah265 => {
                    if !crate::gst_available("vapostproc") {
                        bail!("缺少 vapostproc（gst-plugin-va），无法把采集格式转到 NV12");
                    }
                    parts.push("vapostproc".into());
                    parts.push(format!(
                        "video/x-raw,width={},height={},format=NV12",
                        cfg.width, cfg.height
                    ));
                }
                _ => {
                    parts.push("videoconvert".into());
                    parts.push("videoscale".into());
                    parts.push(scaled_caps(cfg, encoder));
                }
            }
        }
        Capture::X11 {
            display,
            xid,
            show_pointer,
        } => {
            let mut src = "ximagesrc use-damage=false".to_string();
            // 光标默认不进画面：viewer 本地画软件光标，避免压缩光标产生黑框/黑边。
            src.push_str(&format!(" show-pointer={}", *show_pointer));
            if let Some(display) = display {
                src.push_str(&format!(" display-name={display}"));
            }
            if let Some(xid) = xid {
                src.push_str(&format!(" xid={xid}"));
            }
            parts.push(src);
            // ximagesrc 不支持直接协商任意尺寸；统一做一次缩放/转换。
            parts.push("video/x-raw".into());
            match encoder {
                Encoder::Vah264 | Encoder::Vah265 => {
                    if !crate::gst_available("vapostproc") {
                        bail!("缺少 vapostproc（gst-plugin-va）");
                    }
                    parts.push("vapostproc".into());
                    parts.push(format!(
                        "video/x-raw,width={},height={},format=NV12",
                        cfg.width, cfg.height
                    ));
                }
                _ => {
                    parts.push("videoconvert".into());
                    parts.push("videoscale".into());
                    parts.push(scaled_caps(cfg, encoder));
                }
            }
        }
        Capture::KWin { .. } => {
            bail!("KWin 采集需要 SessionManager 先启动 kwin-capture helper 获取 PipeWire node");
        }
        Capture::Wgc { monitor, window } => {
            let mut src = "d3d11screencapturesrc show-cursor=true".to_string();
            if let Some(window) = window {
                src.push_str(&format!(
                    " window-handle={window} window-capture-mode=client"
                ));
            } else if let Some(monitor) = monitor {
                src.push_str(&format!(" monitor-index={monitor}"));
            }
            parts.push(src);
            parts.push(video_caps(cfg, encoder));
        }
        Capture::Dxgi => {
            parts.push("dxgiscreencapsrc show-cursor=true".into());
            parts.push(video_caps(cfg, encoder));
        }
        Capture::Vnc {
            host,
            port,
            password,
        } => {
            // rfbsrc 自己就是 VNC 客户端：连上服务器拿画面，不依赖宿主机桌面。
            let mut src = format!("rfbsrc host={host} port={port}");
            if let Some(password) = password {
                src.push_str(&format!(" password=\"{password}\""));
            }
            parts.push(src);
            // rfbsrc 没有默认 caps，必须显式声明。
            parts.push("video/x-raw".into());
            // 实测 rfbsrc 协商出的是 RGBA，且只在画面变化时出帧，
            // 所以这里不带 framerate；add-borders 保住虚拟机屏幕的宽高比，
            // 否则 720x400 的文字会被拉成 1920x1080。
            parts.push("videoconvert".into());
            parts.push("videoscale add-borders=true".into());
            parts.push(format!(
                "video/x-raw,format=I420,width={},height={}",
                cfg.width, cfg.height
            ));
            if matches!(encoder, Encoder::Vah264 | Encoder::Vah265) {
                // VAAPI 编码器吃 NV12；RGBA→I420 已经做过一次转换，
                // 这里只负责把内存搬到 VAAPI 可访问的格式。
                parts.push("vapostproc".into());
                parts.push(format!(
                    "video/x-raw,width={},height={},format=NV12",
                    cfg.width, cfg.height
                ));
            }
        }
    }

    // 只保留 1 帧，绝不排队；leaky=upstream = 丢旧帧、保最新帧。
    // （注意方向：downstream 是丢新保旧，会导致观察端停留在旧画面。）
    parts.push("queue max-size-buffers=1 leaky=upstream".into());
    parts.push(encoder_segment(encoder, cfg));

    let (parser, payloader) = if encoder.is_h265() {
        ("h265parse", "rtph265pay")
    } else {
        ("h264parse", "rtph264pay")
    };
    parts.push(format!("{parser} config-interval=1"));
    parts.push(format!("{payloader} pt=96"));
    Ok(parts.join(" ! "))
}

fn rtcp_ports(video: u16, audio: u16) -> Result<(u16, u16)> {
    if video == 0 || audio == 0 {
        bail!("端口不能为 0");
    }
    if video == audio {
        bail!("音频端口不能与视频端口相同");
    }
    let video_rtcp = video.checked_add(1).context("视频端口 +1 溢出")?;
    let audio_rtcp = audio.checked_add(1).context("音频端口 +1 溢出")?;
    let ports = [video, video_rtcp, audio, audio_rtcp];
    for (i, port) in ports.iter().enumerate() {
        for other in ports.iter().skip(i + 1) {
            if port == other {
                bail!("RTP/RTCP 端口冲突：{port}");
            }
        }
    }
    Ok((video_rtcp, audio_rtcp))
}

fn rtp_sink(cfg: &PipelineConfig, port: u16) -> Result<String> {
    if cfg.tcp {
        for element in ["rtpstreampay", "tcpserversink"] {
            if !crate::gst_available(element) {
                bail!("缺少 {element}（gst-plugins-good）");
            }
        }
        return Ok(format!(
            "rtpstreampay ! tcpserversink host=0.0.0.0 port={port} sync=false"
        ));
    }
    // UDP：可选 SRTP 加密（media_secret 给了才加密）。
    let enc = match cfg.media_secret.as_deref() {
        Some(secret) => format!("{} ! ", srtp_enc_segment(secret)?),
        None => String::new(),
    };
    Ok(format!(
        "{enc}udpsink host={} port={port} sync=false",
        cfg.target
    ))
}

/// SRTP 密钥：把任意 secret 归一成 srtpenc 需要的 30 字节主密钥（十六进制串）。
/// 主密钥(16B) + 主 salt(14B) = 30 字节；我们从 secret 派生（截断/补零，LAN 内
/// 简单实用；严格场景应换成 DTLS 握手协商）。
pub fn srtp_master_key(secret: &str) -> String {
    // 只取十六进制字符，便于命令行/JSON 传递。
    let mut hex: String = secret.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() > 60 {
        hex.truncate(60);
    }
    while hex.len() < 60 {
        hex.push('0');
    }
    hex.to_ascii_lowercase()
}

/// 发送端：RTP → SRTP 加密。返回应插在 payloader 之后、sink 之前的一段。
/// 单流场景下 srtpenc 的 rtp_sink_0/rtp_src_0 需求 pad 直接串接即可。
fn srtp_enc_segment(secret: &str) -> Result<String> {
    if !crate::gst_available("srtpenc") {
        bail!("缺少 srtpenc（gst-plugins-bad，需安装 gstreamer1.0-plugins-bad / libsrtp）");
    }
    let key = srtp_master_key(secret);
    // 默认 aes-128-icm + hmac-sha1-80（libsrtp 最兼容）；GCM 由元素默认决定。
    Ok(format!(
        "srtpenc name=srtpenc key={key} rtp-cipher=aes-128-icm rtp-auth=hmac-sha1-80 rtcp-cipher=aes-128-icm rtcp-auth=hmac-sha1-80"
    ))
}

/// 接收端：SRTP → RTP 解密。返回应插在 udpsrc 之后、jitterbuffer 之前的一段。
pub fn srtp_dec_segment(secret: &str) -> Result<String> {
    if !crate::gst_available("srtpdec") {
        bail!("缺少 srtpdec（gst-plugins-bad，需安装 gstreamer1.0-plugins-bad / libsrtp）");
    }
    let key = srtp_master_key(secret);
    Ok(format!(
        "srtpdec name=srtpdec key={key} rtp-cipher=aes-128-icm rtp-auth=hmac-sha1-80 rtcp-cipher=aes-128-icm rtcp-auth=hmac-sha1-80"
    ))
}

/// 构造发送端管线（单进程，可直接交给 gst-launch-1.0）。
/// `sync=true` 时音视频进同一进程，用 rtpbin + RTCP SR/NTP 做同步。
pub fn build_sender_pipeline(cfg: &PipelineConfig) -> Result<String> {
    if cfg.target.trim().is_empty() {
        bail!("target 不能为空");
    }
    if cfg.port == 0 {
        bail!("port 不能为 0");
    }
    if cfg.sync {
        return build_av_sender_pipeline(cfg);
    }
    let prefix = build_video_prefix(cfg)?;
    Ok(format!("{prefix} ! {}", rtp_sink(cfg, cfg.port)?))
}

pub fn build_av_sender_pipeline(cfg: &PipelineConfig) -> Result<String> {
    if cfg.tcp {
        bail!("--sync 暂不支持 --tcp（RTP/RTCP 同步走 UDP）");
    }
    if !crate::gst_available("rtpbin") {
        bail!("缺少 rtpbin（gst-plugins-good）");
    }
    let audio_port = cfg.audio_port.context("sync 模式需要 audio_port")?;
    let (video_rtcp, audio_rtcp) = rtcp_ports(cfg.port, audio_port)?;
    let video = build_video_prefix(cfg)?;
    let audio = build_audio_prefix(cfg)?;
    Ok(format!(
        "rtpbin name=rtpbin {video} ! rtpbin.send_rtp_sink_0 \
         rtpbin.send_rtp_src_0 ! udpsink host={target} port={port} sync=false \
         rtpbin.send_rtcp_src_0 ! udpsink host={target} port={video_rtcp} sync=false \
         {audio} ! rtpbin.send_rtp_sink_1 \
         rtpbin.send_rtp_src_1 ! udpsink host={target} port={audio_port} sync=false \
         rtpbin.send_rtcp_src_1 ! udpsink host={target} port={audio_rtcp} sync=false",
        target = cfg.target,
        port = cfg.port,
        video_rtcp = video_rtcp,
        audio = audio,
        audio_port = audio_port,
        audio_rtcp = audio_rtcp,
    ))
}

fn video_sink_chain(sink: &str) -> String {
    // waylandsink/xvimagesink 等需要 BGRx；autovideosink/glimagesink/fakesink 可直接吃 NV12。
    if sink.contains("fakesink") || sink.contains("autovideosink") || sink.contains("glimagesink") {
        sink.to_string()
    } else {
        format!("videoconvert ! {sink}")
    }
}

/// 接收端管线（原生 GStreamer 客户端 / 延迟验证）。
///
/// `decoder`：`auto`（decodebin，兼容性最好）、`avdec`、`vah264`、`decodebin`。
/// 注意：`vah264dec` 输出 VAAPI 内存，普通 sink 可能协商失败，需要 VA 友好 sink
/// 或额外转换；测试链路默认用 `auto`。
pub fn build_receiver_pipeline(port: u16, sink: &str, decoder: &str) -> Result<String> {
    build_receiver_pipeline_sec(port, sink, decoder, None)
}

/// 接收端视频管线；`media_secret` 给了则先 SRTP 解密。
pub fn build_receiver_pipeline_sec(
    port: u16,
    sink: &str,
    decoder: &str,
    media_secret: Option<&str>,
) -> Result<String> {
    if port == 0 {
        bail!("port 不能为 0");
    }
    let element = match decoder {
        "auto" | "decodebin" => "decodebin",
        "avdec" | "avdec_h264" => "avdec_h264",
        "vah264" | "vah264dec" => "vah264dec",
        other => bail!("未知 decoder '{other}'（auto/avdec/vah264/decodebin）"),
    };
    if !crate::gst_available(element) {
        bail!("解码器 {element} 不可用");
    }
    let video_sink = video_sink_chain(sink);
    // SRTP：udpsrc 输出 x-srtp，srtpdec 解密后才是 x-rtp。
    let (caps_media, dec) = match media_secret {
        Some(secret) => (
            "application/x-srtp",
            format!("{} ! ", srtp_dec_segment(secret)?),
        ),
        None => (
            "application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000",
            String::new(),
        ),
    };
    Ok(format!(
        "udpsrc port={port} caps=\"{caps_media}\" ! \
         {dec}rtpjitterbuffer latency=0 ! rtph264depay ! {element} ! {video_sink} sync=false"
    ))
}

/// 音频发送管线：系统声音（monitor）→ Opus 10ms → RTP，独立端口。
fn build_audio_prefix(cfg: &PipelineConfig) -> Result<String> {
    #[cfg(target_os = "linux")]
    let audio_element = "pulsesrc";
    #[cfg(not(target_os = "linux"))]
    let audio_element = "wasapisrc";
    for element in [audio_element, "opusenc", "rtpopuspay"] {
        if !crate::gst_available(element) {
            bail!("缺少音频元素 {element}");
        }
    }
    // Linux: PulseAudio/PipeWire；Windows: WASAPI。
    #[cfg(target_os = "linux")]
    let source = {
        let device = cfg
            .audio_device
            .clone()
            .unwrap_or_else(|| match cfg.audio_source {
                AudioSource::System => "@DEFAULT_MONITOR@".to_string(),
                AudioSource::Mic => "@DEFAULT_SOURCE@".to_string(),
            });
        format!("pulsesrc device=\"{device}\"")
    };
    #[cfg(not(target_os = "linux"))]
    let source = match &cfg.audio_device {
        Some(device) => format!("wasapisrc device=\"{device}\""),
        None => match cfg.audio_source {
            AudioSource::System => "wasapisrc loopback=true".to_string(),
            AudioSource::Mic => "wasapisrc".to_string(),
        },
    };
    Ok(format!(
        "{source} ! audio/x-raw,rate=48000,channels=2 \
         ! queue max-size-buffers=2 leaky=upstream \
         ! opusenc frame-size=10 bitrate=128000 ! rtpopuspay pt=97"
    ))
}

pub fn build_audio_pipeline(cfg: &PipelineConfig) -> Result<String> {
    let port = cfg.audio_port.context("audio_port 未配置")?;
    if port == 0 {
        bail!("audio_port 不能为 0");
    }
    if port == cfg.port {
        bail!("audio_port 不能与视频端口相同");
    }
    let prefix = build_audio_prefix(cfg)?;
    Ok(format!("{prefix} ! {}", rtp_sink(cfg, port)?))
}

/// TCP 视频接收管线（客户端连发送端）。
pub fn build_tcp_receiver_pipeline(
    host: &str,
    port: u16,
    sink: &str,
    decoder: &str,
) -> Result<String> {
    if port == 0 {
        bail!("port 不能为 0");
    }
    let element = match decoder {
        "auto" | "decodebin" => "decodebin",
        "avdec" | "avdec_h264" => "avdec_h264",
        "vah264" | "vah264dec" => "vah264dec",
        other => bail!("未知 decoder '{other}'（auto/avdec/vah264/decodebin）"),
    };
    if !crate::gst_available("rtpstreamdepay") {
        bail!("缺少 rtpstreamdepay（gst-plugins-good）");
    }
    let video_sink = video_sink_chain(sink);
    Ok(format!(
        "tcpclientsrc host={host} port={port} ! application/x-rtp-stream ! rtpstreamdepay ! \
         rtpjitterbuffer latency=0 ! rtph264depay ! {element} ! {video_sink} sync=false"
    ))
}

/// TCP 音频接收管线。
pub fn build_tcp_audio_receiver_pipeline(host: &str, port: u16, sink: &str) -> Result<String> {
    if port == 0 {
        bail!("port 不能为 0");
    }
    if !crate::gst_available("rtpstreamdepay") {
        bail!("缺少 rtpstreamdepay（gst-plugins-good）");
    }
    Ok(format!(
        "tcpclientsrc host={host} port={port} ! application/x-rtp-stream ! rtpstreamdepay ! \
         rtpjitterbuffer latency=0 ! rtpopusdepay ! opusdec ! {sink} sync=false"
    ))
}

/// A/V 同步接收端：音视频同一 pipeline，rtpbin ntp-sync + sink sync=true。
pub fn build_av_receiver_pipeline(
    video_port: u16,
    audio_port: u16,
    video_sink: &str,
    audio_sink: &str,
) -> Result<String> {
    if !crate::gst_available("rtpbin") {
        bail!("缺少 rtpbin（gst-plugins-good）");
    }
    for element in ["decodebin", "rtpopusdepay", "opusdec"] {
        if !crate::gst_available(element) {
            bail!("缺少音频/视频元素 {element}");
        }
    }
    let (video_rtcp, audio_rtcp) = rtcp_ports(video_port, audio_port)?;
    let video_chain = video_sink_chain(video_sink);
    Ok(format!(
        "rtpbin name=rtpbin ntp-sync=true \
         udpsrc port={video_port} caps=\"application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000\" ! rtpbin.recv_rtp_sink_0 \
         udpsrc port={video_rtcp} caps=\"application/x-rtcp\" ! rtpbin.recv_rtcp_sink_0 \
         rtpbin.recv_rtp_src_0 ! rtpjitterbuffer latency=0 ! rtph264depay ! decodebin ! {video_chain} sync=true \
         udpsrc port={audio_port} caps=\"application/x-rtp,media=audio,encoding-name=OPUS,payload=97,clock-rate=48000\" ! rtpbin.recv_rtp_sink_1 \
         udpsrc port={audio_rtcp} caps=\"application/x-rtcp\" ! rtpbin.recv_rtcp_sink_1 \
         rtpbin.recv_rtp_src_1 ! rtpjitterbuffer latency=0 ! rtpopusdepay ! opusdec ! {audio_sink} sync=true"
    ))
}

/// 音频接收管线。
pub fn build_audio_receiver_pipeline(port: u16, sink: &str) -> Result<String> {
    build_audio_receiver_pipeline_sec(port, sink, None)
}

/// 音频接收管线；`media_secret` 给了则先 SRTP 解密。
pub fn build_audio_receiver_pipeline_sec(
    port: u16,
    sink: &str,
    media_secret: Option<&str>,
) -> Result<String> {
    if port == 0 {
        bail!("port 不能为 0");
    }
    for element in ["rtpopusdepay", "opusdec"] {
        if !crate::gst_available(element) {
            bail!("缺少音频元素 {element}");
        }
    }
    let dec = match media_secret {
        Some(secret) => format!("{} ! ", srtp_dec_segment(secret)?),
        None => String::new(),
    };
    // SRTP：udpsrc 输出 x-srtp，srtpdec 解密后才是 x-rtp。
    let caps_media = if media_secret.is_some() {
        "application/x-srtp"
    } else {
        "application/x-rtp,media=audio,encoding-name=OPUS,payload=97,clock-rate=48000"
    };
    Ok(format!(
        "udpsrc port={port} caps=\"{caps_media}\" \
         ! {dec}rtpjitterbuffer latency=0 ! rtpopusdepay ! opusdec ! {sink} sync=false"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PipelineConfig {
        PipelineConfig {
            target: "192.168.1.10".into(),
            port: 5000,
            capture: Capture::Test,
            encoder: Encoder::X264,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 4000,
            audio_port: None,
            audio_source: AudioSource::System,
            audio_device: None,
            sync: false,
            tcp: false,
            media_secret: None,
        }
    }

    #[test]
    fn sender_pipeline_has_low_latency_defaults() {
        if !crate::gst_available("x264enc") {
            return;
        }
        let p = build_sender_pipeline(&cfg()).unwrap();
        assert!(p.contains("udpsink host=192.168.1.10 port=5000 sync=false"));
        assert!(p.contains("rtph264pay pt=96"));
        assert!(p.contains("queue max-size-buffers=1 leaky=upstream"));
        assert!(p.contains("tune=zerolatency"));
        assert!(p.contains("videotestsrc is-live=true"));
    }

    #[test]
    fn pipewire_requires_path_or_fd() {
        let mut c = cfg();
        c.capture = Capture::PipeWire {
            path: None,
            fd: None,
        };
        assert!(build_sender_pipeline(&c).is_err());
    }

    #[test]
    fn receiver_uses_zero_jitter_buffer() {
        let p = build_receiver_pipeline(5000, "fakesink", "auto").unwrap();
        assert!(p.contains("rtpjitterbuffer latency=0"));
        assert!(p.contains("rtph264depay"));
        assert!(p.contains("decodebin"));
    }

    #[test]
    fn audio_pipeline_uses_opus_and_separate_port() {
        if !crate::gst_available("opusenc") || !crate::gst_available("rtpopuspay") {
            return;
        }
        let mut c = cfg();
        c.audio_port = Some(5002);
        let p = build_audio_pipeline(&c).unwrap();
        assert!(p.contains("opusenc frame-size=10"));
        assert!(p.contains("rtpopuspay pt=97"));
        assert!(p.contains("port=5002"));
        assert!(p.contains("@DEFAULT_MONITOR@"));

        c.audio_source = AudioSource::Mic;
        let mic = build_audio_pipeline(&c).unwrap();
        assert!(mic.contains("@DEFAULT_SOURCE@"));

        c.audio_port = Some(5000);
        assert!(build_audio_pipeline(&c).is_err(), "不能复用视频端口");
    }

    #[test]
    fn av_sync_pipeline_uses_rtpbin_and_rtcp() {
        if !crate::gst_available("rtpbin") || !crate::gst_available("x264enc") {
            return;
        }
        let mut c = cfg();
        c.sync = true;
        c.audio_port = Some(5002);
        let p = build_sender_pipeline(&c).unwrap();
        assert!(p.contains("rtpbin name=rtpbin"));
        assert!(p.contains("rtpbin.send_rtp_sink_0"));
        assert!(p.contains("rtpbin.send_rtcp_src_0"));
        assert!(p.contains("rtpbin.send_rtp_sink_1"));
        assert!(p.contains("port=5001")); // 视频 RTCP
        assert!(p.contains("port=5002")); // 音频 RTP
        assert!(p.contains("port=5003")); // 音频 RTCP

        let r = build_av_receiver_pipeline(5000, 5002, "fakesink", "fakesink").unwrap();
        assert!(r.contains("ntp-sync=true"));
        assert!(r.contains("recv_rtp_sink_0"));
        assert!(r.contains("recv_rtcp_sink_1"));
        assert!(r.contains("sync=true"));
    }

    #[test]
    fn srtp_encrypts_sender_and_decrypts_receiver() {
        if !crate::gst_available("srtpenc") || !crate::gst_available("srtpdec") {
            return;
        }
        let mut c = cfg();
        c.media_secret = Some("deadbeef".into());
        let p = build_sender_pipeline(&c).unwrap();
        assert!(p.contains("srtpenc"), "发送端应有 srtpenc：{p}");
        assert!(p.contains("rtp-cipher=aes-128-icm"), "{p}");
        assert!(p.contains("key=deadbeef"), "密钥应归一化（无 0x 前缀）：{p}");

        let r = build_receiver_pipeline_sec(5000, "fakesink", "auto", Some("deadbeef")).unwrap();
        assert!(r.contains("srtpdec"), "接收端应有 srtpdec：{r}");
        assert!(
            r.contains("application/x-srtp"),
            "加密时 udpsrc caps 应为 x-srtp：{r}"
        );

        // 明文时不应出现 srtp 元素，caps 仍为 x-rtp。
        let plain = build_receiver_pipeline_sec(5000, "fakesink", "auto", None).unwrap();
        assert!(!plain.contains("srtpdec"));
        assert!(plain.contains("application/x-rtp,media=video"));
    }

    #[test]
    fn srtp_master_key_normalizes_to_30_bytes() {
        // 60 个十六进制字符 = 30 字节。
        assert_eq!(srtp_master_key("deadbeef").len(), 60);
        assert!(srtp_master_key("deadbeef").ends_with(&"0".repeat(52)));
        assert_eq!(srtp_master_key("xyz").len(), 60, "非十六进制字符被过滤");
    }

    #[test]
    fn x11_window_pipeline_has_xid() {
        if !crate::gst_available("ximagesrc") {
            return;
        }
        let mut c = cfg();
        c.capture = Capture::X11 {
            display: None,
            xid: Some(0x1234),
            show_pointer: false,
        };
        let p = build_sender_pipeline(&c).unwrap();
        assert!(p.contains("xid=4660"));
        assert!(
            p.contains("show-pointer=false"),
            "默认不把光标编进画面（viewer 本地画软件光标），避免黑框/黑边"
        );

        if let Capture::X11 { show_pointer, .. } = &mut c.capture {
            *show_pointer = true;
        }
        let p = build_sender_pipeline(&c).unwrap();
        assert!(p.contains("show-pointer=true"));
    }

    #[test]
    fn vnc_pipeline_uses_rfbsrc_without_fixed_framerate() {
        if !crate::gst_available("rfbsrc") {
            return;
        }
        let mut c = cfg();
        c.capture = Capture::Vnc {
            host: "10.0.0.5".into(),
            port: 5901,
            password: None,
        };
        let p = build_sender_pipeline(&c).unwrap();
        assert!(p.contains("rfbsrc host=10.0.0.5 port=5901"), "{p}");
        // VNC 是变化驱动：画面不动就没有新帧，声明固定帧率会协商失败。
        assert!(!p.contains("framerate="), "VNC 源不该固定帧率：{p}");
        assert!(
            p.contains("videoscale add-borders=true"),
            "虚拟机的非 16:9 分辨率要被加边而不是拉伸：{p}"
        );
    }

    #[test]
    fn vnc_pipeline_passes_password_and_rejects_injection() {
        if !crate::gst_available("rfbsrc") {
            return;
        }
        let mut c = cfg();
        c.capture = Capture::Vnc {
            host: "127.0.0.1".into(),
            port: 5900,
            password: Some("s3cret".into()),
        };
        let p = build_sender_pipeline(&c).unwrap();
        assert!(p.contains("password=\"s3cret\""), "{p}");

        // host 会被拼进 gst-launch 命令行：空白和 '!' 能插入额外元素。
        c.capture = Capture::Vnc {
            host: "1.2.3.4 ! filesink location=/tmp/pwn".into(),
            port: 5900,
            password: None,
        };
        assert!(build_sender_pipeline(&c).is_err(), "host 注入应被拒绝");

        c.capture = Capture::Vnc {
            host: "127.0.0.1".into(),
            port: 5900,
            password: Some("bad\"quote".into()),
        };
        assert!(build_sender_pipeline(&c).is_err(), "密码里的引号会被坏管线");
    }
}
