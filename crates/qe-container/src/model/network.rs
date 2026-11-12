//! Network specifications and planning helpers.

use std::collections::BTreeMap;
use std::net::IpAddr;

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::units::{validate_bridge_name, validate_name};

fn default_driver() -> String {
    "bridge".to_string()
}

/// Declarative description of a Docker bridge network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    /// Network name.
    pub name: String,
    /// Driver. Only `bridge` is supported by the planner in v1.
    #[serde(default = "default_driver")]
    pub driver: String,
    /// IPv4 subnet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<IpNet>,
    /// IPv4 gateway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<IpAddr>,
    /// Linux bridge interface name (`com.docker.network.bridge.name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_name: Option<String>,
    /// Network labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Restrict external access.
    #[serde(default)]
    pub internal: bool,
    /// Allow manual container attachment (swarm).
    #[serde(default)]
    pub attachable: bool,
}

impl NetworkSpec {
    /// Create a bridge network with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            driver: default_driver(),
            subnet: None,
            gateway: None,
            bridge_name: None,
            labels: BTreeMap::new(),
            internal: false,
            attachable: false,
        }
    }

    /// Set the subnet.
    pub fn with_subnet(mut self, subnet: IpNet) -> Self {
        self.subnet = Some(subnet);
        self
    }

    /// Set the gateway.
    pub fn with_gateway(mut self, gateway: IpAddr) -> Self {
        self.gateway = Some(gateway);
        self
    }

    /// Set the Linux bridge interface name.
    pub fn with_bridge_name(mut self, name: impl Into<String>) -> Self {
        self.bridge_name = Some(name.into());
        self
    }

    /// Set the driver.
    pub fn with_driver(mut self, driver: impl Into<String>) -> Self {
        self.driver = driver.into();
        self
    }

    /// Validate the spec.
    pub fn validate(&self) -> Result<()> {
        validate_name("network", &self.name)?;
        if self.driver != "bridge" {
            return Err(Error::InvalidSpec(format!(
                "network driver `{}` is not supported; v1 only manages bridge networks",
                self.driver
            )));
        }
        if let Some(subnet) = self.subnet {
            if !matches!(subnet, IpNet::V4(_)) {
                return Err(Error::InvalidSpec(
                    "v1 only supports IPv4 bridge subnets".to_string(),
                ));
            }
        }
        if let Some(bridge_name) = &self.bridge_name {
            validate_bridge_name(bridge_name)?;
        }
        match (self.subnet, self.gateway) {
            (Some(subnet), Some(gateway)) => {
                if !subnet.contains(&gateway) {
                    return Err(Error::InvalidSpec(format!(
                        "gateway {gateway} is outside subnet {subnet}"
                    )));
                }
                if gateway == subnet.network() || gateway == subnet.broadcast() {
                    return Err(Error::InvalidSpec(format!(
                        "gateway {gateway} must be a usable host address in {subnet}"
                    )));
                }
            }
            (None, Some(gateway)) => {
                return Err(Error::InvalidSpec(format!(
                    "gateway {gateway} requires a subnet"
                )));
            }
            _ => {}
        }
        for key in self.labels.keys() {
            if key.is_empty() {
                return Err(Error::InvalidSpec(
                    "network label key must not be empty".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Validate a static container address against this network.
    pub fn validate_static_ip(&self, ip: IpAddr) -> Result<()> {
        if !ip.is_ipv4() {
            return Err(Error::InvalidSpec(format!(
                "static_ip {ip} must be IPv4 for a v1 bridge network"
            )));
        }
        let Some(subnet) = self.subnet else {
            return Err(Error::InvalidSpec(format!(
                "static_ip {ip} requires network `{}` to declare a subnet",
                self.name
            )));
        };
        if !subnet.contains(&ip) {
            return Err(Error::InvalidSpec(format!(
                "static_ip {ip} is outside network subnet {subnet}"
            )));
        }
        if ip == subnet.network() || ip == subnet.broadcast() {
            return Err(Error::InvalidSpec(format!(
                "static_ip {ip} must be a usable host address"
            )));
        }
        if self.gateway == Some(ip) {
            return Err(Error::InvalidSpec(format!(
                "static_ip {ip} conflicts with the network gateway"
            )));
        }
        Ok(())
    }

    /// Compare this desired spec with an existing network.
    pub fn plan(&self, existing: Option<&NetworkInfo>) -> NetworkPlan {
        let Some(existing) = existing else {
            return NetworkPlan {
                spec: self.clone(),
                existing: None,
                action: NetworkAction::Create,
                reasons: vec!["network does not exist".to_string()],
            };
        };

        let mut reasons = Vec::new();
        if self.driver != existing.driver {
            reasons.push(format!(
                "driver differs (desired `{}`, existing `{}`)",
                self.driver, existing.driver
            ));
        }
        if self.subnet != existing.subnet {
            reasons.push(format!(
                "subnet differs (desired {:?}, existing {:?})",
                self.subnet, existing.subnet
            ));
        }
        if self.gateway != existing.gateway {
            reasons.push(format!(
                "gateway differs (desired {:?}, existing {:?})",
                self.gateway, existing.gateway
            ));
        }
        if self.bridge_name != existing.bridge_name {
            reasons.push(format!(
                "bridge name differs (desired {:?}, existing {:?})",
                self.bridge_name, existing.bridge_name
            ));
        }

        let action = if reasons.is_empty() {
            NetworkAction::Reuse
        } else {
            NetworkAction::Conflict
        };

        NetworkPlan {
            spec: self.clone(),
            existing: Some(existing.clone()),
            action,
            reasons,
        }
    }
}

/// What a [`NetworkPlan`] proposes to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAction {
    /// Network does not exist.
    Create,
    /// Network already matches.
    Reuse,
    /// Network exists but differs; requires an explicit recreate.
    Conflict,
    /// Network should be recreated.
    Recreate,
}

/// A network as observed on the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInfo {
    /// Network id.
    pub id: String,
    /// Network name.
    pub name: String,
    /// Driver.
    pub driver: String,
    /// Subnet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<IpNet>,
    /// Gateway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<IpAddr>,
    /// Linux bridge interface name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_name: Option<String>,
    /// Labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Options.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, String>,
    /// Attached containers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub containers: Vec<NetworkContainer>,
}

/// A container attached to a network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkContainer {
    /// Container id.
    pub id: String,
    /// Container name.
    pub name: String,
    /// IPv4 address on this network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv4: Option<String>,
}

/// Result of comparing a [`NetworkSpec`] with the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPlan {
    /// Desired spec.
    pub spec: NetworkSpec,
    /// Existing network, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing: Option<NetworkInfo>,
    /// Proposed action.
    pub action: NetworkAction,
    /// Human-readable reasons.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

impl NetworkPlan {
    /// Whether applying this plan needs an explicit destructive decision.
    pub fn requires_confirmation(&self) -> bool {
        self.action == NetworkAction::Conflict
    }

    /// Containers currently attached to the existing network.
    pub fn attached(&self) -> &[NetworkContainer] {
        self.existing
            .as_ref()
            .map(|network| network.containers.as_slice())
            .unwrap_or(&[])
    }
}
