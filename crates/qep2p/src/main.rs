//! qep2p CLI：id / serve / share / download / send / ping。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use iroh::{EndpointAddr, EndpointId, RelayUrl};
use qep2p::node::{Event, Node, NodeConfig, RelayChoice};
use qep2p::{load_or_create_identity, parse_ticket, printable, short_id};

#[derive(Parser)]
#[command(
    name = "qep2p",
    version,
    about = "内网优先的 P2P 文件传输：iroh/QUIC + BLAKE3，mDNS 发现，可接自建 relay"
)]
struct Cli {
    /// 身份与 blob 库目录，默认 ~/.config/qep2p
    #[arg(long, global = true, env = "QEP2P_STATE_DIR", value_name = "DIR")]
    state_dir: Option<PathBuf>,

    /// 自建 relay URL，可重复；例如 http://10.0.0.5:3340
    #[arg(
        long,
        global = true,
        env = "QEP2P_RELAY",
        value_delimiter = ',',
        value_name = "URL"
    )]
    relay: Vec<String>,

    /// 自建 relay 的鉴权 token
    #[arg(
        long,
        global = true,
        env = "QEP2P_RELAY_TOKEN",
        hide_env_values = true,
        value_name = "TOKEN"
    )]
    relay_token: Option<String>,

    /// 使用 n0 公共 relay（默认关闭，纯内网不需要）
    #[arg(long, global = true)]
    relay_default: bool,

    /// 关闭 mDNS 发现（跨网段或排查问题时用）
    #[arg(long, global = true)]
    no_mdns: bool,

    /// 分享时只使用 relay 地址（双方无法直连时）
    #[arg(long, global = true)]
    relay_only: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 显示本机 endpoint id
    Id {
        #[arg(long)]
        json: bool,
    },

    /// 常驻：接收推送、响应 Ping；可选同时分享一个文件
    Serve {
        /// 接收文件的目录；不填则拒绝所有推送
        #[arg(long)]
        inbox: Option<PathBuf>,
        /// 允许推送/控制连接的 endpoint id，可重复或逗号分隔（下载仍由 ticket 授权）
        #[arg(long = "allow", value_delimiter = ',')]
        allow: Vec<String>,
        /// 启动时分享一个文件，并打印 ticket
        #[arg(long)]
        share: Option<PathBuf>,
        /// 单文件大小上限（MiB）
        #[arg(long, default_value_t = 1024)]
        max_size_mb: u64,
    },

    /// 分享一个文件，打印 ticket，直到 Ctrl-C
    Share { path: PathBuf },

    /// 按 ticket 下载
    Download {
        ticket: String,
        /// 保存目录
        #[arg(long, default_value = ".")]
        out: PathBuf,
        /// 保存文件名（默认 blob-<hash 前 12 位>）
        #[arg(long)]
        name: Option<String>,
        /// 整体超时秒数
        #[arg(long, default_value_t = 3600)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },

    /// 把文件推给在线节点
    Send {
        /// endpoint id；也可以是 id@http://relay:3340
        target: String,
        /// 要发送的文件
        file: PathBuf,
        /// 直连地址（可选）
        #[arg(long)]
        addr: Option<SocketAddr>,
        /// 额外指定 relay（会写进 ticket，供对端回连）
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
        /// 整体超时秒数
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },

    /// 通过控制通道测 RTT
    Ping {
        target: String,
        #[arg(long, default_value_t = 4)]
        count: u32,
        #[arg(long)]
        addr: Option<SocketAddr>,
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
        #[arg(long, default_value_t = 20)]
        timeout: u64,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("qep2p: {e:#}");
        std::process::exit(1);
    }
}

struct Global {
    state_dir: PathBuf,
    relay: Vec<String>,
    relay_token: Option<String>,
    relay_default: bool,
    no_mdns: bool,
    relay_only: bool,
}

async fn run(cli: Cli) -> Result<()> {
    let state_dir = match &cli.state_dir {
        Some(dir) => dir.clone(),
        None => default_state_dir()?,
    };
    let global = Global {
        state_dir,
        relay: cli.relay.clone(),
        relay_token: cli.relay_token.clone(),
        relay_default: cli.relay_default,
        no_mdns: cli.no_mdns,
        relay_only: cli.relay_only,
    };

    match cli.cmd {
        Cmd::Id { json } => {
            let id = load_or_create_identity(&global.state_dir)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "endpoint_id": id.to_string(),
                        "short": short_id(&id),
                        "hostname": qep2p::hostname(),
                        "state_dir": global.state_dir,
                    }))?
                );
            } else {
                println!("endpoint id: {id}");
                println!("short:       {}", short_id(&id));
                println!("host:        {}", qep2p::hostname());
                println!("state:       {}", global.state_dir.display());
            }
            Ok(())
        }
        Cmd::Serve {
            inbox,
            allow,
            share,
            max_size_mb,
        } => {
            let allow = parse_allow(&allow)?;
            let cfg = node_config(
                &global,
                &global.state_dir,
                inbox,
                allow,
                max_size_mb.saturating_mul(1024 * 1024),
            )?;
            let node = Node::start(cfg).await?;
            print_node(&node);
            if let Some(path) = share {
                let shared = node.share_path(&path).await?;
                println!("{}", shared.ticket);
                eprintln!("[qep2p] 已分享 {}（{} 字节）", path.display(), shared.size);
            }
            eprintln!("[qep2p] 等待连接（Ctrl-C 退出）");
            tokio::signal::ctrl_c().await?;
            node.shutdown().await
        }
        Cmd::Share { path } => {
            let cfg = node_config(
                &global,
                &global.state_dir,
                None,
                Vec::new(),
                1024 * 1024 * 1024,
            )?;
            let node = Node::start(cfg).await?;
            print_node(&node);
            let shared = node.share_path(&path).await?;
            println!("{}", shared.ticket);
            eprintln!("[qep2p] 已分享 {}（{} 字节）", path.display(), shared.size);
            eprintln!("[qep2p] 把 ticket 发给对端即可下载（Ctrl-C 退出）");
            tokio::signal::ctrl_c().await?;
            node.shutdown().await
        }
        Cmd::Download {
            ticket,
            out,
            name,
            timeout,
            json,
        } => {
            let ticket = parse_ticket(&ticket)?;
            let cfg = node_config(
                &global,
                &global.state_dir,
                None,
                Vec::new(),
                1024 * 1024 * 1024,
            )?;
            let node = Node::start(cfg).await?;
            let printer = spawn_progress(node.subscribe());
            let result = tokio::time::timeout(
                Duration::from_secs(timeout),
                node.download_named(&ticket, &out, name.as_deref()),
            )
            .await;
            printer.abort();
            let outcome = match result {
                Ok(result) => result?,
                Err(_) => bail!("下载超时（{timeout} 秒）"),
            };
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "ok": true,
                        "path": outcome.path,
                        "bytes": outcome.bytes,
                    }))?
                );
            } else {
                println!(
                    "下载完成：{}（{} 字节）",
                    outcome.path.display(),
                    outcome.bytes
                );
            }
            node.shutdown().await
        }
        Cmd::Send {
            target,
            file,
            addr,
            relay,
            timeout,
            json,
        } => {
            let relay_url = match relay {
                Some(url) => Some(url.parse::<RelayUrl>().context("--relay URL 非法")?),
                None => None,
            };
            let target = parse_target(&target, addr, relay_url)?;
            let cfg = node_config(
                &global,
                &global.state_dir,
                None,
                Vec::new(),
                1024 * 1024 * 1024,
            )?;
            let node = Node::start(cfg).await?;
            let outcome = node
                .send_file(target, &file, Duration::from_secs(timeout))
                .await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "ok": true,
                        "to": outcome.to.to_string(),
                        "name": outcome.name,
                        "bytes": outcome.bytes,
                        "remote_path": outcome.remote_path,
                    }))?
                );
            } else {
                println!(
                    "发送成功 → {}：{}（{} 字节）",
                    short_id(&outcome.to),
                    outcome.name,
                    outcome.bytes
                );
                if let Some(path) = outcome.remote_path {
                    println!("  对端保存到：{}", printable(&path, 200));
                }
            }
            node.shutdown().await
        }
        Cmd::Ping {
            target,
            count,
            addr,
            relay,
            timeout,
        } => {
            let relay_url = match relay {
                Some(url) => Some(url.parse::<RelayUrl>().context("--relay URL 非法")?),
                None => None,
            };
            let target = parse_target(&target, addr, relay_url)?;
            let cfg = node_config(
                &global,
                &global.state_dir,
                None,
                Vec::new(),
                1024 * 1024 * 1024,
            )?;
            let node = Node::start(cfg).await?;
            let outcome = node
                .ping(
                    target,
                    count,
                    Duration::from_millis(200),
                    Duration::from_secs(timeout),
                )
                .await?;
            println!("pong from {}", short_id(&outcome.to));
            for (i, rtt) in outcome.rtts.iter().enumerate() {
                println!("  seq={:<3} {:.3} ms", i + 1, rtt.as_secs_f64() * 1000.0);
            }
            let avg: f64 = outcome.rtts.iter().map(|d| d.as_secs_f64()).sum::<f64>()
                / outcome.rtts.len() as f64;
            println!("  avg {:.3} ms", avg * 1000.0);
            node.shutdown().await
        }
    }
}

fn node_config(
    global: &Global,
    state_dir: &Path,
    inbox: Option<PathBuf>,
    allow: Vec<EndpointId>,
    max_file_size: u64,
) -> Result<NodeConfig> {
    if global.relay_default && !global.relay.is_empty() {
        bail!("--relay-default 和 --relay 只能选一个");
    }
    if global.relay_token.is_some() && global.relay.is_empty() {
        bail!("--relay-token 需要配合 --relay 使用");
    }
    let relay = if global.relay_default {
        RelayChoice::Default
    } else if !global.relay.is_empty() {
        RelayChoice::Custom {
            urls: global.relay.clone(),
            token: global.relay_token.clone(),
        }
    } else {
        RelayChoice::Disabled
    };
    Ok(NodeConfig {
        state_dir: state_dir.to_path_buf(),
        relay,
        mdns: !global.no_mdns,
        inbox,
        allow,
        max_file_size,
        relay_only: global.relay_only,
    })
}

fn parse_allow(raw: &[String]) -> Result<Vec<EndpointId>> {
    raw.iter()
        .map(|s| {
            s.parse::<EndpointId>()
                .with_context(|| format!("--allow 需要完整的 endpoint id：{s}"))
        })
        .collect()
}

fn parse_target(
    raw: &str,
    addr: Option<SocketAddr>,
    relay: Option<RelayUrl>,
) -> Result<EndpointAddr> {
    let (id_part, relay_part) = match raw.split_once('@') {
        Some((id, relay)) => (id, Some(relay)),
        None => (raw, None),
    };
    let id: EndpointId = id_part
        .trim()
        .parse()
        .context("target 必须是 64 位十六进制 endpoint id")?;
    let mut target = EndpointAddr::new(id);
    if let Some(addr) = addr {
        target = target.with_ip_addr(addr);
    }
    if let Some(url) = relay_part {
        target = target.with_relay_url(url.parse().context("target 里的 relay URL 非法")?);
    }
    if let Some(url) = relay {
        target = target.with_relay_url(url);
    }
    Ok(target)
}

fn print_node(node: &Node) {
    let addr = node.addr();
    let addrs: Vec<String> = addr.addrs.iter().map(ToString::to_string).collect();
    eprintln!("[qep2p] endpoint id {}", node.id());
    eprintln!("[qep2p] short       {}", short_id(&node.id()));
    eprintln!(
        "[qep2p] addrs       {}",
        if addrs.is_empty() {
            "-".to_string()
        } else {
            addrs.join(", ")
        }
    );
}

fn spawn_progress(mut rx: tokio::sync::broadcast::Receiver<Event>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last = 0u64;
        while let Ok(event) = rx.recv().await {
            if let Event::Progress { received } = event
                && received >= last + 16 * 1024 * 1024
            {
                eprintln!("[qep2p] 已接收 {:.1} MiB", received as f64 / 1048576.0);
                last = received;
            }
        }
    })
}

fn default_state_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir).join("qep2p"));
    }
    let home = std::env::var("HOME").context("HOME 未设置，请用 --state-dir 指定目录")?;
    Ok(PathBuf::from(home).join(".config/qep2p"))
}
