//! 容器生命周期控制（Incus CLI 后端）：`container.command` 下行执行器。
//!
//! 课程环境容器由镜像管道导入到 Incus；本模块只做**管理**：
//! `list / start / stop / restart / delete / snapshot.create|list|restore|delete`。
//! 通过 `incus` CLI（`QE_INCUS_BIN` 可覆盖）执行，参数逐个传给 argv，不经过 shell。
//!
//! Docker 容器不在本模块范围（课程环境走 Incus/LXC）；后续可扩展 `qe-container` Engine。

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use uuid::Uuid;

/// 单条容器命令默认超时（启动/停止/快照通常秒级，留足余量）。
pub const CONTAINER_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// 容器名/快照名约束（与 VM 名一致，防 argv 注入）。
pub const CONTAINER_NAME_MAX_CHARS: usize = 64;

/// 允许的容器 op。
pub const CONTAINER_OPS: &[&str] = &[
    "list",
    "start",
    "stop",
    "restart",
    "delete",
    "snapshot.create",
    "snapshot.list",
    "snapshot.restore",
    "snapshot.delete",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerCommand {
    pub request_id: Uuid,
    pub op: String,
    pub name: Option<String>,
    pub snapshot: Option<String>,
    pub force: bool,
}

/// 解析并校验 `container.command` payload。
pub fn parse(payload: &str) -> Result<ContainerCommand> {
    let root: Value = serde_json::from_str(payload).context("container.command 不是合法 JSON")?;
    let request_id = root
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("缺少 request_id"))?
        .parse::<Uuid>()
        .context("request_id 不是合法 UUID")?;
    let op = root
        .get("op")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|op| !op.is_empty())
        .ok_or_else(|| anyhow::anyhow!("缺少 op"))?
        .to_ascii_lowercase();
    if !CONTAINER_OPS.contains(&op.as_str()) {
        bail!("不支持的容器 op: {op}");
    }
    let args = root.get("args").cloned().unwrap_or_else(|| json!({}));
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    if op != "list" && name.is_none() {
        bail!("op={op} 需要 args.name");
    }
    if let Some(name) = name.as_deref() {
        validate_name(name)?;
    }
    let snapshot = args
        .get("snapshot")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|snapshot| !snapshot.is_empty())
        .map(str::to_owned);
    if op.starts_with("snapshot.") && op != "snapshot.list" && snapshot.is_none() {
        bail!("op={op} 需要 args.snapshot");
    }
    if let Some(snapshot) = snapshot.as_deref() {
        validate_name(snapshot)?;
    }
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    Ok(ContainerCommand {
        request_id,
        op,
        name,
        snapshot,
        force,
    })
}

/// 名称校验：`[A-Za-z0-9._-]{1,64}`。
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.chars().count() > CONTAINER_NAME_MAX_CHARS {
        bail!("容器名/快照名长度必须在 1..={CONTAINER_NAME_MAX_CHARS}");
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        bail!("容器名/快照名只允许字母、数字、. _ -");
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ContainerResult {
    pub request_id: Uuid,
    pub op: String,
    pub ok: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}

impl ContainerResult {
    fn success(command: &ContainerCommand, data: Value) -> Self {
        Self {
            request_id: command.request_id,
            op: command.op.clone(),
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    fn failure(command: &ContainerCommand, error: impl Into<String>) -> Self {
        Self {
            request_id: command.request_id,
            op: command.op.clone(),
            ok: false,
            data: None,
            error: Some(error.into()),
        }
    }
}

/// 解析 payload 并执行；解析失败返回 Err（无 request_id 可回传）。
pub async fn execute_payload(payload: &str) -> Result<ContainerResult> {
    let command = parse(payload)?;
    let result = match execute(&command).await {
        Ok(data) => ContainerResult::success(&command, data),
        Err(error) => ContainerResult::failure(&command, format!("{error:#}")),
    };
    Ok(result)
}

fn incus_bin() -> String {
    std::env::var("QE_INCUS_BIN")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "incus".to_owned())
}

async fn run_incus(args: &[String]) -> Result<String> {
    let bin = incus_bin();
    let output = tokio::time::timeout(
        CONTAINER_COMMAND_TIMEOUT,
        tokio::process::Command::new(&bin).args(args).output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("{bin} {} 超时", args.join(" ")))?
    .with_context(|| format!("执行 {bin} 失败（Incus 未安装？）"))?;
    if !output.status.success() {
        bail!(
            "{bin} {} 失败（{}）: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn arg(value: &str) -> String {
    value.to_owned()
}

async fn execute(command: &ContainerCommand) -> Result<Value> {
    let name = command.name.as_deref().unwrap_or_default();
    match command.op.as_str() {
        "list" => {
            let stdout = run_incus(&["list".into(), "--format".into(), "json".into()]).await?;
            let raw: Value = serde_json::from_str(&stdout).context("解析 incus list JSON 失败")?;
            let containers = raw
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            let ipv4 = item
                                .get("state")
                                .and_then(|state| state.get("network"))
                                .and_then(Value::as_object)
                                .map(|network| {
                                    network
                                        .values()
                                        .filter_map(|iface| iface.get("addresses"))
                                        .flat_map(|addresses| {
                                            addresses.as_array().cloned().unwrap_or_default()
                                        })
                                        .filter_map(|address| {
                                            address
                                                .get("family")
                                                .and_then(Value::as_str)
                                                .filter(|family| *family == "inet")
                                                .and_then(|_| {
                                                    address.get("address").and_then(Value::as_str)
                                                })
                                                .map(str::to_owned)
                                        })
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            json!({
                                "name": item.get("name").cloned().unwrap_or(Value::Null),
                                "status": item.get("status").cloned().unwrap_or(Value::Null),
                                "type": item.get("type").cloned().unwrap_or(Value::Null),
                                "ipv4": ipv4,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(json!({ "containers": containers }))
        }
        "start" => {
            run_incus(&["start".into(), arg(name)]).await?;
            Ok(json!({ "name": name, "result": "ok" }))
        }
        "stop" => {
            let mut args = vec!["stop".into(), arg(name)];
            if command.force {
                args.push("--force".into());
            }
            run_incus(&args).await?;
            Ok(json!({ "name": name, "result": "ok", "forced": command.force }))
        }
        "restart" => {
            run_incus(&["restart".into(), arg(name)]).await?;
            Ok(json!({ "name": name, "result": "ok" }))
        }
        "delete" => {
            let mut args = vec!["delete".into(), arg(name)];
            if command.force {
                args.push("--force".into());
            }
            run_incus(&args).await?;
            Ok(json!({ "name": name, "result": "ok", "forced": command.force }))
        }
        "snapshot.create" => {
            let snapshot = command.snapshot.as_deref().unwrap_or_default();
            run_incus(&["snapshot".into(), "create".into(), arg(name), arg(snapshot)]).await?;
            Ok(json!({ "name": name, "snapshot": snapshot }))
        }
        "snapshot.list" => {
            let stdout = run_incus(&[
                "snapshot".into(),
                "list".into(),
                arg(name),
                "--format".into(),
                "json".into(),
            ])
            .await?;
            let raw: Value =
                serde_json::from_str(&stdout).context("解析 incus snapshot list JSON 失败")?;
            let snapshots = raw
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            json!({
                                "name": item.get("name").cloned().unwrap_or(Value::Null),
                                "created_at": item.get("created_at").cloned().unwrap_or(Value::Null),
                                "stateful": item.get("stateful").cloned().unwrap_or(Value::Null),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(json!({ "name": name, "snapshots": snapshots }))
        }
        "snapshot.restore" => {
            let snapshot = command.snapshot.as_deref().unwrap_or_default();
            run_incus(&["snapshot".into(), "restore".into(), arg(name), arg(snapshot)]).await?;
            Ok(json!({ "name": name, "snapshot": snapshot }))
        }
        "snapshot.delete" => {
            let snapshot = command.snapshot.as_deref().unwrap_or_default();
            run_incus(&["snapshot".into(), "delete".into(), arg(name), arg(snapshot)]).await?;
            Ok(json!({ "name": name, "snapshot": snapshot }))
        }
        other => bail!("不支持的容器 op: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_id() -> Uuid {
        Uuid::parse_str("0b6f9d4e-4a41-4b6e-9d0b-4f8b0f3c2a11").unwrap()
    }

    #[test]
    fn parses_lifecycle_commands() {
        let command = parse(&format!(
            r#"{{"request_id":"{}","op":"start","args":{{"name":"debian12-cfg"}}}}"#,
            request_id()
        ))
        .unwrap();
        assert_eq!(command.op, "start");
        assert_eq!(command.name.as_deref(), Some("debian12-cfg"));
        let command = parse(&format!(
            r#"{{"request_id":"{}","op":"delete","args":{{"name":"c1","force":true}}}}"#,
            request_id()
        ))
        .unwrap();
        assert!(command.force);
    }

    #[test]
    fn parses_snapshot_commands() {
        let command = parse(&format!(
            r#"{{"request_id":"{}","op":"snapshot.restore","args":{{"name":"c1","snapshot":"s1"}}}}"#,
            request_id()
        ))
        .unwrap();
        assert_eq!(command.snapshot.as_deref(), Some("s1"));
        assert!(parse(&format!(
            r#"{{"request_id":"{}","op":"snapshot.create","args":{{"name":"c1"}}}}"#,
            request_id()
        ))
        .is_err());
    }

    #[test]
    fn rejects_bad_names_and_ops() {
        assert!(validate_name("../etc").is_err());
        assert!(validate_name("ok-1_a.b").is_ok());
        assert!(parse(&format!(
            r#"{{"request_id":"{}","op":"run","args":{{"name":"c1"}}}}"#,
            request_id()
        ))
        .is_err());
        assert!(parse(&format!(
            r#"{{"request_id":"{}","op":"start","args":{{}}}}"#,
            request_id()
        ))
        .is_err());
    }
}
