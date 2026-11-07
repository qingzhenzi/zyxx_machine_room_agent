//! 节点角色常驻运行入口；由统一入口 `qe-agent --role node` 调用。

use std::sync::Arc;
use std::time::Duration;

use crate::cache::ObjectCache;
use crate::config::AgentConfig;
use crate::importer::ImporterSet;
use crate::job::{ImagePullJob, ensure_staging_root};
use crate::policy::{self, RuntimePolicy};
use crate::presign::{MtlsPresignClient, PresignClient};
use crate::pull::{HttpObjectFetcher, ObjectFetcher, Puller};
use crate::report::{MtlsReporter, Reporter};
use anyhow::{Context, Result};
use qe_device_pki_client::creds::{self, Credentials};
use qe_device_pki_client::downlink::{self, Dispatcher};
use qe_device_pki_client::transport::{self, DownlinkMessage, OutboxQueue, TransportConfig};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

/// 单次拉取的下行消息条数。
const DOWNLINK_BATCH: i64 = 20;

/// 远端策略应用后的通知钩子（`qe-agent` 用来联动 boot 周期检查等）。
///
/// 用进程级 `OnceLock` 注册而非改 `AgentConfig`（其 derive `Eq` 容不下闭包）。
/// 仅统一入口注册；独立使用本库时钩子为空，零开销。
type RemotePolicyHook = Arc<dyn Fn(u64) + Send + Sync>;
static REMOTE_POLICY_HOOK: std::sync::OnceLock<RemotePolicyHook> = std::sync::OnceLock::new();

/// 注册远端策略应用通知（参数：新的 `telemetry_full_interval_secs`，
/// 即"周期性深度检查"节奏；boot 周期检查直接复用它作为复检间隔）。
/// 进程生命周期内只生效一次。
pub fn on_remote_policy_applied(hook: impl Fn(u64) + Send + Sync + 'static) {
    let _ = REMOTE_POLICY_HOOK.set(Arc::new(hook));
}

fn notify_remote_policy(full_interval_secs: u64) {
    if let Some(hook) = REMOTE_POLICY_HOOK.get() {
        hook(full_interval_secs);
    }
}

/// 从进程环境读取配置并运行（本库直用的便捷入口，测试/嵌入用）。
pub async fn run() -> Result<()> {
    let config = AgentConfig::from_env().context("读取 agent 配置失败")?;
    run_with_config(config).await
}

/// 使用已解析的配置运行（统一 `qe-agent` 入口；数据目录/槽位可由调用方覆盖）。
pub async fn run_with_config(config: AgentConfig) -> Result<()> {
    // rustls 依赖树里同时存在 aws-lc-rs 与 ring 时无法自动选 provider，必须显式安装。
    let _ = rustls::crypto::ring::default_provider().install_default();
    init_tracing();
    match run_inner(config).await {
        Ok(()) => {
            info!("qe-agent-node 已退出");
            Ok(())
        }
        Err(error) => {
            error!(error = %format!("{error:#}"), "qe-agent-node 退出");
            Err(error)
        }
    }
}

/// 实际启动流程（原 `main.rs` 中的 `run()` 实现）。
async fn run_inner(config: AgentConfig) -> Result<()> {
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("创建数据目录失败: {}", config.data_dir.display()))?;
    std::fs::create_dir_all(&config.cache_dir)
        .with_context(|| format!("创建缓存目录失败: {}", config.cache_dir.display()))?;

    // 本地策略 = 内置默认 < 配置文件 patch < env（远端在 confirm 之后覆盖）。
    let runtime_policy = Arc::new(RuntimePolicy::new(config.local_policy.clone()));
    info!(
        slot = %config.slot,
        endpoint = %config.mtls_endpoint,
        data_dir = %config.data_dir.display(),
        cache_dir = %config.cache_dir.display(),
        config_file = %config.config_file.display(),
        lxc_auto_start = config.local_policy.lxc_auto_start,
        poll_interval_secs = config.local_policy.poll_interval_secs,
        download_concurrency = config.local_policy.download_concurrency,
        cache_max_bytes = config.local_policy.cache_max_bytes,
        "启动 qe-agent-node（本地策略已加载）"
    );

    // 1. 设备凭证（复用 device-pki-client 的目录布局：<data_dir>/creds/<slot>）
    let creds_dir =
        creds::locate(&config.slot, Some(&config.data_dir)).context("定位设备凭证目录失败")?;
    let mut credentials = Credentials::load(&creds_dir)
        .with_context(|| format!("加载设备凭证失败: {}", creds_dir.display()))?;
    // 以环境变量为准（证书 meta 里的端点可能是注册时的旧地址）。
    credentials.mtls_endpoint = config.mtls_endpoint.clone();
    if let Err(issue) = credentials.verify() {
        warn!(
            slot = %config.slot,
            issue = %issue,
            advice = %issue.advice(),
            "设备凭证自检未通过，仍尝试启动"
        );
    }

    // 2. mTLS confirm。服务端对已 active 的证书可能返回错误，这不是致命问题。
    let confirm_config = transport_config(&credentials, &config);
    match transport::confirm(&confirm_config).await {
        Ok(reply) => info!(
            status = %reply.status,
            serial = %reply.serial,
            mtls = %reply.mtls,
            "mTLS 通讯已确认"
        ),
        Err(error) => warn!(
            error = %error,
            "mTLS confirm 失败（若证书已激活可忽略），继续启动下行轮询"
        ),
    }

    // 2.1 启动拉取远端策略：远端优先级最高；失败只 warn，绝不阻塞启动。
    match policy::MtlsAgentConfigClient::new(&transport_config(&credentials, &config)) {
        Ok(client) => match policy::fetch_and_apply(&client, &runtime_policy).await {
            Ok(change) => info!(revision = change.revision, "启动远端策略拉取完成"),
            Err(error) => warn!(
                error = %format!("{error:#}"),
                "启动拉取远端策略失败，继续使用本地策略"
            ),
        },
        Err(error) => warn!(
            error = %format!("{error:#}"),
            "构建远端策略 mTLS 客户端失败，继续使用本地策略"
        ),
    }

    // 启动时生效策略：Puller/缓存按此固化（运行中变更需重启）。
    let startup_policy = runtime_policy.current();
    info!(
        lxc_auto_start = startup_policy.lxc_auto_start,
        poll_interval_secs = startup_policy.poll_interval_secs,
        download_concurrency = startup_policy.download_concurrency,
        cache_max_bytes = startup_policy.cache_max_bytes,
        "当前生效策略"
    );

    // 3. 上行 outbox（持久化，发送成功后删除）
    let outbox = Arc::new(
        OutboxQueue::open(&config.data_dir.join("outbox"))
            .map_err(|error| anyhow::anyhow!("打开上行 outbox 失败: {error}"))?,
    );
    let reporter: Arc<dyn Reporter> = Arc::new(MtlsReporter::new(
        transport_config(&credentials, &config),
        Arc::clone(&outbox),
    ));

    // 4. 预签名 + 对象下载 + 内容寻址缓存（并发/上限取启动时策略）
    let presign: Arc<dyn PresignClient> = Arc::new(
        MtlsPresignClient::new(&transport_config(&credentials, &config))
            .context("构建预签名 mTLS 客户端失败")?,
    );
    let fetcher: Arc<dyn ObjectFetcher> =
        Arc::new(HttpObjectFetcher::new().context("构建对象下载客户端失败")?);
    let cache = Arc::new(ObjectCache::new(
        &config.cache_dir,
        startup_policy.cache_max_bytes,
    ));
    let startup_concurrency = startup_policy.download_concurrency.max(1);

    // 5. staging 根目录
    let staging_root = config.data_dir.join("staging");
    ensure_staging_root(&staging_root)?;

    // 6. 下行分发：image.pull 串行执行；config.update 立即应用。
    let dispatcher = Arc::new(Dispatcher::new());
    let import_lock = Arc::new(tokio::sync::Mutex::new(()));

    // 6.1 image.pull：每次任务开始时读当前策略，按 lxc_auto_start 重建 ImporterSet。
    {
        let runtime_policy = Arc::clone(&runtime_policy);
        let config = config.clone();
        let presign = Arc::clone(&presign);
        let fetcher = Arc::clone(&fetcher);
        let cache = Arc::clone(&cache);
        let reporter = Arc::clone(&reporter);
        let staging_root = staging_root.clone();
        let import_lock = Arc::clone(&import_lock);
        dispatcher.subscribe("image.pull", move |message: DownlinkMessage| {
            let runtime_policy = Arc::clone(&runtime_policy);
            let config = config.clone();
            let presign = Arc::clone(&presign);
            let fetcher = Arc::clone(&fetcher);
            let cache = Arc::clone(&cache);
            let reporter = Arc::clone(&reporter);
            let staging_root = staging_root.clone();
            let import_lock = Arc::clone(&import_lock);
            async move {
                // 导入必须串行：docker load / incus image import / virsh vol-upload
                // 同时跑会争抢同一份运行时状态。
                let _guard = import_lock.lock().await;
                // 持锁只到 clone 为止，绝不跨 await。
                let current = runtime_policy.current();
                let mut task_config = config.clone();
                task_config.lxc_auto_start = current.lxc_auto_start;
                // Puller 很轻（只持有 Arc 与启动时并发数）；按任务重建以套用
                // 当前 lxc_auto_start，download_concurrency 仍按启动时策略。
                let job = ImagePullJob::new(
                    Puller::new(
                        Arc::clone(&presign),
                        Arc::clone(&fetcher),
                        Arc::clone(&cache),
                        startup_concurrency,
                    ),
                    ImporterSet::builtin(&task_config),
                    Arc::clone(&reporter),
                    &staging_root,
                );
                match job.run(&message.payload).await {
                    Ok(outcome) if outcome.ok => info!(
                        transfer_id = %outcome.transfer_id,
                        imported_ref = ?outcome.imported_ref,
                        "image.pull 处理成功"
                    ),
                    Ok(outcome) => warn!(
                        transfer_id = %outcome.transfer_id,
                        error = ?outcome.error,
                        "image.pull 失败，已上报 transfer.completed(ok=false)"
                    ),
                    Err(error) => {
                        // payload 无法解析：ack 掉避免毒消息无限重投，错误留给运维排查。
                        error!(
                            id = message.id,
                            error = %format!("{error:#}"),
                            "image.pull payload 解析失败，跳过该消息"
                        );
                    }
                }
                Ok(())
            }
        });
    }

    // 6.2 config.update：校验完整策略后替换；坏 payload 返回 Err → 不 ack，下轮重投。
    {
        let runtime_policy = Arc::clone(&runtime_policy);
        dispatcher.subscribe("config.update", move |message: DownlinkMessage| {
            let runtime_policy = Arc::clone(&runtime_policy);
            async move {
                match policy::handle_config_update(&message.payload, &runtime_policy) {
                    Ok(change) => {
                        notify_remote_policy(change.current.telemetry_full_interval_secs);
                        Ok(())
                    }
                    Err(error) => {
                        error!(
                            id = message.id,
                            error = %format!("{error:#}"),
                            "config.update 处理失败，不 ack（下轮重投）"
                        );
                        Err(qe_device_pki_client::Error::Unexpected(format!(
                            "config.update 处理失败: {error:#}"
                        )))
                    }
                }
            }
        });
    }

    // 7. 下行轮询循环 + Ctrl-C：每次轮询后按当前策略的 poll_interval_secs 休眠。
    let downlink_config = transport_config(&credentials, &config);
    let state_file = downlink::state_path(&config.slot, Some(&config.data_dir))
        .context("定位下行游标文件失败")?;
    // 第一次立即轮询，之后睡眠间隔取「处理完之后」的最新策略。
    let mut next_delay = Duration::ZERO;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("收到 Ctrl-C，停止轮询并退出");
                break;
            }
            _ = tokio::time::sleep(next_delay) => {
                match downlink::poll_once(
                    &downlink_config,
                    &dispatcher,
                    &state_file,
                    DOWNLINK_BATCH,
                )
                .await
                {
                    Ok(0) => debug!("没有新的下行消息"),
                    Ok(count) => info!(count, "处理下行消息"),
                    Err(error) => warn!(
                        error = %error,
                        "轮询下行失败，下轮按当前策略间隔重试"
                    ),
                }
                // 本批可能刚应用了 config.update，睡眠取处理之后的最新策略，
                // 保证 poll_interval_secs 从下一轮立即生效。
                next_delay = runtime_policy.poll_interval();
            }
        }
    }

    // 未送达的上行消息已持久化在 outbox，下次启动或下一次上报时重放。
    drop(outbox);
    Ok(())
}

/// 构造一份 mTLS 传输配置（`TransportConfig` 不实现 Clone，按需构造）。
fn transport_config(credentials: &Credentials, config: &AgentConfig) -> TransportConfig {
    let mut credentials = credentials.clone();
    credentials.mtls_endpoint = config.mtls_endpoint.clone();
    TransportConfig::new(credentials, config.data_dir.clone())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,qe_agent_node=debug"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(false)
        .compact()
        .try_init();
}
