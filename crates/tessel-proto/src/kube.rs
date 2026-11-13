//! kubernetes, as the kubernetes module shows it: a cluster's objects of a
//! few kinds, as rows small enough for a pane. the daemon asks `kubectl`
//! (so whatever the person's kubeconfig and auth plugins do, it does too).

use serde::{Deserialize, Serialize};

/// the kinds the module lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KubeKind {
    Pods,
    Deployments,
    Services,
    Nodes,
    #[serde(other)]
    Unknown,
}

impl KubeKind {
    pub const ALL: [KubeKind; 4] = [KubeKind::Pods, KubeKind::Deployments, KubeKind::Services, KubeKind::Nodes];

    /// what `kubectl get` calls it.
    pub fn resource(self) -> &'static str {
        match self {
            KubeKind::Pods => "pods",
            KubeKind::Deployments => "deployments",
            KubeKind::Services => "services",
            KubeKind::Nodes => "nodes",
            KubeKind::Unknown => "pods",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            KubeKind::Pods => "Pods",
            KubeKind::Deployments => "Deployments",
            KubeKind::Services => "Services",
            KubeKind::Nodes => "Nodes",
            KubeKind::Unknown => "Pods",
        }
    }

    /// whether its objects live in a namespace.
    pub fn namespaced(self) -> bool {
        self != KubeKind::Nodes
    }
}

/// how an object is doing, for its dot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KubeHealth {
    /// running and ready.
    Ok,
    /// on its way: starting, rolling out, not all ready yet.
    Pending,
    /// failing: crash loops, failed pulls, failed pods, unready nodes.
    Bad,
    /// done or idle: completed jobs' pods, deployments scaled to zero.
    #[default]
    #[serde(other)]
    Quiet,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KubeContainer {
    pub name: String,
    pub image: String,
    pub ready: bool,
    pub restarts: u32,
    /// running, waiting (with why), terminated (with why).
    pub state: String,
}

/// one row: a pod, a deployment, a service or a node.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KubeObject {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// what kubectl's STATUS column would say: Running, CrashLoopBackOff, 2/3 available…
    pub status: String,
    pub health: KubeHealth,
    /// ready of how many: containers for a pod, replicas for a deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restarts: Option<u32>,
    /// from the metrics server, when the cluster has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_milli: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<u64>,
    /// a few facts for when it's opened: node, IP, images, ports, type…
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub containers: Vec<KubeContainer>,
}

/// what can be done to an object from the module (logs and a shell are terminals).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KubeAct {
    /// a pod: delete it (its owner makes a new one).
    Delete,
    /// a deployment: roll its pods.
    Restart,
    #[serde(other)]
    Unknown,
}

/// a cpu amount as kubernetes writes it (`250m`, `1`, `1500000n`), in millicores.
pub fn cpu_milli(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix('n') {
        return n.parse::<f64>().ok().map(|v| (v / 1_000_000.0).round() as u64);
    }
    if let Some(u) = s.strip_suffix('u') {
        return u.parse::<f64>().ok().map(|v| (v / 1000.0).round() as u64);
    }
    if let Some(m) = s.strip_suffix('m') {
        return m.parse::<f64>().ok().map(|v| v.round() as u64);
    }
    s.parse::<f64>().ok().map(|v| (v * 1000.0).round() as u64)
}

/// a memory amount as kubernetes writes it (`128Mi`, `1Gi`, `500M`, `1024`), in bytes.
pub fn mem_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    const UNITS: [(&str, f64); 11] = [
        ("Ki", 1024.0),
        ("Mi", 1048576.0),
        ("Gi", 1073741824.0),
        ("Ti", 1099511627776.0),
        ("Pi", 1125899906842624.0),
        ("k", 1e3),
        ("K", 1e3),
        ("M", 1e6),
        ("G", 1e9),
        ("T", 1e12),
        ("P", 1e15),
    ];
    for (suffix, mult) in UNITS {
        if let Some(n) = s.strip_suffix(suffix) {
            return n.parse::<f64>().ok().map(|v| (v * mult).round() as u64);
        }
    }
    s.parse::<f64>().ok().map(|v| v.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_read_as_kubernetes_writes_them() {
        assert_eq!(cpu_milli("250m"), Some(250));
        assert_eq!(cpu_milli("2"), Some(2000));
        assert_eq!(cpu_milli("1500000n"), Some(2));
        assert_eq!(mem_bytes("128Mi"), Some(134_217_728));
        assert_eq!(mem_bytes("1G"), Some(1_000_000_000));
        assert_eq!(mem_bytes("4096"), Some(4096));
        assert_eq!(mem_bytes("lots"), None);
    }

    #[test]
    fn a_kind_from_a_newer_client_is_read_as_unknown() {
        assert_eq!(serde_json::from_str::<KubeKind>("\"ingresses\"").unwrap(), KubeKind::Unknown);
        assert_eq!(serde_json::from_str::<KubeKind>("\"pods\"").unwrap(), KubeKind::Pods);
    }
}
