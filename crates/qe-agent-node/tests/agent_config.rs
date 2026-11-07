//! P2 节点配置管理单测（契约 §17.5）：本地配置文件优先级、远端策略覆盖、
//! `config.update` 应用与非法值拒绝、启动拉取失败回退。
//!
//! 全部使用本地临时文件与 mock 客户端，不依赖真实 device-pki / 网络。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use qe_agent_node::BoxFuture;
use qe_agent_node::config::AgentConfig;
use qe_agent_node::policy::{
    AgentConfigClient, AgentConfigReply, RuntimePolicy, fetch_and_apply, handle_config_update,
    validate_complete_policy,
};
use qe_domain::agent_config::AgentPolicy;

/// 用内存 map 模拟 env：不改写进程环境，测试可并行。
fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    move |key| map.get(key).cloned()
}

fn base_env(config_file: &Path) -> Vec<(String, String)> {
    vec![
        (
            "DEVICE_PKI_MTLS_URL".to_owned(),
            "https://node:4441".to_owned(),
        ),
        ("DEVICE_SLOT".to_owned(), "slot-1".to_owned()),
        (
            "AGENT_CONFIG_FILE".to_owned(),
            config_file.display().to_string(),
        ),
    ]
}

fn load_config(config_file: &Path, extra: &[(&str, &str)]) -> Result<AgentConfig> {
    let mut pairs = base_env(config_file);
    pairs.extend(
        extra
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
    );
    let refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    AgentConfig::from_lookup(lookup_from(&refs))
}

fn local_policy() -> AgentPolicy {
    AgentPolicy {
        lxc_auto_start: false,
        poll_interval_secs: 2,
        download_concurrency: 4,
        cache_max_bytes: 10 * 1024 * 1024 * 1024,
        telemetry_fast_interval_secs: 10,
        telemetry_full_interval_secs: 300,
    }
}

fn complete_payload(revision: i64, policy: &AgentPolicy) -> String {
    serde_json::json!({
        "revision": revision,
        "policy": {
            "lxc_auto_start": policy.lxc_auto_start,
            "poll_interval_secs": policy.poll_interval_secs,
            "download_concurrency": policy.download_concurrency,
            "cache_max_bytes": policy.cache_max_bytes,
            "telemetry_fast_interval_secs": policy.telemetry_fast_interval_secs,
            "telemetry_full_interval_secs": policy.telemetry_full_interval_secs,
        }
    })
    .to_string()
}

struct StaticClient {
    reply: std::result::Result<AgentConfigReply, String>,
}

impl AgentConfigClient for StaticClient {
    fn fetch<'a>(&'a self) -> BoxFuture<'a, Result<AgentConfigReply>> {
        Box::pin(async move {
            match &self.reply {
                Ok(reply) => Ok(reply.clone()),
                Err(message) => Err(anyhow!(message.clone())),
            }
        })
    }
}

// ---------------- 本地配置文件与优先级 ----------------

#[test]
fn missing_file_uses_builtin_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does-not-exist.json");
    let config = load_config(&path, &[]).unwrap();

    assert_eq!(config.local_policy, AgentPolicy::default());
    assert_eq!(config.local_policy.poll_interval_secs, 2);
    assert_eq!(config.local_policy.download_concurrency, 4);
    assert_eq!(config.local_policy.cache_max_bytes, 10_737_418_240);
    assert!(!config.local_policy.lxc_auto_start);
    // 旧字段与 local_policy 保持同步
    assert_eq!(config.download_concurrency, 4);
    assert_eq!(config.cache_max_bytes, 10_737_418_240);
    assert!(!config.lxc_auto_start);
}

#[test]
fn config_file_patch_overrides_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-config.json");
    std::fs::write(
        &path,
        r#"{"policy":{"lxc_auto_start":true,"poll_interval_secs":7,
             "download_concurrency":3,"cache_max_bytes":2048}}"#,
    )
    .unwrap();

    let config = load_config(&path, &[]).unwrap();
    assert_eq!(config.config_file, path);
    assert_eq!(
        config.local_policy,
        AgentPolicy {
            lxc_auto_start: true,
            poll_interval_secs: 7,
            download_concurrency: 3,
            cache_max_bytes: 2048,
            ..AgentPolicy::default()
        }
    );
    assert!(config.lxc_auto_start);
    assert_eq!(config.download_concurrency, 3);
    assert_eq!(config.cache_max_bytes, 2048);
}

#[test]
fn env_overrides_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-config.json");
    std::fs::write(
        &path,
        r#"{"policy":{"lxc_auto_start":false,"poll_interval_secs":7,
             "download_concurrency":3,"cache_max_bytes":2048}}"#,
    )
    .unwrap();

    let config = load_config(
        &path,
        &[
            ("LXC_AUTO_START", "true"),
            ("POLL_INTERVAL_SECS", "9"),
            ("DOWNLOAD_CONCURRENCY", "8"),
            ("CACHE_MAX_BYTES", "4096"),
        ],
    )
    .unwrap();

    assert_eq!(
        config.local_policy,
        AgentPolicy {
            lxc_auto_start: true,
            poll_interval_secs: 9,
            download_concurrency: 8,
            cache_max_bytes: 4096,
            ..AgentPolicy::default()
        }
    );
}

#[test]
fn partial_config_file_keeps_other_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-config.json");
    std::fs::write(&path, r#"{"policy":{"poll_interval_secs":5}}"#).unwrap();

    let config = load_config(&path, &[]).unwrap();
    assert_eq!(config.local_policy.poll_interval_secs, 5);
    assert_eq!(config.local_policy.download_concurrency, 4);
    assert_eq!(config.local_policy.cache_max_bytes, 10 * 1024 * 1024 * 1024);
    assert!(!config.local_policy.lxc_auto_start);
}

#[test]
fn malformed_config_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-config.json");
    std::fs::write(&path, "{not json").unwrap();

    let error = load_config(&path, &[]).unwrap_err();
    assert!(
        error.to_string().contains("解析 agent 配置文件失败"),
        "{error:#}"
    );
}

#[test]
fn out_of_range_config_file_value_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-config.json");
    std::fs::write(&path, r#"{"policy":{"poll_interval_secs":0}}"#).unwrap();

    let error = load_config(&path, &[]).unwrap_err();
    assert!(
        error.to_string().contains("POLL_INTERVAL_SECS"),
        "{error:#}"
    );

    // env 覆盖非法文件值后应能启动（优先级语义：校验的是最终策略）
    let config = load_config(&path, &[("POLL_INTERVAL_SECS", "5")]).unwrap();
    assert_eq!(config.local_policy.poll_interval_secs, 5);
}

#[test]
fn unknown_config_file_fields_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-config.json");
    std::fs::write(
        &path,
        r#"{"policy":{"poll_interval_secs":3,"unknown_future_field":true}}"#,
    )
    .unwrap();
    let config = load_config(&path, &[]).unwrap();
    assert_eq!(config.local_policy.poll_interval_secs, 3);
}

// ---------------- 远端策略覆盖与失败回退 ----------------

#[tokio::test]
async fn remote_policy_overrides_local_policy() {
    let remote = AgentPolicy {
        lxc_auto_start: true,
        poll_interval_secs: 11,
        download_concurrency: 12,
        cache_max_bytes: 8192,
        telemetry_fast_interval_secs: 15,
        telemetry_full_interval_secs: 600,
    };
    let runtime = RuntimePolicy::new(local_policy());
    let client = StaticClient {
        reply: Ok(AgentConfigReply {
            revision: 7,
            policy: remote.clone(),
        }),
    };

    let change = fetch_and_apply(&client, &runtime).await.unwrap();
    assert_eq!(change.revision, 7);
    assert_eq!(change.previous, local_policy());
    assert_eq!(change.current, remote);
    assert_eq!(runtime.current(), remote);
    assert_eq!(runtime.poll_interval().as_secs(), 11);
}

#[tokio::test]
async fn remote_fetch_failure_falls_back_to_local_policy() {
    let runtime = RuntimePolicy::new(local_policy());
    let client = StaticClient {
        reply: Err("connection refused".to_owned()),
    };

    let error = fetch_and_apply(&client, &runtime).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("connection refused"),
        "{error:#}"
    );
    // 启动拉取失败不能改动本地策略
    assert_eq!(runtime.current(), local_policy());
}

#[tokio::test]
async fn invalid_remote_policy_is_rejected_and_local_kept() {
    let runtime = RuntimePolicy::new(local_policy());
    let client = StaticClient {
        reply: Ok(AgentConfigReply {
            revision: 3,
            policy: AgentPolicy {
                poll_interval_secs: 0,
                ..local_policy()
            },
        }),
    };

    let error = fetch_and_apply(&client, &runtime).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("远端 agent 配置非法"),
        "{error:#}"
    );
    assert_eq!(runtime.current(), local_policy());
}

// ---------------- config.update ----------------

#[test]
fn config_update_applies_complete_policy() {
    let runtime = RuntimePolicy::new(local_policy());
    let next = AgentPolicy {
        lxc_auto_start: true,
        poll_interval_secs: 30,
        download_concurrency: 4,
        cache_max_bytes: 10 * 1024 * 1024 * 1024,
        ..local_policy()
    };

    let change = handle_config_update(&complete_payload(9, &next), &runtime).unwrap();
    assert_eq!(change.revision, 9);
    assert_eq!(change.previous, local_policy());
    assert_eq!(change.current, next);
    assert_eq!(runtime.current(), next);
    // 只有 lxc_auto_start / poll_interval_secs 变化 → 不需要重启
    assert!(!change.restart_required());
    assert_eq!(runtime.poll_interval().as_secs(), 30);
}

#[test]
fn config_update_flags_restart_only_for_puller_fields() {
    let runtime = RuntimePolicy::new(local_policy());
    let next = AgentPolicy {
        lxc_auto_start: false,
        poll_interval_secs: 2,
        download_concurrency: 9,
        cache_max_bytes: 10 * 1024 * 1024 * 1024,
        ..local_policy()
    };
    let change = handle_config_update(&complete_payload(2, &next), &runtime).unwrap();
    assert!(change.restart_required());

    // cache_max_bytes 变化同样需要重启
    let next = AgentPolicy {
        download_concurrency: 4,
        cache_max_bytes: 4096,
        ..next
    };
    let change = handle_config_update(&complete_payload(3, &next), &runtime).unwrap();
    assert!(change.restart_required());
    assert_eq!(runtime.current().cache_max_bytes, 4096);
}

#[test]
fn config_update_rejects_out_of_range_values_without_mutation() {
    let runtime = RuntimePolicy::new(local_policy());
    let cases = [
        AgentPolicy {
            poll_interval_secs: 0,
            ..local_policy()
        },
        AgentPolicy {
            poll_interval_secs: 61,
            ..local_policy()
        },
        AgentPolicy {
            download_concurrency: 0,
            ..local_policy()
        },
        AgentPolicy {
            download_concurrency: 33,
            ..local_policy()
        },
        AgentPolicy {
            cache_max_bytes: 1024 * 1024 * 1024 * 1024 + 1,
            ..local_policy()
        },
    ];

    for (index, policy) in cases.iter().enumerate() {
        let error = handle_config_update(&complete_payload(index as i64 + 1, policy), &runtime)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("config.update 策略非法"),
            "case {index}: {error:#}"
        );
        assert_eq!(runtime.current(), local_policy(), "非法策略不能生效");
    }
}

#[test]
fn config_update_rejects_bad_payload() {
    let runtime = RuntimePolicy::new(local_policy());

    // 非 JSON
    assert!(handle_config_update("not-json", &runtime).is_err());
    // 缺 revision
    assert!(handle_config_update(r#"{"policy":{}}"#, &runtime).is_err());
    // 缺 policy
    assert!(handle_config_update(r#"{"revision":1}"#, &runtime).is_err());
    // policy 类型错误
    assert!(handle_config_update(r#"{"revision":1,"policy":"bad"}"#, &runtime).is_err());
    // 未知 scope / 多余字段不影响策略字段解析，但未知字段本身应被忽略
    let ok = handle_config_update(
        &serde_json::json!({
            "revision": 4,
            "policy": {
                "lxc_auto_start": true,
                "poll_interval_secs": 6,
                "download_concurrency": 2,
                "cache_max_bytes": 0,
            },
            "unexpected": "ignored"
        })
        .to_string(),
        &runtime,
    );
    assert!(ok.is_ok());
    assert_eq!(runtime.current().poll_interval_secs, 6);
    assert_eq!(runtime.current().cache_max_bytes, 0);
}

#[test]
fn complete_policy_validation_accepts_boundaries() {
    for policy in [
        AgentPolicy {
            poll_interval_secs: 1,
            download_concurrency: 1,
            cache_max_bytes: 0,
            ..local_policy()
        },
        AgentPolicy {
            poll_interval_secs: 60,
            download_concurrency: 32,
            cache_max_bytes: 1024 * 1024 * 1024 * 1024,
            ..local_policy()
        },
    ] {
        validate_complete_policy(&policy).unwrap();
    }
}

#[test]
fn runtime_policy_is_shareable_across_tasks() {
    let runtime = Arc::new(RuntimePolicy::new(local_policy()));
    let handle = Arc::clone(&runtime);
    std::thread::spawn(move || {
        handle.replace(AgentPolicy {
            poll_interval_secs: 4,
            ..local_policy()
        });
    })
    .join()
    .unwrap();
    assert_eq!(runtime.poll_interval().as_secs(), 4);
}
