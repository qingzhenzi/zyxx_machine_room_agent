//! TOML loading for [`ServiceFile`], including path resolution and `${VAR}`
//! expansion.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{HealthCheck, ServiceFile, ServiceSpec};

impl ServiceFile {
    /// Parse and validate a TOML service file.
    ///
    /// Relative mount sources and `env_file` paths are resolved against
    /// `base_dir`, which should be the directory containing the file.
    pub fn from_toml_str(text: &str, base_dir: impl AsRef<Path>) -> Result<Self> {
        let base_dir = std::path::absolute(base_dir.as_ref()).map_err(|error| {
            Error::config(
                base_dir.as_ref(),
                format!("cannot resolve base directory: {error}"),
            )
        })?;
        let value: toml::Value =
            toml::from_str(text).map_err(|error| Error::config_at("<toml>", error.to_string()))?;
        validate_toml_keys(&value)?;

        let mut file: ServiceFile =
            toml::from_str(text).map_err(|error| Error::config_at("<toml>", error.to_string()))?;

        let env: HashMap<String, String> = std::env::vars().collect();
        file.expand_variables(&env)?;
        file.resolve_relative_paths(&base_dir);
        file.apply_implicit_network();
        file.validate()?;
        Ok(file)
    }

    /// Load a TOML service file from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = std::path::absolute(path.as_ref()).map_err(|error| {
            Error::config(path.as_ref(), format!("cannot resolve file path: {error}"))
        })?;
        let text = std::fs::read_to_string(&path)
            .map_err(|error| Error::config(&path, format!("cannot read file: {error}")))?;
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        Self::from_toml_str(&text, base_dir).map_err(|error| match error {
            Error::Config {
                path: location,
                message,
            } => Error::Config {
                path: if location.is_empty() {
                    format!(" at {}", path.display())
                } else {
                    format!(
                        " at {} ({})",
                        path.display(),
                        location.trim_start_matches(" at ")
                    )
                },
                message,
            },
            other => other,
        })
    }

    /// Render the file back to pretty TOML.
    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self)
            .map_err(|error| Error::Other(format!("cannot serialize service file: {error}")))
    }

    /// Resolve paths and fill in the implicit network reference.
    ///
    /// The loader already does this; this method is public so programmatically
    /// constructed files can opt in to the same behavior.
    pub fn resolve_paths(&mut self, base_dir: impl AsRef<Path>) {
        self.resolve_relative_paths(base_dir.as_ref());
    }

    fn resolve_relative_paths(&mut self, base_dir: &Path) {
        let container = &mut self.service.container;
        for mount in &mut container.mounts {
            if mount.source.is_relative() {
                mount.source = normalize_path(&base_dir.join(&mount.source));
            }
        }
        if let Some(env_file) = &mut container.env_file {
            if env_file.is_relative() {
                *env_file = normalize_path(&base_dir.join(&*env_file));
            }
        }
    }

    fn expand_variables(&mut self, env: &HashMap<String, String>) -> Result<()> {
        let container = &mut self.service.container;
        expand_value("service.image", &mut container.image, env)?;
        expand_list("service.command", &mut container.command, env)?;
        if let Some(entrypoint) = &mut container.entrypoint {
            expand_list("service.entrypoint", entrypoint, env)?;
        }
        expand_map("service.env", &mut container.env, env)?;
        if let Some(path) = &mut container.env_file {
            expand_path("service.env_file", path, env)?;
        }
        expand_option("service.workdir", &mut container.workdir, env)?;
        expand_option("service.user", &mut container.user, env)?;
        expand_option("service.hostname", &mut container.hostname, env)?;
        for mount in &mut container.mounts {
            expand_path("service.mounts.source", &mut mount.source, env)?;
            expand_value("service.mounts.target", &mut mount.target, env)?;
        }
        expand_option("service.network", &mut container.network, env)?;
        expand_option("service.stop_signal", &mut container.stop_signal, env)?;
        expand_map("service.labels", &mut container.labels, env)?;
        expand_option("service.platform", &mut container.platform, env)?;

        if let Some(network) = &mut self.network {
            expand_value("network.name", &mut network.name, env)?;
            expand_value("network.driver", &mut network.driver, env)?;
            expand_option("network.bridge_name", &mut network.bridge_name, env)?;
            expand_map("network.labels", &mut network.labels, env)?;
        }

        if let Some(healthcheck) = &mut self.service.healthcheck {
            match healthcheck {
                HealthCheck::Http { path, .. } => {
                    expand_value("service.healthcheck.path", path, env)?
                }
                HealthCheck::Exec { command, .. } => {
                    expand_list("service.healthcheck.command", command, env)?
                }
                HealthCheck::Tcp { .. } => {}
            }
        }

        Ok(())
    }
}

const TOP_LEVEL_KEYS: &[&str] = &["version", "network", "service"];
const NETWORK_KEYS: &[&str] = &[
    "name",
    "driver",
    "subnet",
    "gateway",
    "bridge_name",
    "labels",
    "internal",
    "attachable",
];
const SERVICE_KEYS: &[&str] = &[
    "name",
    "image",
    "command",
    "entrypoint",
    "env",
    "env_file",
    "workdir",
    "user",
    "hostname",
    "mounts",
    "ports",
    "network",
    "static_ip",
    "restart",
    "stop_signal",
    "stop_grace",
    "resources",
    "security",
    "labels",
    "log",
    "platform",
    "healthcheck",
];
const MOUNT_KEYS: &[&str] = &["source", "target", "read_only", "kind"];
const PORT_KEYS: &[&str] = &["container", "host", "host_ip", "protocol"];
const RESOURCE_KEYS: &[&str] = &["memory", "cpus", "pids"];
const SECURITY_KEYS: &[&str] = &[
    "read_only",
    "cap_drop",
    "cap_add",
    "no_new_privileges",
    "tmpfs",
    "ulimits",
];
const ULIMIT_KEYS: &[&str] = &["name", "soft", "hard"];
const LOG_KEYS: &[&str] = &["driver", "max_size", "max_file"];
const HEALTHCHECK_KEYS: &[&str] = &[
    "type",
    "port",
    "path",
    "expect_status",
    "interval",
    "timeout",
    "start_period",
    "retries",
    "command",
];

fn validate_toml_keys(value: &toml::Value) -> Result<()> {
    check_table(value, "", TOP_LEVEL_KEYS)?;
    let Some(root) = value.as_table() else {
        return Err(Error::config_at(
            "<toml>",
            "service file must be a TOML table",
        ));
    };

    if let Some(network) = root.get("network") {
        check_table(network, "network", NETWORK_KEYS)?;
    }

    let Some(service) = root.get("service") else {
        return Ok(());
    };
    check_table(service, "service", SERVICE_KEYS)?;
    let Some(service) = service.as_table() else {
        return Ok(());
    };

    if let Some(mounts) = service.get("mounts") {
        check_array_of_tables(mounts, "service.mounts", MOUNT_KEYS)?;
    }
    if let Some(ports) = service.get("ports") {
        check_array_of_tables(ports, "service.ports", PORT_KEYS)?;
    }
    if let Some(resources) = service.get("resources") {
        check_table(resources, "service.resources", RESOURCE_KEYS)?;
    }
    if let Some(security) = service.get("security") {
        check_table(security, "service.security", SECURITY_KEYS)?;
        if let Some(ulimits) = security.get("ulimits") {
            check_array_of_tables(ulimits, "service.security.ulimits", ULIMIT_KEYS)?;
        }
    }
    if let Some(log) = service.get("log") {
        check_table(log, "service.log", LOG_KEYS)?;
    }
    if let Some(healthcheck) = service.get("healthcheck") {
        check_table(healthcheck, "service.healthcheck", HEALTHCHECK_KEYS)?;
    }

    Ok(())
}

fn check_table(value: &toml::Value, path: &str, allowed: &[&str]) -> Result<()> {
    let Some(table) = value.as_table() else {
        return Ok(());
    };
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            let location = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            return Err(Error::config_at(location, format!("unknown field `{key}`")));
        }
    }
    Ok(())
}

fn check_array_of_tables(value: &toml::Value, path: &str, allowed: &[&str]) -> Result<()> {
    let Some(items) = value.as_array() else {
        return Ok(());
    };
    for (index, item) in items.iter().enumerate() {
        check_table(item, &format!("{path}[{index}]"), allowed)?;
    }
    Ok(())
}

impl ServiceSpec {
    /// Build a service spec from a TOML `[service]` table alone.
    pub fn from_toml_str(text: &str) -> Result<Self> {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            service: ServiceSpec,
        }
        let wrapper: Wrapper =
            toml::from_str(text).map_err(|error| Error::config_at("<toml>", error.to_string()))?;
        wrapper.service.validate()?;
        Ok(wrapper.service)
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn expand_list(field: &str, values: &mut [String], env: &HashMap<String, String>) -> Result<()> {
    for value in values {
        expand_value(field, value, env)?;
    }
    Ok(())
}

fn expand_map(
    field: &str,
    values: &mut std::collections::BTreeMap<String, String>,
    env: &HashMap<String, String>,
) -> Result<()> {
    let original = std::mem::take(values);
    for (mut key, mut value) in original {
        expand_value(field, &mut key, env)?;
        expand_value(field, &mut value, env)?;
        values.insert(key, value);
    }
    Ok(())
}

fn expand_option(
    field: &str,
    value: &mut Option<String>,
    env: &HashMap<String, String>,
) -> Result<()> {
    if let Some(value) = value {
        expand_value(field, value, env)?;
    }
    Ok(())
}

fn expand_path(field: &str, path: &mut PathBuf, env: &HashMap<String, String>) -> Result<()> {
    let mut text = path.to_string_lossy().into_owned();
    expand_value(field, &mut text, env)?;
    *path = PathBuf::from(text);
    Ok(())
}

fn expand_value(field: &str, value: &mut String, env: &HashMap<String, String>) -> Result<()> {
    *value = expand_string(field, value, env)?;
    Ok(())
}

fn expand_string(field: &str, input: &str, env: &HashMap<String, String>) -> Result<String> {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(Error::config_at(
                field,
                format!("unterminated variable expression in `{input}`"),
            ));
        };
        let expression = &after[..end];
        let (name, default) = match expression.split_once(":-") {
            Some((name, default)) => (name.trim(), Some(default)),
            None => (expression.trim(), None),
        };
        if name.is_empty() {
            return Err(Error::config_at(
                field,
                format!("empty variable name in `{input}`"),
            ));
        }

        match (env.get(name), default) {
            (Some(value), _) if !value.is_empty() => output.push_str(value),
            (Some(_), Some(default)) | (None, Some(default)) => output.push_str(default),
            (Some(_), None) => {}
            (None, None) => {
                return Err(Error::config_at(
                    field,
                    format!("environment variable `{name}` is not set and has no default"),
                ));
            }
        }

        rest = &after[end + 1..];
    }

    output.push_str(rest);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn expands_variables_and_defaults() {
        let env = env(&[("IMAGE", "python:3.12-slim"), ("PORT", "8000")]);
        assert_eq!(
            expand_string("field", "${IMAGE}", &env).unwrap(),
            "python:3.12-slim"
        );
        assert_eq!(
            expand_string("field", "${MISSING:-fallback}", &env).unwrap(),
            "fallback"
        );
        assert_eq!(
            expand_string("field", "port=${PORT}", &env).unwrap(),
            "port=8000"
        );
        assert!(expand_string("field", "${MISSING}", &env).is_err());
    }

    #[test]
    fn leaves_plain_dollars_alone() {
        let env = HashMap::new();
        assert_eq!(
            expand_string("field", "pa$$word $HOME", &env).unwrap(),
            "pa$$word $HOME"
        );
    }

    #[test]
    fn parses_and_resolves_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("app");
        std::fs::create_dir(&app).unwrap();
        let text = r#"
version = 1

[network]
name = "docker1"
subnet = "172.30.0.0/16"
gateway = "172.30.0.1"
bridge_name = "docker1"

[service]
name = "py-api"
image = "python:3.12-slim"
command = ["python", "-m", "http.server", "8000"]
mounts = [{ source = "./app", target = "/workspace", read_only = true }]
ports = [{ container = 8000, host = 18000 }]
log = { max_size = "1m", max_file = 2 }
network = "docker1"
static_ip = "172.30.0.10"
healthcheck = { type = "http", port = 8000, path = "/health", interval = "1s", retries = 3 }
"#;
        let file = ServiceFile::from_toml_str(text, dir.path()).unwrap();
        assert_eq!(file.version, 1);
        assert_eq!(file.service.container.name, "py-api");
        assert_eq!(file.service.container.mounts[0].source, app);
        assert_eq!(file.service.container.network.as_deref(), Some("docker1"));
        assert!(file.validate().is_ok());
    }

    #[test]
    fn rejects_unknown_fields() {
        let text = r#"
version = 1
[service]
name = "x"
image = "alpine"
unknown_field = 1
"#;
        let error = ServiceFile::from_toml_str(text, ".").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }
}
