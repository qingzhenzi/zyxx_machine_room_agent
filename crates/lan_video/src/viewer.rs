//! 极简原生 viewer：GStreamer 解码 + SDL2 窗口/键鼠捕获 + 控制 WS + 拖拽上传。
//!
//! 只做"看+控"的最小闭环，方便验证协议；复杂 UI 可以后续换 Tauri/Vue。

use anyhow::{Context, Result, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use sdl2::keyboard::Keycode;
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::video::FullscreenType;
use std::ffi::CStr;
use std::path::Path;
use std::sync::mpsc::{self, Sender, SyncSender};
use std::time::Duration;

pub struct ViewOptions {
    pub port: u16,
    pub jitter_ms: u32,
    pub audio_port: Option<u16>,
    pub tcp: bool,
    pub host: String,
    pub agent: Option<String>,
    pub session: Option<String>,
    pub token: Option<String>,
    pub fullscreen: bool,
    pub title: String,
    /// SRTP 媒体解密密钥；Some 时接收端先 srtpdec。
    pub media_secret: Option<String>,
}

struct Frame {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

/// RTP 接收质量（来自 rtpjitterbuffer stats）；用来看丢包/迟到，指导 FEC/重传决策。
#[derive(Clone, Copy, Debug, Default)]
struct RtpStats {
    pushed: u64,
    lost: u64,
    late: u64,
}

pub fn run(opts: ViewOptions) -> Result<()> {
    // 尽量让 SDL 走合成器/垂直同步；X11 下也避免绕过 compositor。
    sdl2::hint::set("SDL_RENDER_VSYNC", "1");
    sdl2::hint::set("SDL_HINT_VIDEO_X11_NET_WM_BYPASS_COMPOSITOR", "0");
    gst::init().context("GStreamer 初始化失败")?;
    let (frame_tx, frame_rx) = mpsc::sync_channel::<Frame>(2);
    let (stats_tx, stats_rx) = mpsc::sync_channel::<RtpStats>(4);
    spawn_video(
        opts.port,
        opts.tcp,
        &opts.host,
        opts.jitter_ms,
        opts.media_secret.clone(),
        frame_tx,
        stats_tx,
    )?;
    let _audio = match spawn_audio(
        opts.audio_port,
        opts.tcp,
        &opts.host,
        opts.media_secret.clone(),
    ) {
        Ok(audio) => audio,
        Err(err) => {
            eprintln!("audio disabled: {err:#}");
            None
        }
    };

    let control = match (&opts.agent, &opts.session, &opts.token) {
        (Some(agent), Some(session), Some(token)) => Some(spawn_control(agent, session, token)?),
        _ => None,
    };

    let sdl = sdl2::init().map_err(|e| anyhow::anyhow!("SDL 初始化失败: {e}"))?;
    let video = sdl
        .video()
        .map_err(|e| anyhow::anyhow!("SDL video 初始化失败: {e}"))?;
    let window = video
        .window(&opts.title, 1280, 720)
        .resizable()
        .position_centered()
        .build()
        .map_err(|e| anyhow::anyhow!("创建窗口失败: {e}"))?;
    let mut canvas = window
        .into_canvas()
        .present_vsync()
        .build()
        .map_err(|e| anyhow::anyhow!("创建 canvas 失败: {e}"))?;
    let texture_creator = canvas.texture_creator();
    let mut texture = None;
    let mouse = sdl.mouse();
    let clipboard = video.clipboard();

    let mut fullscreen = opts.fullscreen;
    if fullscreen {
        let _ = canvas.window_mut().set_fullscreen(FullscreenType::Desktop);
    }
    let mut capture_mouse = control.is_some();
    if capture_mouse {
        let _ = mouse.set_relative_mouse_mode(true);
    }
    let mut last_status = String::new();
    let mut rtp_stats = RtpStats::default();
    // 软件光标位置：相对模式下 SDL 隐藏系统光标，我们在视频内容区自己画一个。
    let mut cursor_x = 0i32;
    let mut cursor_y = 0i32;
    let mut cursor_visible = false;
    let mut buttons_down = 0u8;

    'outer: loop {
        let mut raw: sdl2::sys::SDL_Event = unsafe { std::mem::zeroed() };
        while unsafe { sdl2::sys::SDL_PollEvent(&mut raw) } == 1 {
            let ty = unsafe { raw.type_ };
            if ty == sdl2::sys::SDL_EventType::SDL_QUIT as u32 {
                break 'outer;
            } else if ty == sdl2::sys::SDL_EventType::SDL_KEYDOWN as u32 {
                let key = unsafe { raw.key };
                if key.repeat != 0 {
                    continue;
                }
                let sym = key.keysym.sym as i32;
                if sym == sdl2::sys::SDL_KeyCode::SDLK_F12 as i32 {
                    break 'outer;
                }
                if sym == sdl2::sys::SDL_KeyCode::SDLK_F11 as i32 {
                    fullscreen = !fullscreen;
                    let _ = canvas.window_mut().set_fullscreen(if fullscreen {
                        FullscreenType::Desktop
                    } else {
                        FullscreenType::Off
                    });
                    continue;
                }
                if sym == sdl2::sys::SDL_KeyCode::SDLK_F10 as i32 {
                    capture_mouse = !capture_mouse;
                    let _ = mouse.set_relative_mouse_mode(capture_mouse);
                    continue;
                }
                let ctrl = unsafe { raw.key.keysym.mod_ } & 0x00c0 != 0;
                if ctrl && sym == sdl2::sys::SDL_KeyCode::SDLK_v as i32 {
                    if let Ok(text) = clipboard.clipboard_text() {
                        if !text.is_empty() {
                            let mut payload = Vec::with_capacity(text.len() + 1);
                            payload.push(5u8);
                            payload.extend_from_slice(text.as_bytes());
                            send(&control, payload);
                            set_status(&mut last_status, "clipboard sent");
                        }
                    }
                }
                if let Some(code) = Keycode::from_i32(sym).and_then(evdev_key) {
                    send(&control, key_event(code, true));
                }
            } else if ty == sdl2::sys::SDL_EventType::SDL_KEYUP as u32 {
                let key = unsafe { raw.key };
                let sym = key.keysym.sym as i32;
                if let Some(code) = Keycode::from_i32(sym).and_then(evdev_key) {
                    send(&control, key_event(code, false));
                }
            } else if ty == sdl2::sys::SDL_EventType::SDL_MOUSEMOTION as u32 {
                let motion = unsafe { raw.motion };
                if capture_mouse && (motion.xrel != 0 || motion.yrel != 0) {
                    send(&control, mouse_move(motion.xrel, motion.yrel));
                    // 软件光标：相对模式下同步推进，稍后夹到视频内容区内。
                    cursor_x += motion.xrel;
                    cursor_y += motion.yrel;
                    cursor_visible = true;
                }
            } else if ty == sdl2::sys::SDL_EventType::SDL_MOUSEBUTTONDOWN as u32
                || ty == sdl2::sys::SDL_EventType::SDL_MOUSEBUTTONUP as u32
            {
                let button = unsafe { raw.button };
                let down = ty == sdl2::sys::SDL_EventType::SDL_MOUSEBUTTONDOWN as u32;
                if let Some(code) = match button.button as u32 {
                    1 => Some(0x110),
                    2 => Some(0x112),
                    3 => Some(0x111),
                    _ => None,
                } {
                    send(&control, button_event(code, down));
                    let bit = match button.button as u32 {
                        1 => 1u8,
                        2 => 4,
                        3 => 2,
                        _ => 0,
                    };
                    if down {
                        buttons_down |= bit;
                    } else {
                        buttons_down &= !bit;
                    }
                    cursor_visible = true;
                }
            } else if ty == sdl2::sys::SDL_EventType::SDL_MOUSEWHEEL as u32 {
                let wheel = unsafe { raw.wheel };
                if capture_mouse && wheel.y != 0 {
                    send(
                        &control,
                        wheel_event(wheel.y.clamp(i16::MIN as i32, i16::MAX as i32) as i16),
                    );
                }
            } else if ty == sdl2::sys::SDL_EventType::SDL_DROPFILE as u32 {
                let drop = unsafe { raw.drop };
                if !drop.file.is_null() {
                    let filename = unsafe { CStr::from_ptr(drop.file) }
                        .to_string_lossy()
                        .into_owned();
                    unsafe { sdl2::sys::SDL_free(drop.file as *mut _) };
                    if let (Some(agent), Some(session), Some(token)) =
                        (&opts.agent, &opts.session, &opts.token)
                    {
                        let agent = agent.clone();
                        let session = session.clone();
                        let token = token.clone();
                        std::thread::spawn(move || {
                            let path = Path::new(&filename);
                            match upload_file(&agent, &session, &token, path) {
                                Ok(()) => eprintln!("uploaded: {filename}"),
                                Err(err) => eprintln!("upload failed: {err:#}"),
                            }
                        });
                        set_status(&mut last_status, "uploading dropped file…");
                    }
                }
            }
        }

        while let Ok(frame) = frame_rx.try_recv() {
            let (w, h) = (frame.width, frame.height);
            if texture.is_none() {
                texture = Some(
                    texture_creator
                        .create_texture_streaming(PixelFormatEnum::RGB24, w, h)
                        .map_err(|e| anyhow::anyhow!("创建纹理失败: {e}"))?,
                );
            }
            if let Some(tex) = texture.as_mut() {
                tex.update(None, &frame.data, (w * 3) as usize)
                    .map_err(|e| anyhow::anyhow!("更新纹理失败: {e}"))?;
            }
        }

        canvas.clear();
        if let Some(tex) = texture.as_ref() {
            let query = tex.query();
            let (tw, th) = (query.width, query.height);
            let (ow, oh) = canvas.output_size().map_err(|e| anyhow::anyhow!("{e}"))?;
            let scale = f32::min(ow as f32 / tw as f32, oh as f32 / th as f32);
            let dw = (tw as f32 * scale) as u32;
            let dh = (th as f32 * scale) as u32;
            let dst = Rect::new(
                ((ow.saturating_sub(dw)) / 2) as i32,
                ((oh.saturating_sub(dh)) / 2) as i32,
                dw,
                dh,
            );
            canvas
                .copy(tex, None, Some(dst))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            // 软件光标：只在鼠标捕获时画，位置夹在视频内容区内（避免黑框/黑边）。
            if capture_mouse && cursor_visible {
                let vx = dst.x();
                let vy = dst.y();
                let vw = dst.width() as i32;
                let vh = dst.height() as i32;
                cursor_x = cursor_x.clamp(0, vw.saturating_sub(1));
                cursor_y = cursor_y.clamp(0, vh.saturating_sub(1));
                draw_soft_cursor(&mut canvas, vx + cursor_x, vy + cursor_y, buttons_down != 0)?;
            }
        }
        canvas.present();
        // 合并 RTP 丢包/迟到统计到标题（供 FEC/重传决策；丢包大才考 FEC）。
        while let Ok(st) = stats_rx.try_recv() {
            rtp_stats = st;
        }
        let loss_pct = if rtp_stats.pushed + rtp_stats.lost > 0 {
            rtp_stats.lost as f64 * 100.0 / (rtp_stats.pushed + rtp_stats.lost) as f64
        } else {
            0.0
        };
        let stats_str = if rtp_stats.pushed + rtp_stats.lost + rtp_stats.late > 0 {
            format!(
                "[rtp lost={:.2}% late={}]",
                loss_pct, rtp_stats.late
            )
        } else {
            String::new()
        };
        let mut title = opts.title.clone();
        if !last_status.is_empty() {
            title.push_str(&format!(" — {last_status}"));
        }
        if !stats_str.is_empty() {
            title.push_str(&format!(" {stats_str}"));
        }
        let _ = canvas.window_mut().set_title(&title);
        std::thread::sleep(Duration::from_millis(1));
    }

    mouse.set_relative_mouse_mode(false);
    Ok(())
}

fn spawn_video(
    port: u16,
    tcp: bool,
    host: &str,
    jitter_ms: u32,
    media_secret: Option<String>,
    tx: SyncSender<Frame>,
    stats_tx: SyncSender<RtpStats>,
) -> Result<()> {
    let host = host.to_string();
    std::thread::spawn(move || {
        let decoder = if gst::ElementFactory::find("avdec_h264").is_some() {
            "avdec_h264"
        } else {
            "decodebin"
        };
        // SRTP：udpsrc 输出 x-srtp，srtpdec 解密后才是 x-rtp。
        let (caps_media, dec) = match media_secret.as_deref() {
            Some(secret) => (
                "application/x-srtp",
                format!("{} ! ", crate::pipeline::srtp_dec_segment(secret).unwrap_or_default()),
            ),
            None => (
                "application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000",
                String::new(),
            ),
        };
        let pipeline_str = if tcp {
            format!(
                "tcpclientsrc host={host} port={port} ! application/x-rtp-stream ! rtpstreamdepay ! \
                 rtpjitterbuffer name=jitter latency={jitter_ms} ! rtph264depay ! h264parse ! {decoder} ! videoconvert ! \
                 video/x-raw,format=RGB ! appsink name=videosink sync=false max-buffers=1 drop=true"
            )
        } else {
            format!(
                "udpsrc port={port} caps=\"{caps_media}\" ! \
                 {dec}rtpjitterbuffer name=jitter latency={jitter_ms} ! rtph264depay ! h264parse ! {decoder} ! videoconvert ! \
                 video/x-raw,format=RGB ! appsink name=videosink sync=false max-buffers=1 drop=true"
            )
        };
        let result: Result<()> = (|| {
            let pipeline = gst::parse::launch(&pipeline_str)?
                .downcast::<gst::Pipeline>()
                .map_err(|_| anyhow::anyhow!("pipeline downcast 失败"))?;
            // SRTP：给 srtpdec 接 request-key，才能用 media_secret 解密。
            if let Some(secret) = media_secret.as_deref() {
                if let Some(dec) = pipeline.by_name("srtpdec") {
                    wire_srtpdec_key(&dec, secret)?;
                }
            }
            let sink = pipeline
                .by_name("videosink")
                .context("缺少 videosink")?
                .downcast::<AppSink>()
                .map_err(|_| anyhow::anyhow!("videosink 不是 appsink"))?;
            let jitter = pipeline.by_name("jitter");
            pipeline.set_state(gst::State::Playing)?;
            let mut tick: u32 = 0;
            loop {
                // 每 ~2s 上报一次 rtpjitterbuffer 丢包/迟到统计，供 FEC/重传决策。
                tick = tick.wrapping_add(1);
                if tick % 40 == 0 {
                    if let Some(jb) = jitter.as_ref() {
                        if let Ok(stats) = jb.property_value("stats").get::<gst::Structure>() {
                            let pushed = stats.get::<u64>("num-pushed").unwrap_or(0);
                            let lost = stats.get::<u64>("num-lost").unwrap_or(0);
                            let late = stats.get::<u64>("num-late").unwrap_or(0);
                            let _ = stats_tx.try_send(RtpStats { pushed, lost, late });
                        }
                    }
                }
                match sink.try_pull_sample(gst::ClockTime::from_mseconds(50)) {
                    Some(sample) => {
                        let Some(buffer) = sample.buffer() else {
                            continue;
                        };
                        let Some(caps) = sample.caps() else { continue };
                        let Some(structure) = caps.structure(0) else {
                            continue;
                        };
                        let Ok(width) = structure.get::<i32>("width") else {
                            continue;
                        };
                        let Ok(height) = structure.get::<i32>("height") else {
                            continue;
                        };
                        let Ok(map) = buffer.map_readable() else {
                            continue;
                        };
                        let data = map.as_slice().to_vec();
                        let expected = width as usize * height as usize * 3;
                        if data.len() < expected {
                            continue;
                        }
                        let _ = tx.try_send(Frame {
                            width: width as u32,
                            height: height as u32,
                            data: data[..expected].to_vec(),
                        });
                    }
                    None => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        })();
        if let Err(err) = result {
            eprintln!("video pipeline: {err:#}");
        }
    });
    Ok(())
}

/// 给 `srtpdec` 接上 `request-key` 信号，返回带密钥的 caps。
///
/// srtpdec 没有 key 属性，只能靠 request-key 信号拿到密钥；handler 必须
/// 返回一个 `application/x-srtp` caps，里面带 `srtp-key`（GstBuffer）以及
/// cipher/auth 字段（与 srtpenc 输出一致），否则解不出明文。
fn wire_srtpdec_key(dec: &gst::Element, secret: &str) -> Result<()> {
    use gst::glib::signal::SignalHandlerId;
    use gst::glib::translate::IntoGlibPtr;
    use gst::prelude::*;
    use std::ffi::c_void;

    let key_bytes = {
        let hex = crate::pipeline::srtp_master_key(secret);
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0))
            .collect::<Vec<u8>>()
    };
    let key_buf = gst::Buffer::from_slice(key_bytes);

    // handler 签名：(GstElement*, guint ssrc, gpointer) -> GstCaps*
    unsafe extern "C" fn trampoline(
        _dec: *mut gst::glib::gobject_ffi::GObject,
        _ssrc: u32,
        user_data: *mut c_void,
    ) -> *mut gst::ffi::GstCaps {
        let key_buf = unsafe { &*(user_data as *const gst::Buffer) };
        let caps = gst::Caps::builder("application/x-srtp")
            .field("srtp-key", key_buf)
            .field("srtp-cipher", "aes-128-icm")
            .field("srtp-auth", "hmac-sha1-80")
            .field("srtcp-cipher", "aes-128-icm")
            .field("srtcp-auth", "hmac-sha1-80")
            .build();
        // signal 返回值的所有权交给 C（GValue 会 unref）。
        caps.into_glib_ptr()
    }

    let boxed = Box::new(key_buf);
    let handler: SignalHandlerId = unsafe {
        gst::glib::signal::connect_raw(
            dec.as_ptr() as *mut gst::glib::gobject_ffi::GObject,
            b"request-key\0".as_ptr() as *const _,
            std::mem::transmute(trampoline as *const c_void),
            Box::into_raw(boxed) as *mut _,
        )
    };
    // 保留 handler：srtpdec 生命周期内一直有效，不随本函数释放。
    std::mem::forget(handler);
    Ok(())
}

fn spawn_audio(
    port: Option<u16>,
    tcp: bool,
    host: &str,
    media_secret: Option<String>,
) -> Result<Option<gst::Element>> {
    let Some(port) = port else { return Ok(None) };
    let source = if tcp {
        format!(
            "tcpclientsrc host={host} port={port} ! application/x-rtp-stream ! rtpstreamdepay ! rtpjitterbuffer latency=15"
        )
    } else {
        let (caps_media, dec) = match media_secret.as_deref() {
            Some(secret) => (
                "application/x-srtp",
                format!(
                    "{} ! ",
                    crate::pipeline::srtp_dec_segment(secret).unwrap_or_default()
                ),
            ),
            None => (
                "application/x-rtp,media=audio,encoding-name=OPUS,payload=97,clock-rate=48000",
                String::new(),
            ),
        };
        format!("udpsrc port={port} caps=\"{caps_media}\" ! {dec}rtpjitterbuffer latency=15")
    };
    let pipeline_str = format!("{source} ! rtpopusdepay ! opusdec ! autoaudiosink sync=false");
    let pipeline = gst::parse::launch(&pipeline_str)?;
    if let Some(secret) = media_secret.as_deref() {
        if let Ok(bin) = pipeline.clone().downcast::<gst::Bin>() {
            if let Some(dec) = bin.by_name("srtpdec") {
                wire_srtpdec_key(&dec, secret)?;
            }
        }
    }
    pipeline.set_state(gst::State::Playing)?;
    Ok(Some(pipeline))
}

/// 无头 SRTP 接收端：构建 → 解析 → 接 srtpdec request-key → 跑到 EOS/Ctrl-C。
///
/// `gst-launch-1.0` 子进程无法连 request-key 信号，所以加密接收必须走进程内
/// gstreamer。无 media_secret 时请用普通 `receive`（gst-launch 路径）。
pub fn run_sec_receiver(
    pipeline_str: &str,
    media_secret: &str,
    audio_pipeline_str: Option<&str>,
) -> Result<()> {
    gst::init().context("GStreamer 初始化失败")?;
    let wire = |p: &gst::Element| -> Result<()> {
        if let Ok(bin) = p.clone().downcast::<gst::Bin>() {
            if let Some(dec) = bin.by_name("srtpdec") {
                wire_srtpdec_key(&dec, media_secret)?;
            }
        }
        Ok(())
    };

    let video = gst::parse::launch(pipeline_str)?;
    wire(&video)?;
    video.set_state(gst::State::Playing)?;
    let audio = match audio_pipeline_str {
        Some(s) => {
            let a = gst::parse::launch(s)?;
            wire(&a)?;
            a.set_state(gst::State::Playing)?;
            Some(a)
        }
        None => None,
    };

    // 简单主循环：等 EOS / ERROR。
    let bus = video.bus().context("video 无 bus")?;
    loop {
        let msg = bus.timed_pop(gst::ClockTime::from_mseconds(500));
        match msg {
            Some(m) => match m.view() {
                gst::MessageView::Eos(..) => break,
                gst::MessageView::Error(e) => {
                    eprintln!("receiver error: {}", e.error());
                    break;
                }
                _ => {}
            },
            None => {
                // 无消息：继续等（Ctrl-C 由进程信号终止）。
            }
        }
    }
    video.set_state(gst::State::Null)?;
    if let Some(a) = audio {
        let _ = a.set_state(gst::State::Null);
    }
    Ok(())
}

fn spawn_control(agent: &str, session: &str, token: &str) -> Result<Sender<Vec<u8>>> {
    let uri = control_uri(agent, session, token)?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let (mut ws, _) = match tungstenite::connect(uri.as_str()) {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("control WS 连接失败: {err}");
                return;
            }
        };
        for data in rx {
            if ws.send(tungstenite::Message::Binary(data.into())).is_err() {
                break;
            }
        }
    });
    Ok(tx)
}

fn control_uri(agent: &str, session: &str, token: &str) -> Result<String> {
    let base = agent.trim_end_matches('/');
    let ws_base = if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if base.starts_with("ws://") || base.starts_with("wss://") {
        base.to_string()
    } else {
        bail!("agent 需要 http:// 或 https:// 前缀");
    };
    Ok(format!(
        "{ws_base}/v1/sessions/{session}/control?token={token}"
    ))
}

fn send(control: &Option<Sender<Vec<u8>>>, data: Vec<u8>) {
    if let Some(tx) = control {
        let _ = tx.send(data);
    }
}

fn mouse_move(dx: i32, dy: i32) -> Vec<u8> {
    let dx = dx.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    let dy = dy.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    let mut out = Vec::with_capacity(5);
    out.push(1);
    out.extend_from_slice(&dx.to_le_bytes());
    out.extend_from_slice(&dy.to_le_bytes());
    out
}

fn button_event(code: u16, down: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(4);
    out.push(2);
    out.extend_from_slice(&code.to_le_bytes());
    out.push(down as u8);
    out
}

fn key_event(code: u16, down: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(4);
    out.push(3);
    out.extend_from_slice(&code.to_le_bytes());
    out.push(down as u8);
    out
}

fn wheel_event(delta: i16) -> Vec<u8> {
    let mut out = Vec::with_capacity(3);
    out.push(4);
    out.extend_from_slice(&delta.to_le_bytes());
    out
}

fn set_status(last: &mut String, status: &str) {
    if last != status {
        *last = status.to_string();
    }
}

/// 在 (x, y) 画一个软件光标：白色描边 + 黑色箭头（按下时蓝色填充）。
/// 只用 fill_rect，避免额外纹理/图片资源。
fn draw_soft_cursor(
    canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
    x: i32,
    y: i32,
    pressed: bool,
) -> Result<()> {
    use sdl2::pixels::Color;
    // 箭头形状（热点 (0,0)），每行 (dy, dx_start, width)。
    let rows = [
        (0, 0, 12),
        (1, 0, 10),
        (2, 0, 9),
        (3, 0, 8),
        (4, 0, 7),
        (5, 0, 6),
        (6, 0, 5),
        (7, 0, 4),
        (8, 0, 3),
        (9, 0, 3),
        (10, 1, 3),
        (11, 2, 3),
        (12, 3, 2),
        (13, 4, 2),
    ];
    // 先用白色描一圈边（每行外扩 1px），再填内部。
    canvas.set_draw_color(Color::RGB(255, 255, 255));
    for &(dy, dx, w) in &rows {
        let _ = canvas.fill_rect(Rect::new(x + dx - 1, y + dy - 1, w as u32 + 2, 3));
    }
    canvas.set_draw_color(if pressed {
        Color::RGB(60, 130, 255)
    } else {
        Color::RGB(20, 20, 20)
    });
    for &(dy, dx, w) in &rows {
        let _ = canvas.fill_rect(Rect::new(x + dx, y + dy, w as u32, 1));
    }
    Ok(())
}

fn evdev_key(key: Keycode) -> Option<u16> {
    Some(match key {
        Keycode::Escape => 1,
        Keycode::Num1 => 2,
        Keycode::Num2 => 3,
        Keycode::Num3 => 4,
        Keycode::Num4 => 5,
        Keycode::Num5 => 6,
        Keycode::Num6 => 7,
        Keycode::Num7 => 8,
        Keycode::Num8 => 9,
        Keycode::Num9 => 10,
        Keycode::Num0 => 11,
        Keycode::Minus => 12,
        Keycode::Equals => 13,
        Keycode::Backspace => 14,
        Keycode::Tab => 15,
        Keycode::Q => 16,
        Keycode::W => 17,
        Keycode::E => 18,
        Keycode::R => 19,
        Keycode::T => 20,
        Keycode::Y => 21,
        Keycode::U => 22,
        Keycode::I => 23,
        Keycode::O => 24,
        Keycode::P => 25,
        Keycode::LeftBracket => 26,
        Keycode::RightBracket => 27,
        Keycode::Return => 28,
        Keycode::LCtrl => 29,
        Keycode::A => 30,
        Keycode::S => 31,
        Keycode::D => 32,
        Keycode::F => 33,
        Keycode::G => 34,
        Keycode::H => 35,
        Keycode::J => 36,
        Keycode::K => 37,
        Keycode::L => 38,
        Keycode::Semicolon => 39,
        Keycode::Quote => 40,
        Keycode::Backquote => 41,
        Keycode::LShift => 42,
        Keycode::Backslash => 43,
        Keycode::Z => 44,
        Keycode::X => 45,
        Keycode::C => 46,
        Keycode::V => 47,
        Keycode::B => 48,
        Keycode::N => 49,
        Keycode::M => 50,
        Keycode::Comma => 51,
        Keycode::Period => 52,
        Keycode::Slash => 53,
        Keycode::RShift => 54,
        Keycode::KpMultiply => 55,
        Keycode::LAlt => 56,
        Keycode::Space => 57,
        Keycode::CapsLock => 58,
        Keycode::F1 => 59,
        Keycode::F2 => 60,
        Keycode::F3 => 61,
        Keycode::F4 => 62,
        Keycode::F5 => 63,
        Keycode::F6 => 64,
        Keycode::F7 => 65,
        Keycode::F8 => 66,
        Keycode::F9 => 67,
        Keycode::F10 => 68,
        Keycode::NumLockClear => 69,
        Keycode::ScrollLock => 70,
        Keycode::Kp7 => 71,
        Keycode::Kp8 => 72,
        Keycode::Kp9 => 73,
        Keycode::KpMinus => 74,
        Keycode::Kp4 => 75,
        Keycode::Kp5 => 76,
        Keycode::Kp6 => 77,
        Keycode::KpPlus => 78,
        Keycode::Kp1 => 79,
        Keycode::Kp2 => 80,
        Keycode::Kp3 => 81,
        Keycode::Kp0 => 82,
        Keycode::KpPeriod => 83,
        Keycode::F11 => 87,
        Keycode::F12 => 88,
        Keycode::RAlt => 100,
        Keycode::Home => 102,
        Keycode::Up => 103,
        Keycode::PageUp => 104,
        Keycode::Left => 105,
        Keycode::Right => 106,
        Keycode::End => 107,
        Keycode::Down => 108,
        Keycode::PageDown => 109,
        Keycode::Insert => 110,
        Keycode::Delete => 111,
        Keycode::RCtrl => 97,
        Keycode::RGui => 126,
        Keycode::LGui => 125,
        _ => return None,
    })
}

fn urlencode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn upload_file(agent: &str, session: &str, token: &str, path: &Path) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("文件名无效")?;
    let url = format!(
        "{}/v1/sessions/{session}/files?token={token}&name={}",
        agent.trim_end_matches('/'),
        urlencode(name)
    );
    let status = std::process::Command::new("curl")
        .args(["-sS", "-X", "POST", &url, "--data-binary"])
        .arg(format!("@{}", path.display()))
        .status()
        .context("执行 curl 失败")?;
    if !status.success() {
        bail!("curl 返回 {status}");
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 控制协议编码（控制端 → agent）：
    ///   [1, dx i16, dy i16] 鼠标移动 / [2, code u16, down u8] 按键 /
    ///   [3, code u16, down u8] 键盘 / [4, delta i16] 滚轮。
    /// 与 api.rs 的 handle_control_binary 约定一致。
    #[test]
    fn control_protocol_encoding_matches_spec() {
        // 鼠标移动 +10, -20
        assert_eq!(mouse_move(10, -20), vec![1, 10, 0, 236, 255]);
        // 鼠标左键按下（BTN_LEFT=0x110）
        assert_eq!(button_event(0x110, true), vec![2, 0x10, 0x01, 1]);
        // 键盘 A（KEY_A=30）释放
        assert_eq!(key_event(30, false), vec![3, 30, 0, 0]);
        // 滚轮 -3
        assert_eq!(wheel_event(-3), vec![4, 253, 255]);
    }

    /// 端到端 SRTP 回环：gst-launch 发加密流 → viewer wire_srtpdec_key 解密 → 数解码帧。
    /// 验证 request-key 注入的密钥真能解出明文（不只字符串匹配）。
    #[test]
    fn srtp_loopback_decrypts_frames() {
        gst::init().unwrap();
        if gst::ElementFactory::find("srtpenc").is_none()
            || gst::ElementFactory::find("srtpdec").is_none()
            || gst::ElementFactory::find("x264enc").is_none()
            || gst::ElementFactory::find("avdec_h264").is_none()
        {
            eprintln!("跳过：缺 srtp/x264/avdec 元素");
            return;
        }
        let secret = "deadbeef";
        let key_hex = crate::pipeline::srtp_master_key(secret);
        let port = 5987u16;

        let sender_str = format!(
            "videotestsrc is-live=true num-buffers=120 ! \
             video/x-raw,width=320,height=180,framerate=30/1 ! \
             x264enc tune=zerolatency bitrate=500 key-int-max=30 ! \
             h264parse config-interval=1 ! rtph264pay pt=96 ! \
             srtpenc name=enc key={key_hex} rtp-cipher=aes-128-icm rtp-auth=hmac-sha1-80 ! \
             udpsink host=127.0.0.1 port={port} sync=false"
        );
        let mut sender = std::process::Command::new("gst-launch-1.0")
            .args(["-q"])
            .args(crate::tokenize_pipeline(&sender_str))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("启动 sender");
        std::thread::sleep(std::time::Duration::from_millis(800));

        let rx_str = format!(
            "udpsrc port={port} caps=\"application/x-srtp\" ! \
             srtpdec name=dec ! rtpjitterbuffer latency=0 ! rtph264depay ! avdec_h264 ! \
             fakesink name=sink sync=false signal-handoffs=true"
        );
        let receiver = gst::parse::launch(&rx_str).unwrap().downcast::<gst::Bin>().unwrap();
        let dec = receiver.by_name("dec").expect("srtpdec");
        wire_srtpdec_key(&dec, secret).expect("wire key");

        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let sink = receiver.by_name("sink").expect("sink");
        sink.connect("handoff", false, move |_vals| {
            c.fetch_add(1, Ordering::SeqCst);
            None
        });

        receiver.set_state(gst::State::Playing).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(5));
        receiver.set_state(gst::State::Null).unwrap();
        let _ = sender.kill();
        let _ = sender.wait();

        let frames = count.load(Ordering::SeqCst);
        eprintln!("SRTP 回环解码帧数: {frames}");
        assert!(frames > 10, "SRTP 解密后应解出 >10 帧，实际 {frames}");
    }
}
