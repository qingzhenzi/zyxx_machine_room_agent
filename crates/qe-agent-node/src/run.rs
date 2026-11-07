//! 节点角色常驻运行入口；由统一入口 `qe-agent --role node` 调用。

use std::sync::Arc;
use std::time::Duration;

use crate::cache::ObjectCache;
use crate::config::AgentConfig;
use crate::importer::ImporterSet;
use crate::job::{ImagePullJob, ensure_staging_root};
use crate::p2p::{MtlsSeedRegistry, P2pDistribution, SeedRegistry, p2p_enabled_from_env};
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

/// 延迟获取 P2P 节点：boot 流水线在独立线程里启动 `qep2p::Node`，node 角色
/// 可能先进入下行循环；每次 `image.pull` 时再取，避免启动时序竞争。
pub type P2pNodeProvider = Arc<dyn Fn() -> Option<Arc<qep2p::Node>> + Send + Sync>;

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
    run_with_p2p(config, None).await
}

/// 同 [`run_with_config`]，额外传入 P2P 节点提供者（供 boot 阶段懒启动的节点）。
///
/// `QE_P2P_ENABLED` 未开启时不使用 provider；开启但节点尚未启动/启动失败只告警，
/// 回退 HTTP 分片下载，不 panic、不阻塞启动。
pub async fn run_with_p2p(
    config: AgentConfig,
    p2p_provider: Option<P2pNodeProvider>,
) -> Result<()> {
    // rustls 依赖树里同时存在 aws-lc-rs 与 ring 时无法自动选 provider，必须显式安装。
    let _ = rustls::crypto::ring::default_provider().install_default();
    init_tracing();
    match run_inner(config, p2p_provider).await {
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
async fn run_inner(config: AgentConfig, p2p_provider: Option<P2pNodeProvider>) -> Result<()> {
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

    // 5.1 P2P seed 登记客户端（设备证书 mTLS）；是否真正启用由 QE_P2P_ENABLED 决定。
    let p2p_seeds: Arc<dyn SeedRegistry> = Arc::new(MtlsSeedRegistry::new(transport_config(
        &credentials,
        &config,
    )));

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
        let p2p_provider = p2p_provider.clone();
        let p2p_seeds = Arc::clone(&p2p_seeds);
        dispatcher.subscribe("image.pull", move |message: DownlinkMessage| {
            let runtime_policy = Arc::clone(&runtime_policy);
            let config = config.clone();
            let presign = Arc::clone(&presign);
            let fetcher = Arc::clone(&fetcher);
            let cache = Arc::clone(&cache);
            let reporter = Arc::clone(&reporter);
            let staging_root = staging_root.clone();
            let import_lock = Arc::clone(&import_lock);
            let p2p_provider = p2p_provider.clone();
            let p2p_seeds = Arc::clone(&p2p_seeds);
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
                let mut job = ImagePullJob::new(
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
                if let Some(p2p) =
                    resolve_p2p_distribution(p2p_provider.as_deref(), Arc::clone(&p2p_seeds))
                {
                    job = job.with_p2p(p2p);
                }
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

    // 6.1b VM 远程控制（仅 node / both 角色；`--role machine` 不调用本函数，故不会注册，
    // 避免无 libvirt 权限的机器注册 VM 命令）。
    {
        let executor: Arc<dyn crate::vm_control::VmExecutor> = Arc::new(
            crate::vm_control::LibvirtExecutor::new(config.data_dir.clone())
                .with_transport(transport_config(&credentials, &config)),
        );
        let command_transport = Arc::new(transport_config(&credentials, &config));
        dispatcher.subscribe("vm.command", move |message: DownlinkMessage| {
            let executor = Arc::clone(&executor);
            let transport = Arc::clone(&command_transport);
            async move {
                let started = std::time::Instant::now();
                match crate::vm_control::execute_payload(executor, &message.payload).await {
                    Ok(result) => {
                        let elapsed_ms = started.elapsed().as_millis();
                        if result.ok {
                            info!(
                                request_id = %result.request_id,
                                op = result.op.as_str(),
                                elapsed_ms,
                                "vm.command 执行成功"
                            );
                        } else {
                            warn!(
                                request_id = %result.request_id,
                                op = result.op.as_str(),
                                elapsed_ms,
                                error = ?result.error,
                                "vm.command 执行失败"
                            );
                        }
                        report_command_result_with_retry(&transport, &result).await;
                    }
                    Err(error) => {
                        // 无法解析时没有 request_id 可回传；ack 掉避免毒消息无限重投。
                        error!(
                            id = message.id,
                            error = %format!("{error:#}"),
                            "vm.command payload 解析失败，跳过"
                        );
                    }
                }
                Ok(())
            }
        });
    }

    // 6.1c noVNC 隧道（仅 node / both）：vnc.open 到达后异步 spawn 桥接任务，
    // 不阻塞其它下行消息。WS 路径使用设备证书 serial（device-pki 校验一致）。
    {
        let executor: Arc<dyn crate::vm_control::VmExecutor> = Arc::new(
            crate::vm_control::LibvirtExecutor::new(config.data_dir.clone()),
        );
        let vnc_transport = Arc::new(transport_config(&credentials, &config));
        let certificate_serial = credentials.serial.clone();
        dispatcher.subscribe("vnc.open", move |message: DownlinkMessage| {
            let executor = Arc::clone(&executor);
            let transport = Arc::clone(&vnc_transport);
            let certificate_serial = certificate_serial.clone();
            async move {
                let open = match crate::vnc::VncOpen::parse(&message.payload) {
                    Ok(open) => open,
                    Err(error) => {
                        warn!(
                            id = message.id,
                            error = %format!("{error:#}"),
                            "vnc.open payload 解析失败，跳过"
                        );
                        return Ok(());
                    }
                };
                let serial =
                    crate::vnc::effective_serial(open.serial.as_deref(), &certificate_serial);
                let task_open = open.clone();
                let task_serial = serial.clone();
                tokio::spawn(async move {
                    let started = std::time::Instant::now();
                    match crate::vnc::run_session(
                        executor,
                        transport.as_ref(),
                        &task_open,
                        &task_serial,
                    )
                    .await
                    {
                        Ok(stats) => info!(
                            session_id = %task_open.session_id,
                            vm = %task_open.vm,
                            serial = %task_serial,
                            bytes_to_vnc = stats.bytes_to_vnc,
                            bytes_to_ws = stats.bytes_to_ws,
                            reason = stats.reason,
                            elapsed_ms = started.elapsed().as_millis(),
                            "vnc 隧道结束"
                        ),
                        Err(error) => warn!(
                            session_id = %task_open.session_id,
                            vm = %task_open.vm,
                            serial = %task_serial,
                            error = %format!("{error:#}"),
                            "vnc 隧道失败"
                        ),
                    }
                });
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

/// 解析本次 pull 的 P2P 上下文：开关关闭 / 节点未就绪都返回 `None`（回退 HTTP，不 panic）。
fn resolve_p2p_distribution(
    provider: Option<&(dyn Fn() -> Option<Arc<qep2p::Node>> + Send + Sync)>,
    seeds: Arc<dyn SeedRegistry>,
) -> Option<Arc<P2pDistribution>> {
    if !p2p_enabled_from_env() {
        return None;
    }
    let Some(node) = provider.and_then(|provider| provider()) else {
        warn!("QE_P2P_ENABLED=1 但 P2P 节点未启动（boot services 未就绪/启动失败），本次回退 HTTP");
        return None;
    };
    let blobs: Arc<dyn crate::p2p::BlobExchange> = node;
    Some(Arc::new(P2pDistribution::new(blobs, seeds)))
}

/// 回传 VM 命令结果（3 次短重试）。
///
/// 最终失败只记日志、不向上返回错误：命令已经执行过，让下行重投会导致
/// reboot/destroy 等动作被重复执行；宁可丢一次结果也不重复操作。
async fn report_command_result_with_retry(
    transport: &TransportConfig,
    result: &crate::vm_control::CommandResult,
) {
    for attempt in 1..=3u32 {
        match qe_device_pki_client::command::report_command_result(
            transport,
            &result.request_id,
            result.ok,
            result.data.as_ref(),
            result.error.as_deref(),
        )
        .await
        {
            Ok(_) => return,
            Err(error) => {
                if attempt < 3 {
                    warn!(
                        request_id = %result.request_id,
                        attempt,
                        error = %error,
                        "命令结果回传失败，稍后重试"
                    );
                    tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt))).await;
                } else {
                    error!(
                        request_id = %result.request_id,
                        error = %error,
                        "命令结果回传失败（已重试 3 次，结果可能丢失）"
                    );
                }
            }
        }
    }
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
