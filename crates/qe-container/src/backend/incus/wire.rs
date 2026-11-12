use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// API wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(super) struct Envelope {
    #[serde(rename = "type")]
    pub(super) kind: String,
    #[serde(default)]
    pub(super) error: Option<String>,
    #[serde(default)]
    pub(super) error_code: Option<i64>,
    #[serde(default)]
    pub(super) metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Operation {
    #[serde(default)]
    pub(super) id: String,
    #[serde(default)]
    pub(super) status: String,
    #[serde(default)]
    pub(super) err: Option<String>,
    #[serde(default)]
    pub(super) metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct InstanceEntry {
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) status_code: Option<i64>,
    #[serde(default)]
    pub(super) created_at: Option<String>,
    #[serde(default)]
    pub(super) last_used_at: Option<String>,
    #[serde(default)]
    pub(super) config: BTreeMap<String, String>,
    #[serde(default)]
    pub(super) devices: BTreeMap<String, Value>,
    #[serde(default)]
    pub(super) expanded_config: BTreeMap<String, String>,
    #[serde(default)]
    pub(super) expanded_devices: BTreeMap<String, Value>,
    #[serde(default)]
    pub(super) state: Option<InstanceState>,
}

impl InstanceEntry {
    pub(super) fn effective_config(&self) -> &BTreeMap<String, String> {
        if !self.expanded_config.is_empty() {
            &self.expanded_config
        } else {
            &self.config
        }
    }

    pub(super) fn effective_devices(&self) -> &BTreeMap<String, Value> {
        if !self.expanded_devices.is_empty() {
            &self.expanded_devices
        } else {
            &self.devices
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct InstanceState {
    #[serde(default)]
    pub(super) status_code: Option<i64>,
    #[serde(default)]
    pub(super) network: Option<BTreeMap<String, IfaceState>>,
    #[serde(default)]
    pub(super) cpu: Option<CpuState>,
    #[serde(default)]
    pub(super) memory: Option<MemState>,
    #[serde(default)]
    pub(super) processes: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct IfaceState {
    #[serde(default)]
    pub(super) counters: Option<Counters>,
    #[serde(default)]
    pub(super) addresses: Vec<Addr>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct Counters {
    #[serde(default)]
    pub(super) bytes_received: u64,
    #[serde(default)]
    pub(super) bytes_sent: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct Addr {
    #[serde(default)]
    pub(super) address: String,
    #[serde(default)]
    pub(super) family: String,
    #[serde(default)]
    pub(super) scope: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct CpuState {
    #[serde(default)]
    pub(super) usage: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct MemState {
    #[serde(default)]
    pub(super) usage: Option<u64>,
    #[serde(default)]
    pub(super) total: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ImageEntry {
    #[serde(default)]
    pub(super) fingerprint: String,
    #[serde(default)]
    pub(super) size: Option<u64>,
    #[serde(default)]
    pub(super) created_at: Option<String>,
    #[serde(default)]
    pub(super) uploaded_at: Option<String>,
    #[serde(default)]
    pub(super) aliases: Vec<AliasEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct AliasEntry {
    #[serde(default)]
    pub(super) name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct NetworkEntry {
    #[serde(default)]
    pub(super) name: String,
    #[serde(rename = "type", default)]
    pub(super) kind: String,
    #[serde(default)]
    pub(super) config: BTreeMap<String, String>,
    #[serde(default)]
    pub(super) managed: bool,
    #[serde(default)]
    pub(super) used_by: Vec<String>,
}

