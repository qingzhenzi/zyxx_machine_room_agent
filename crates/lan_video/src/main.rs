use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use lan_video::{
    api::{self, AppState},
    capabilities,
    pipeline::{
        Capture, Encoder, PipelineConfig, build_audio_receiver_pipeline_sec,
        build_av_receiver_pipeline, build_receiver_pipeline_sec, build_sender_pipeline,
        build_tcp_audio_receiver_pipeline, build_tcp_receiver_pipeline,
    },
    session::SessionManager,
};
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "lan-video",
    version,
    about = "局域网低延迟视频传输（GStreamer + RTP/UDP）"
)]
struct Cli {
    /// JSON 输出
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct PipelineArgs {
    /// 目标 IP（接收端）
    #[arg(long)]
    target: String,
    /// RTP 端口
    #[arg(long, default_value_t = 5000)]
    port: u16,
    /// 采集源：test / pipewire / x11 / kwin / wgc / dxgi
    #[arg(long, default_value = "test")]
    capture: String,
    /// PipeWire 节点 id（KWin screencast / portal 提供）
    #[arg(long)]
    pipewire_path: Option<u32>,
    /// PipeWire fd（portal 提供）
    #[arg(long)]
    pipewire_fd: Option<i32>,
    /// KWin 窗口 UUID（capture=kwin 时抓单个窗口；不传则抓整屏）
    #[arg(long)]
    kwin_window: Option<String>,
    /// X11 窗口 XID（capture=x11 时抓单个窗口；支持 0x 前缀）
    #[arg(long)]
    x11_window: Option<String>,
    /// capture=x11 时把鼠标指针画进画面（默认不画，viewer 本地画软件光标）
    #[arg(long)]
    show_pointer: bool,
    /// Windows 窗口 HWND（capture=wgc 时抓单个窗口；支持 0x 前缀）
    #[arg(long)]
    window_handle: Option<String>,
    /// Windows 显示器序号（capture=wgc 整屏时用，0 起）
    #[arg(long)]
    monitor: Option<u32>,
    /// VNC 服务器地址（capture=vnc 时必填；QEMU 在别的机器上就填那台的 IP）
    #[arg(long)]
    vnc_host: Option<String>,
    /// VNC 端口（capture=vnc 时用；默认 5900，QEMU 规则是 5900 + display 号）
    #[arg(long, default_value_t = 5900)]
    vnc_port: u16,
    /// VNC 密码（对应 libvirt XML 里的 graphics passwd）
    #[arg(long)]
    vnc_password: Option<String>,
    /// 编码器：auto / vah264 / vah265 / nvh264 / nvh265 / mfh264 / x264
    #[arg(long, default_value = "auto")]
    encoder: String,
    #[arg(long, default_value_t = 1920)]
    width: u32,
    #[arg(long, default_value_t = 1080)]
    height: u32,
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// 码率 kbps
    #[arg(long, default_value_t = 12000)]
    bitrate: u32,
    /// 音频 RTP 端口（可选，需与视频端口不同）
    #[arg(long)]
    audio_port: Option<u16>,
    /// 音频通道：system（系统声音）/ mic（麦克风）
    #[arg(long, default_value = "system")]
    audio_source: String,
    /// 具体设备名（优先于 audio_source；`lan-video devices` 可列）
    #[arg(long)]
    audio_device: Option<String>,
    /// A/V 同步（rtpbin + RTCP SR/NTP；需要 audio-port）
    #[arg(long)]
    sync: bool,
    /// run 模式下额外监听 127.0.0.1:<port>，接受 STOP/START 管理命令
    #[arg(long)]
    manage_port: Option<u16>,
    /// 用 RTP-over-TCP（rtpstreampay + tcpserversink）代替 UDP
    #[arg(long)]
    tcp: bool,
    /// SRTP 媒体加密密钥（任意十六进制字符串，30 字节内；不给则明文 RTP）
    #[arg(long)]
    media_secret: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 探测 GStreamer / 编码器 / 采集源 / 网卡
    Capabilities,
    /// 列出音频设备（系统声音 / 麦克风）
    Devices,
    /// 列出可采集的窗口（Linux/KDE）
    Windows,
    /// 列出运行中 agent 的会话（会话列表 UI，CLI 版）
    Sessions {
        /// agent 地址，例如 http://127.0.0.1:38080
        #[arg(long, default_value = "http://127.0.0.1:38080")]
        agent: String,
        /// agent Bearer token（--token）
        #[arg(long)]
        token: Option<String>,
    },
    /// 强杀某路会话（多设备/会话管理）
    Kill {
        /// agent 地址
        #[arg(long, default_value = "http://127.0.0.1:38080")]
        agent: String,
        /// 会话 id
        id: String,
        #[arg(long)]
        token: Option<String>,
    },
    /// 只打印 gst-launch 管线，不启动
    Pipeline(PipelineArgs),
    /// 启动控制 API（供管理端调用）
    Serve {
        #[arg(long, default_value = "127.0.0.1:38080")]
        bind: String,
        /// 可选 Bearer token
        #[arg(long)]
        token: Option<String>,
    },
    /// 原生 viewer：GStreamer 解码 + SDL2 窗口 + 控制 WS
    #[cfg(all(target_os = "linux", feature = "viewer"))]
    View(ViewArgs),
    /// 前台起一路会话，Ctrl-C 结束（本地验证用）
    Run(PipelineArgs),
    /// 原生接收端（GStreamer 播放 / fakesink 测试）
    Receive {
        #[arg(long, default_value_t = 5000)]
        port: u16,
        #[arg(long, default_value = "autovideosink")]
        sink: String,
        /// 解码器：auto / avdec / vah264 / decodebin
        #[arg(long, default_value = "auto")]
        decoder: String,
        /// 音频 RTP 端口（可选）
        #[arg(long)]
        audio_port: Option<u16>,
        /// 音频播放 sink
        #[arg(long, default_value = "autoaudiosink")]
        audio_sink: String,
        /// A/V 同步接收（rtpbin + RTCP/NTP；需要 --audio-port）
        #[arg(long)]
        sync: bool,
        /// 用 RTP-over-TCP 客户端（连接发送端）
        #[arg(long)]
        tcp: bool,
        /// TCP 发送端地址（--tcp 时使用）
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// SRTP 媒体解密密钥（与发送端 --media-secret 一致；不给则收明文）
        #[arg(long)]
        media_secret: Option<String>,
    },
    /// UDP 网络探测（RTT/抖动/丢包；不是玻璃到玻璃延迟）
    Probe {
        /// 作为回显服务端
        #[arg(long)]
        listen: bool,
        /// 服务端绑定地址
        #[arg(long, default_value = "0.0.0.0:40000")]
        bind: String,
        /// 客户端目标 ip:port
        #[arg(long)]
        peer: Option<String>,
        #[arg(long, default_value_t = 200)]
        count: u64,
        #[arg(long, default_value_t = 5)]
        interval_ms: u64,
    },
    /// 内部：uinput 输入注入 helper（由控制通道拉起）
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    UinputInject,
    /// 内部：KWin screencast 采集 helper（由 SessionManager 拉起）
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    KwinCapture {
        /// 光标模式：1=隐藏 2=嵌入 4=metadata
        #[arg(long, default_value_t = 2)]
        pointer: u32,
        /// 窗口 UUID（`{uuid}`）；不传抓整屏
        #[arg(long)]
        window: Option<String>,
    },
}

#[cfg(all(target_os = "linux", feature = "viewer"))]
#[derive(Args, Debug)]
struct ViewArgs {
    /// 接收视频的 RTP 端口
    #[arg(long)]
    port: u16,
    /// 接收音频的 RTP 端口（可选）
    #[arg(long)]
    audio_port: Option<u16>,
    /// 视频 jitter buffer 延迟（ms；UDP 用，0=最激进）
    #[arg(long, default_value_t = 15)]
    jitter_ms: u32,
    /// 用 RTP-over-TCP 客户端连接发送端
    #[arg(long)]
    tcp: bool,
    /// TCP 发送端地址（--tcp 时使用）
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// agent 地址，例如 http://127.0.0.1:38080（启用控制/文件需要）
    #[arg(long)]
    agent: Option<String>,
    /// 会话 id（启用控制需要）
    #[arg(long)]
    session: Option<String>,
    /// 会话 control_token（启用控制需要）
    #[arg(long)]
    token: Option<String>,
    /// 启动即全屏
    #[arg(long)]
    fullscreen: bool,
    #[arg(long, default_value = "lan_video viewer")]
    title: String,
    /// SRTP 媒体解密密钥（与发送端 --media-secret 一致；不给则收明文）
    #[arg(long)]
    media_secret: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("lan_video=info,warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Capabilities => {
            let caps = capabilities();
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&caps)?);
            } else {
                println!("platform        : {}", caps.platform);
                println!(
                    "gstreamer       : {}",
                    caps.gst_version.as_deref().unwrap_or("未安装")
                );
                println!("encoders        : {}", caps.encoders.join(", "));
                println!("capture sources : {}", caps.capture_sources.join(", "));
                println!("vaapi / nvenc   : {} / {}", caps.vaapi, caps.nvenc);
                println!("ips             : {}", caps.ips.join(", "));
                println!("recommended     : {}", caps.recommended);
            }
        }
        Cmd::Devices => {
            let devices = lan_video::devices::audio_devices();
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&devices)?);
            } else {
                for device in devices {
                    println!(
                        "{:<6} {:<3} {}",
                        device.kind,
                        if device.default { "*" } else { "" },
                        device.name
                    );
                    println!("       id: {}", device.id);
                }
            }
        }
        Cmd::Windows => {
            #[cfg(target_os = "linux")]
            {
                let kwin = lan_video::kwin::list_windows();
                if let Ok(windows) = &kwin {
                    if !windows.is_empty() {
                        if cli.json {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(
                                    &serde_json::json!({"backend": "kwin", "windows": windows})
                                )?
                            );
                        } else {
                            for window in windows {
                                println!("[kwin] {}", window.title);
                                println!("  uuid: {}", window.uuid);
                                println!("  app : {}", window.app);
                            }
                        }
                        return Ok(());
                    }
                }
                let windows = lan_video::x11::list_windows();
                if cli.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(
                            &serde_json::json!({"backend": "x11", "windows": windows})
                        )?
                    );
                } else {
                    for window in windows {
                        println!("[x11] {}", window.title);
                        println!("  xid: {} (0x{:x})", window.id, window.id);
                        println!("  app: {}", window.app);
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                match lan_video::windows::list_windows() {
                    Ok(windows) => {
                        if cli.json {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(
                                    &serde_json::json!({"backend": "win32", "windows": windows})
                                )?
                            );
                        } else {
                            for window in windows {
                                println!("[win32] {}", window.title);
                                println!("  hwnd: {} (0x{:x})", window.hwnd, window.hwnd);
                                println!("  app : {}", window.app);
                            }
                        }
                    }
                    Err(err) => bail!(
                        "窗口枚举失败：{err:#}\n备选：PowerShell Get-Process 取 MainWindowHandle 后传 --window-handle"
                    ),
                }
            }
        }
        Cmd::Pipeline(args) => {
            let cfg = to_config(&args)?;
            println!("{}", build_sender_pipeline(&cfg)?);
        }
        Cmd::Sessions { agent, token } => {
            list_sessions_cli(&agent, token.as_deref())?;
        }
        Cmd::Kill { agent, id, token } => {
            kill_session_cli(&agent, &id, token.as_deref())?;
        }
        Cmd::Serve { bind, token } => {
            let manager = Arc::new(SessionManager::new()?);
            let state = AppState {
                manager: manager.clone(),
                token,
            };
            api::serve(state, &bind).await?;
            manager.kill_all();
        }
        #[cfg(all(target_os = "linux", feature = "viewer"))]
        Cmd::View(args) => {
            lan_video::viewer::run(lan_video::viewer::ViewOptions {
                port: args.port,
                jitter_ms: args.jitter_ms,
                audio_port: args.audio_port,
                tcp: args.tcp,
                host: args.host,
                agent: args.agent,
                session: args.session,
                token: args.token,
                fullscreen: args.fullscreen,
                title: args.title,
                media_secret: args.media_secret,
            })?;
        }
        Cmd::Run(args) => {
            let cfg = to_config(&args)?;
            let manager = Arc::new(SessionManager::new()?);
            let info = manager.create(&cfg)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                println!("session  : {}", info.id);
                println!("pid      : {}", info.pid);
                println!("rtp      : {}:{}", info.target, info.rtp_port);
                if let Some(audio_port) = info.audio_port {
                    println!("audio    : {}:{}", info.target, audio_port);
                }
                println!("pipeline : {}", info.pipeline);
                println!("log      : {}", info.log_path);
                println!("Ctrl-C 结束…");
            }
            if let Some(port) = args.manage_port {
                let manager = manager.clone();
                let cfg = cfg.clone();
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
                        Ok(listener) => listener,
                        Err(err) => {
                            eprintln!("manage listener 绑定 127.0.0.1:{port} 失败: {err}");
                            return;
                        }
                    };
                    for stream in listener.incoming().flatten() {
                        let mut reader = std::io::BufReader::new(stream);
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_err() {
                            continue;
                        }
                        match line.trim() {
                            "STOP" => manager.kill_all(),
                            "START" => {
                                manager.kill_all();
                                if let Err(err) = manager.create(&cfg) {
                                    eprintln!("manage START 失败: {err:#}");
                                }
                            }
                            _ => {}
                        }
                    }
                });
            }
            tokio::signal::ctrl_c().await?;
            manager.kill_all();
        }
        Cmd::Receive {
            port,
            sink,
            decoder,
            audio_port,
            audio_sink,
            sync,
            tcp,
            host,
            media_secret,
        } => {
            let gst = lan_video::which("gst-launch-1.0")
                .ok_or_else(|| anyhow::anyhow!("未找到 gst-launch-1.0"))?;
            if tcp && sync {
                bail!("--tcp 暂不支持 --sync");
            }
            let ms = media_secret.as_deref();
            if sync {
                let audio_port =
                    audio_port.ok_or_else(|| anyhow::anyhow!("--sync 需要同时给 --audio-port"))?;
                let pipeline = build_av_receiver_pipeline(port, audio_port, &sink, &audio_sink)?;
                println!("{pipeline}");
                let status = std::process::Command::new(&gst)
                    .args(["-e", "-q"])
                    .args(lan_video::tokenize_pipeline(&pipeline))
                    .status()?;
                if !status.success() {
                    bail!("同步接收端退出：{status}");
                }
            } else if tcp {
                let pipeline = build_tcp_receiver_pipeline(&host, port, &sink, &decoder)?;
                println!("{pipeline}");
                let mut audio_child = match audio_port {
                    Some(audio_port) => {
                        let audio_pipeline =
                            build_tcp_audio_receiver_pipeline(&host, audio_port, &audio_sink)?;
                        println!("{audio_pipeline}");
                        Some(
                            std::process::Command::new(&gst)
                                .args(["-e", "-q"])
                                .args(lan_video::tokenize_pipeline(&audio_pipeline))
                                .spawn()?,
                        )
                    }
                    None => None,
                };
                let status = std::process::Command::new(&gst)
                    .args(["-e", "-q"])
                    .args(lan_video::tokenize_pipeline(&pipeline))
                    .status()?;
                if let Some(mut child) = audio_child.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                if !status.success() {
                    bail!("TCP 接收端退出：{status}");
                }
            } else {
                // 加密接收需要进程内 gstreamer 连 srtpdec request-key（gst-launch 做不到）。
                if ms.is_some() && !tcp {
                    #[cfg(all(target_os = "linux", feature = "viewer"))]
                    {
                        let pipeline = build_receiver_pipeline_sec(port, &sink, &decoder, ms)?;
                        let audio_pipeline =
                            audio_port.map(|p| build_audio_receiver_pipeline_sec(p, &audio_sink, ms)).transpose()?;
                        println!("{pipeline}");
                        return lan_video::viewer::run_sec_receiver(
                            &pipeline,
                            ms.unwrap(),
                            audio_pipeline.as_deref(),
                        );
                    }
                    #[cfg(not(all(target_os = "linux", feature = "viewer")))]
                    {
                        bail!("--media-secret 需要 --features viewer 构建（进程内 gstreamer 才能解 SRTP）");
                    }
                }
                let pipeline = build_receiver_pipeline_sec(port, &sink, &decoder, ms)?;
                println!("{pipeline}");
                let mut audio_child = match audio_port {
                    Some(audio_port) => {
                        let audio_pipeline =
                            build_audio_receiver_pipeline_sec(audio_port, &audio_sink, ms)?;
                        println!("{audio_pipeline}");
                        Some(
                            std::process::Command::new(&gst)
                                .args(["-e", "-q"])
                                .args(lan_video::tokenize_pipeline(&audio_pipeline))
                                .spawn()?,
                        )
                    }
                    None => None,
                };
                let status = std::process::Command::new(&gst)
                    .args(["-e", "-q"])
                    .args(lan_video::tokenize_pipeline(&pipeline))
                    .status()?;
                if let Some(mut child) = audio_child.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                if !status.success() {
                    bail!("接收端退出：{status}");
                }
            }
        }
        Cmd::Probe {
            listen,
            bind,
            peer,
            count,
            interval_ms,
        } => {
            if listen {
                lan_video::probe::listen(&bind).await?;
            } else {
                let peer =
                    peer.ok_or_else(|| anyhow::anyhow!("客户端模式需要 --peer <ip:port>"))?;
                let stats = lan_video::probe::measure(&peer, count, interval_ms).await?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&stats)?);
                } else {
                    println!(
                        "sent={} received={} lost={} ({:.1}%)",
                        stats.sent, stats.received, stats.lost, stats.loss_pct
                    );
                    println!(
                        "rtt ms: min={:.3} mean={:.3} p50={:.3} p95={:.3} max={:.3} jitter={:.3}",
                        stats.min_ms,
                        stats.mean_ms,
                        stats.p50_ms,
                        stats.p95_ms,
                        stats.max_ms,
                        stats.jitter_ms
                    );
                }
            }
        }
        #[cfg(target_os = "linux")]
        Cmd::UinputInject => {
            std::process::exit(lan_video::input::run_uinput());
        }
        #[cfg(target_os = "linux")]
        Cmd::KwinCapture { pointer, window } => {
            lan_video::kwin::ensure_permission_file()?;
            std::process::exit(lan_video::kwin::run(pointer, window.as_deref()));
        }
    }

    Ok(())
}

fn parse_window_id(value: Option<&str>) -> Result<Option<u64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    let parsed = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16)
    } else {
        value.parse::<u64>()
    };
    parsed
        .map(Some)
        .map_err(|err| anyhow::anyhow!("无效窗口 id '{value}': {err}"))
}

fn to_config(args: &PipelineArgs) -> Result<PipelineConfig> {
    let capture = match args.capture.as_str() {
        "test" => Capture::Test,
        "pipewire" => Capture::PipeWire {
            path: args.pipewire_path,
            fd: args.pipewire_fd,
        },
        "x11" => Capture::X11 {
            display: None,
            xid: parse_window_id(args.x11_window.as_deref())?,
            // 默认不把光标编进画面：viewer 本地画软件光标，避免黑框/黑边。
            show_pointer: args.show_pointer,
        },
        "kwin" => {
            #[cfg(target_os = "linux")]
            {
                Capture::KWin {
                    pointer: None,
                    window: args.kwin_window.clone(),
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                bail!("KWin 采集仅支持 Linux")
            }
        }
        "wgc" => Capture::Wgc {
            monitor: args.monitor,
            window: parse_window_id(args.window_handle.as_deref())?,
        },
        "dxgi" => Capture::Dxgi,
        "vnc" => Capture::Vnc {
            host: args
                .vnc_host
                .clone()
                .context("capture=vnc 需要 --vnc-host（虚拟机所在机器的 IP）")?,
            port: args.vnc_port,
            password: args.vnc_password.clone(),
        },
        other => bail!("未知 capture '{other}'（test/pipewire/x11/kwin/wgc/dxgi/vnc）"),
    };
    let encoder = match args.encoder.as_str() {
        "auto" => Encoder::Auto,
        "vah264" => Encoder::Vah264,
        "vah265" => Encoder::Vah265,
        "nvh264" => Encoder::Nvh264,
        "nvh265" => Encoder::Nvh265,
        "mfh264" => Encoder::Mfh264,
        "x264" => Encoder::X264,
        other => bail!("未知 encoder '{other}'"),
    };
    Ok(PipelineConfig {
        capture,
        encoder,
        width: args.width,
        height: args.height,
        fps: args.fps,
        bitrate_kbps: args.bitrate,
        target: args.target.clone(),
        port: args.port,
        audio_port: args.audio_port,
        audio_source: lan_video::pipeline::AudioSource::parse(&args.audio_source)?,
        audio_device: args.audio_device.clone(),
        sync: args.sync,
        tcp: args.tcp,
        media_secret: args.media_secret.clone(),
    })
}

/// 用 curl 发 HTTP 请求（避免引入重型 HTTP client；与 viewer 上传一致）。
fn curl_json(method: &str, url: &str, token: Option<&str>) -> Result<String> {
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-sS", "-X", method, url]);
    if let Some(token) = token {
        cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
    }
    let out = cmd.output().context("执行 curl 失败")?;
    if !out.status.success() {
        bail!("curl 返回 {}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn list_sessions_cli(agent: &str, token: Option<&str>) -> Result<()> {
    let url = format!("{}/v1/sessions", agent.trim_end_matches('/'));
    let body = curl_json("GET", &url, token)?;
    let val: serde_json::Value = serde_json::from_str(&body).context("响应不是 JSON")?;
    let sessions = val.get("sessions").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    if sessions.is_empty() {
        println!("（无会话）");
        return Ok(());
    }
    println!("{:<22} {:<8} {:<10} {:<6} {:<8} {}", "ID", "STATE", "CAPTURE", "PORT", "TCP", "TARGET");
    for s in &sessions {
        println!(
            "{:<22} {:<8} {:<10} {:<6} {:<8} {}",
            s.get("id").and_then(|v| v.as_str()).unwrap_or("-"),
            s.get("state").and_then(|v| v.as_str()).unwrap_or("-"),
            s.get("capture").and_then(|v| v.as_str()).unwrap_or("-"),
            s.get("rtp_port").and_then(|v| v.as_u64()).unwrap_or(0),
            s.get("tcp").and_then(|v| v.as_bool()).unwrap_or(false),
            s.get("target").and_then(|v| v.as_str()).unwrap_or("-"),
        );
    }
    Ok(())
}

fn kill_session_cli(agent: &str, id: &str, token: Option<&str>) -> Result<()> {
    let url = format!("{}/v1/sessions/{}", agent.trim_end_matches('/'), id);
    let body = curl_json("DELETE", &url, token)?;
    println!("{body}");
    Ok(())
}
