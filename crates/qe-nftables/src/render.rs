//! 把 [`FirewallSpec`] 渲染成 `nft` 脚本文本。

use crate::spec::{FirewallSpec, PortRule};
use crate::{Error, TABLE};

/// 渲染完整脚本：整表删除重建，保证幂等。
pub fn render_script(spec: &FirewallSpec) -> Result<String, Error> {
    let mut script = String::new();
    script.push_str("#!nft\n");
    script.push_str(&format!("destroy table inet {TABLE}\n"));

    if !spec.enabled {
        // 关闭 = 只清空专用表，不写任何规则。
        return Ok(script);
    }

    script.push_str(&format!("table inet {TABLE} {{\n"));

    if !spec.blocked_ports.is_empty() {
        script.push_str("  chain block {\n");
        script.push_str("    type filter hook input priority filter; policy accept;\n");
        for rule in &spec.blocked_ports {
            validate(rule)?;
            script.push_str(&format!(
                "    {} dport {} drop\n",
                rule.protocol.as_str(),
                rule.port
            ));
        }
        script.push_str("  }\n");
    }

    script.push_str("  chain input {\n");
    script.push_str("    type filter hook input priority filter - 10; policy accept;\n");
    script.push_str("    ct state established,related accept\n");
    for rule in spec.effective_ports() {
        validate(&rule)?;
        let source = rule
            .from
            .as_ref()
            .map(|cidr| format!("ip saddr {cidr} "))
            .unwrap_or_default();
        script.push_str(&format!(
            "    {source}{} dport {} accept\n",
            rule.protocol.as_str(),
            rule.port
        ));
    }
    script.push_str("  }\n");
    script.push_str("}\n");
    Ok(script)
}

fn validate(rule: &PortRule) -> Result<(), Error> {
    if rule.port == 0 {
        return Err(Error::InvalidPort(rule.port));
    }
    if let Some(cidr) = &rule.from
        && !cidr.contains('/')
        && cidr.parse::<std::net::Ipv4Addr>().is_err()
    {
        return Err(Error::InvalidCidr(cidr.clone()));
    }
    Ok(())
}
