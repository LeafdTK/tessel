//! kubernetes for the kubernetes module, through the person's own `kubectl`:
//! their kubeconfig, their contexts, their auth plugins. nothing here talks
//! to a cluster itself.
//!
//! a daemon started from the dock has almost nothing on PATH, and kubectl's
//! auth plugins (gke-gcloud-auth-plugin, kubelogin…) need to be found too, so
//! kubectl runs with the PATH a login shell would have.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use tessel_proto::kube::{KubeAct, KubeContainer, KubeHealth, KubeKind, KubeObject, cpu_milli, mem_bytes};

/// kubectl, and the PATH to run it with. found once.
fn kubectl() -> Option<&'static (PathBuf, String)> {
    static FOUND: OnceLock<Option<(PathBuf, String)>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let ours = std::env::var("PATH").unwrap_or_default();
            if let Some(bin) = find("kubectl", &ours) {
                return Some((bin, ours));
            }
            let theirs = login_path()?;
            find("kubectl", &theirs).map(|bin| (bin, theirs))
        })
        .as_ref()
}

fn find(name: &str, path: &str) -> Option<PathBuf> {
    path.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join(name)).find(|p| p.is_file())
}

/// PATH as a login shell sets it. a shell that hangs isn't waited on for long.
fn login_path() -> Option<String> {
    const MARK: &str = "__tessel_path__";
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut child = std::process::Command::new(shell)
        .args(["-ilc", &format!("printf '{MARK}%s' \"$PATH\"")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut buf);
        let _ = tx.send(buf);
    });
    let got = rx.recv_timeout(Duration::from_secs(8));
    let _ = child.kill();
    let _ = child.wait();
    Some(got.ok()?.rsplit_once(MARK)?.1.trim().to_owned())
}

/// run kubectl; what it printed, or why it failed in its own words.
async fn run(args: &[String]) -> Result<Vec<u8>, String> {
    // the first look may ask a login shell, which is slow: not on the runtime's threads.
    let Some((bin, path)) = tokio::task::spawn_blocking(kubectl).await.ok().flatten() else {
        return Err("kubectl isn't installed (or isn't on your PATH)".into());
    };
    let child = tokio::process::Command::new(bin)
        .args(args)
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("running kubectl: {e}"))?;
    let out = match tokio::time::timeout(Duration::from_secs(20), child.wait_with_output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("running kubectl: {e}")),
        Err(_) => return Err("kubectl didn't answer in 20s: is the cluster reachable?".into()),
    };
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        let said = said.lines().filter(|l| !l.trim().is_empty()).take(3).collect::<Vec<_>>().join("\n");
        return Err(if said.is_empty() { format!("kubectl failed ({})", out.status) } else { said });
    }
    Ok(out.stdout)
}

fn with_context(context: &Option<String>, rest: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = context.iter().flat_map(|c| ["--context".to_owned(), c.clone()]).collect();
    args.extend(rest.iter().map(|s| (*s).to_owned()));
    args
}

pub async fn contexts() -> Result<(Option<String>, Vec<String>), String> {
    let names = run(&with_context(&None, &["config", "get-contexts", "-o", "name"])).await?;
    let contexts = String::from_utf8_lossy(&names).lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect();
    let current = run(&with_context(&None, &["config", "current-context"])).await.ok().map(|c| String::from_utf8_lossy(&c).trim().to_owned()).filter(|c| !c.is_empty());
    Ok((current, contexts))
}

pub async fn namespaces(context: Option<String>) -> Result<Vec<String>, String> {
    let out = run(&with_context(&context, &["get", "namespaces", "-o", "jsonpath={.items[*].metadata.name}", "--request-timeout=10s"])).await?;
    let mut names: Vec<String> = String::from_utf8_lossy(&out).split_whitespace().map(str::to_owned).collect();
    names.sort();
    Ok(names)
}

/// a kind's objects, with cpu and memory when the cluster can say.
pub async fn list(context: Option<String>, namespace: Option<String>, kind: KubeKind) -> Result<(Vec<KubeObject>, bool), String> {
    let scope: Vec<String> = match (&namespace, kind.namespaced()) {
        (Some(ns), true) => vec!["-n".into(), ns.clone()],
        (None, true) => vec!["-A".into()],
        (_, false) => vec![],
    };
    let mut get = with_context(&context, &["get", kind.resource(), "-o", "json", "--request-timeout=10s"]);
    get.extend(scope.iter().cloned());
    let top = matches!(kind, KubeKind::Pods | KubeKind::Nodes).then(|| {
        let mut top = with_context(&context, &["top", kind.resource(), "--no-headers", "--request-timeout=5s"]);
        top.extend(scope.iter().cloned());
        top
    });
    let (listed, usage) = tokio::join!(run(&get), async {
        match &top {
            Some(args) => run(args).await.ok(),
            None => None,
        }
    });
    let json: serde_json::Value = serde_json::from_slice(&listed?).map_err(|e| format!("kubectl said something unreadable: {e}"))?;
    let mut objects = parse(kind, &json);
    let metrics = usage.is_some();
    if let Some(usage) = usage {
        apply_usage(&mut objects, &String::from_utf8_lossy(&usage), kind == KubeKind::Pods && namespace.is_none());
    }
    objects.sort_by(|a, b| a.namespace.cmp(&b.namespace).then_with(|| a.name.cmp(&b.name)));
    Ok((objects, metrics))
}

pub async fn act(context: Option<String>, namespace: String, kind: KubeKind, name: String, act: KubeAct) -> Result<String, String> {
    let args = match (act, kind) {
        (KubeAct::Delete, KubeKind::Pods) => with_context(&context, &["delete", "pod", &name, "-n", &namespace, "--wait=false"]),
        (KubeAct::Restart, KubeKind::Deployments) => with_context(&context, &["rollout", "restart", &format!("deployment/{name}"), "-n", &namespace]),
        _ => return Err(format!("{act:?} isn't something to do to {}", kind.resource())),
    };
    let out = run(&args).await?;
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

/// `kubectl top`'s lines onto the objects they're about.
fn apply_usage(objects: &mut [KubeObject], top: &str, with_namespace: bool) {
    for line in top.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (ns, name, rest) = match (with_namespace, cols.as_slice()) {
            (true, [ns, name, rest @ ..]) => (Some(*ns), *name, rest),
            (false, [name, rest @ ..]) => (None, *name, rest),
            _ => continue,
        };
        // pods: CPU MEMORY. nodes: CPU CPU% MEMORY MEMORY%.
        let (cpu, mem) = match rest {
            [cpu, mem] => (*cpu, *mem),
            [cpu, _, mem, _] => (*cpu, *mem),
            _ => continue,
        };
        if let Some(o) = objects.iter_mut().find(|o| o.name == name && (ns.is_none() || o.namespace.as_deref() == ns)) {
            o.cpu_milli = cpu_milli(cpu);
            o.mem_bytes = mem_bytes(mem);
        }
    }
}

/// `kubectl get -o json`'s items as rows.
pub fn parse(kind: KubeKind, json: &serde_json::Value) -> Vec<KubeObject> {
    let items = json["items"].as_array().cloned().unwrap_or_default();
    items
        .iter()
        .map(|i| {
            let meta = &i["metadata"];
            let mut o = KubeObject {
                name: meta["name"].as_str().unwrap_or("").to_owned(),
                namespace: meta["namespace"].as_str().map(str::to_owned),
                created_ms: meta["creationTimestamp"].as_str().and_then(rfc3339_ms),
                ..KubeObject::default()
            };
            match kind {
                KubeKind::Pods => pod(i, &mut o),
                KubeKind::Deployments => deployment(i, &mut o),
                KubeKind::Services => service(i, &mut o),
                KubeKind::Nodes => node(i, &mut o),
                KubeKind::Unknown => {}
            }
            o
        })
        .collect()
}

fn s(v: &serde_json::Value) -> &str {
    v.as_str().unwrap_or("")
}

/// a pod's status as `kubectl get pods` works it out.
fn pod(i: &serde_json::Value, o: &mut KubeObject) {
    let status = &i["status"];
    let phase = s(&status["phase"]);
    let containers: Vec<&serde_json::Value> = status["containerStatuses"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    let total = i["spec"]["containers"].as_array().map_or(containers.len(), Vec::len);
    let ready = containers.iter().filter(|c| c["ready"].as_bool() == Some(true)).count();
    o.ready = Some(format!("{ready}/{total}"));
    o.restarts = Some(containers.iter().map(|c| c["restartCount"].as_u64().unwrap_or(0) as u32).sum());

    // a container's own trouble says more than the pod's phase.
    fn waiting<'a>(c: &&'a serde_json::Value) -> Option<&'a str> {
        c["state"]["waiting"]["reason"].as_str().filter(|r| !r.is_empty() && *r != "ContainerCreating" && *r != "PodInitializing")
    }
    fn failed<'a>(c: &&'a serde_json::Value) -> Option<&'a str> {
        c["state"]["terminated"]["reason"].as_str().filter(|_| c["state"]["terminated"]["exitCode"].as_i64().unwrap_or(0) != 0)
    }
    let init_waiting = status["initContainerStatuses"].as_array().and_then(|a| a.iter().find(|c| c["state"]["terminated"]["exitCode"].as_i64() != Some(0)).map(|c| c["state"]["waiting"]["reason"].as_str().unwrap_or("Running").to_owned()));
    let (text, health) = if !i["metadata"]["deletionTimestamp"].is_null() {
        ("Terminating".to_owned(), KubeHealth::Pending)
    } else if let Some(r) = containers.iter().find_map(waiting) {
        let bad = matches!(r, "CrashLoopBackOff" | "ImagePullBackOff" | "ErrImagePull" | "CreateContainerConfigError" | "CreateContainerError" | "InvalidImageName" | "RunContainerError");
        (r.to_owned(), if bad { KubeHealth::Bad } else { KubeHealth::Pending })
    } else if let (Some(r), "Running") = (containers.iter().find_map(failed), phase) {
        (r.to_owned(), KubeHealth::Bad)
    } else if let (Some(w), "Pending") = (&init_waiting, phase) {
        (format!("Init:{w}"), KubeHealth::Pending)
    } else {
        match phase {
            "Running" if ready == total => ("Running".into(), KubeHealth::Ok),
            "Running" => ("Running".into(), KubeHealth::Pending),
            "Succeeded" => ("Completed".into(), KubeHealth::Quiet),
            "Failed" => (status["reason"].as_str().unwrap_or("Failed").to_owned(), KubeHealth::Bad),
            "" => ("Unknown".into(), KubeHealth::Pending),
            other => (other.to_owned(), KubeHealth::Pending),
        }
    };
    (o.status, o.health) = (text, health);

    let spec = &i["spec"];
    for (k, v) in [("Node", s(&spec["nodeName"])), ("IP", s(&status["podIP"])), ("QoS", s(&status["qosClass"]))] {
        if !v.is_empty() {
            o.facts.push((k.into(), v.into()));
        }
    }
    if let Some(owner) = i["metadata"]["ownerReferences"].as_array().and_then(|a| a.first()) {
        o.facts.push(("Owner".into(), format!("{} {}", s(&owner["kind"]), s(&owner["name"]))));
    }
    let images: std::collections::HashMap<&str, &str> = spec["containers"].as_array().map(|a| a.iter().map(|c| (s(&c["name"]), s(&c["image"]))).collect()).unwrap_or_default();
    o.containers = spec["containers"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| {
                    let name = s(&c["name"]);
                    let st = containers.iter().find(|x| s(&x["name"]) == name);
                    let state = st.map_or_else(
                        || "waiting".to_owned(),
                        |x| {
                            let state = &x["state"];
                            if let Some(w) = state["waiting"].as_object() {
                                format!("waiting: {}", w.get("reason").and_then(|r| r.as_str()).unwrap_or("…"))
                            } else if let Some(t) = state["terminated"].as_object() {
                                format!("terminated: {}", t.get("reason").and_then(|r| r.as_str()).unwrap_or("…"))
                            } else {
                                "running".to_owned()
                            }
                        },
                    );
                    KubeContainer {
                        name: name.to_owned(),
                        image: images.get(name).copied().unwrap_or("").to_owned(),
                        ready: st.is_some_and(|x| x["ready"].as_bool() == Some(true)),
                        restarts: st.map_or(0, |x| x["restartCount"].as_u64().unwrap_or(0) as u32),
                        state,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
}

fn deployment(i: &serde_json::Value, o: &mut KubeObject) {
    let want = i["spec"]["replicas"].as_u64().unwrap_or(1);
    let st = &i["status"];
    let ready = st["readyReplicas"].as_u64().unwrap_or(0);
    let updated = st["updatedReplicas"].as_u64().unwrap_or(0);
    o.ready = Some(format!("{ready}/{want}"));
    let stuck = st["conditions"].as_array().is_some_and(|c| c.iter().any(|c| s(&c["type"]) == "Progressing" && s(&c["status"]) == "False"));
    (o.status, o.health) = if want == 0 {
        ("Scaled to 0".into(), KubeHealth::Quiet)
    } else if stuck {
        ("Stuck rolling out".into(), KubeHealth::Bad)
    } else if ready >= want && updated >= want {
        ("Available".into(), KubeHealth::Ok)
    } else if ready == 0 {
        ("Unavailable".into(), KubeHealth::Bad)
    } else {
        ("Rolling out".into(), KubeHealth::Pending)
    };
    let images: Vec<&str> = i["spec"]["template"]["spec"]["containers"].as_array().map(|a| a.iter().map(|c| s(&c["image"])).collect()).unwrap_or_default();
    if !images.is_empty() {
        o.facts.push(("Images".into(), images.join(", ")));
    }
    o.facts.push(("Up to date".into(), format!("{updated} of {want}")));
    let strategy = s(&i["spec"]["strategy"]["type"]);
    if !strategy.is_empty() {
        o.facts.push(("Strategy".into(), strategy.into()));
    }
}

fn service(i: &serde_json::Value, o: &mut KubeObject) {
    let spec = &i["spec"];
    let kind = s(&spec["type"]);
    let ingress: Vec<String> = i["status"]["loadBalancer"]["ingress"]
        .as_array()
        .map(|a| a.iter().map(|x| x["ip"].as_str().or(x["hostname"].as_str()).unwrap_or("").to_owned()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    (o.status, o.health) = match kind {
        "LoadBalancer" if ingress.is_empty() => ("LoadBalancer (pending)".into(), KubeHealth::Pending),
        "" => ("ClusterIP".into(), KubeHealth::Ok),
        k => (k.to_owned(), KubeHealth::Ok),
    };
    let ports: Vec<String> = spec["ports"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|p| {
                    let port = p["port"].as_u64().unwrap_or(0);
                    let target = match &p["targetPort"] {
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::String(t) => t.clone(),
                        _ => String::new(),
                    };
                    let node = p["nodePort"].as_u64().map(|n| format!(" (node {n})")).unwrap_or_default();
                    let proto = s(&p["protocol"]);
                    let proto = if proto == "TCP" || proto.is_empty() { String::new() } else { format!("/{proto}") };
                    if target.is_empty() || target == port.to_string() { format!("{port}{proto}{node}") } else { format!("{port}→{target}{proto}{node}") }
                })
                .collect()
        })
        .unwrap_or_default();
    o.ready = (!ports.is_empty()).then(|| ports.join(", "));
    let ip = s(&spec["clusterIP"]);
    if !ip.is_empty() {
        o.facts.push(("Cluster IP".into(), ip.into()));
    }
    if !ingress.is_empty() {
        o.facts.push(("External".into(), ingress.join(", ")));
    }
    if let Some(sel) = spec["selector"].as_object() {
        let sel: Vec<String> = sel.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(""))).collect();
        o.facts.push(("Selects".into(), sel.join(", ")));
    }
}

fn node(i: &serde_json::Value, o: &mut KubeObject) {
    let st = &i["status"];
    let ready = st["conditions"].as_array().and_then(|c| c.iter().find(|c| s(&c["type"]) == "Ready")).map(|c| s(&c["status"]) == "True");
    let cordoned = i["spec"]["unschedulable"].as_bool() == Some(true);
    (o.status, o.health) = match (ready, cordoned) {
        (Some(true), false) => ("Ready".into(), KubeHealth::Ok),
        (Some(true), true) => ("Ready, cordoned".into(), KubeHealth::Pending),
        _ => ("NotReady".into(), KubeHealth::Bad),
    };
    let roles: Vec<&str> = i["metadata"]["labels"].as_object().map(|l| l.keys().filter_map(|k| k.strip_prefix("node-role.kubernetes.io/")).collect()).unwrap_or_default();
    if !roles.is_empty() {
        o.facts.push(("Roles".into(), roles.join(", ")));
    }
    let info = &st["nodeInfo"];
    for (k, v) in [("Version", s(&info["kubeletVersion"])), ("OS", s(&info["osImage"]))] {
        if !v.is_empty() {
            o.facts.push((k.into(), v.into()));
        }
    }
    if let Some(ip) = st["addresses"].as_array().and_then(|a| a.iter().find(|a| s(&a["type"]) == "InternalIP")) {
        o.facts.push(("Internal IP".into(), s(&ip["address"]).into()));
    }
    let cap = &st["capacity"];
    if let (Some(cpu), Some(mem)) = (cap["cpu"].as_str(), cap["memory"].as_str()) {
        let gib = mem_bytes(mem).map(|b| format!("{:.1} GiB", b as f64 / 1073741824.0)).unwrap_or_else(|| mem.to_owned());
        o.facts.push(("Capacity".into(), format!("{cpu} cpu, {gib}")));
    }
}

/// `2026-11-12T23:05:00Z` (what kubernetes writes) in ms since the epoch.
fn rfc3339_ms(t: &str) -> Option<u64> {
    let b = t.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[10] != b'T' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| t.get(r)?.parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    // days from the civil date (howard hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    u64::try_from(secs).ok().map(|s| s * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_time_as_kubernetes_writes_it() {
        assert_eq!(rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_ms("2026-11-12T23:05:00Z"), Some(1_794_524_700_000));
        assert_eq!(rfc3339_ms("yesterday"), None);
    }

    fn pod_json(phase: &str, statuses: serde_json::Value) -> serde_json::Value {
        json!({"items": [{
            "metadata": {"name": "web-1", "namespace": "orchard", "creationTimestamp": "2026-11-12T23:05:00Z",
                         "ownerReferences": [{"kind": "ReplicaSet", "name": "web-7f"}]},
            "spec": {"nodeName": "foreman", "containers": [{"name": "web", "image": "ghcr.io/x/web:1"}, {"name": "proxy", "image": "envoy"}]},
            "status": {"phase": phase, "podIP": "10.0.0.4", "qosClass": "Burstable", "containerStatuses": statuses}
        }]})
    }

    #[test]
    fn a_pod_reads_as_kubectl_says_it() {
        let ok = json!([{"name": "web", "ready": true, "restartCount": 1, "state": {"running": {}}}, {"name": "proxy", "ready": true, "restartCount": 2, "state": {"running": {}}}]);
        let p = &parse(KubeKind::Pods, &pod_json("Running", ok))[0];
        assert_eq!((p.status.as_str(), p.health, p.ready.as_deref(), p.restarts), ("Running", KubeHealth::Ok, Some("2/2"), Some(3)));
        assert_eq!(p.namespace.as_deref(), Some("orchard"));
        assert!(p.facts.contains(&("Node".into(), "foreman".into())) && p.facts.contains(&("Owner".into(), "ReplicaSet web-7f".into())));
        assert_eq!(p.containers[0].image, "ghcr.io/x/web:1");

        // one container crash looping is what the pod is.
        let crashing = json!([{"name": "web", "ready": false, "restartCount": 9, "state": {"waiting": {"reason": "CrashLoopBackOff"}}}, {"name": "proxy", "ready": true, "restartCount": 0, "state": {"running": {}}}]);
        let p = &parse(KubeKind::Pods, &pod_json("Running", crashing))[0];
        assert_eq!((p.status.as_str(), p.health, p.ready.as_deref()), ("CrashLoopBackOff", KubeHealth::Bad, Some("1/2")));
        assert_eq!(p.containers[0].state, "waiting: CrashLoopBackOff");

        let done = &parse(KubeKind::Pods, &pod_json("Succeeded", json!([])))[0];
        assert_eq!((done.status.as_str(), done.health), ("Completed", KubeHealth::Quiet));
    }

    #[test]
    fn a_deployment_is_available_rolling_or_stuck() {
        let d = |want: u64, ready: u64, updated: u64, stuck: bool| {
            let cond = if stuck { json!([{"type": "Progressing", "status": "False"}]) } else { json!([]) };
            let j = json!({"items": [{"metadata": {"name": "api", "namespace": "orchard"},
                "spec": {"replicas": want, "template": {"spec": {"containers": [{"image": "api:2"}]}}},
                "status": {"readyReplicas": ready, "updatedReplicas": updated, "conditions": cond}}]});
            let o = parse(KubeKind::Deployments, &j).remove(0);
            (o.status, o.health, o.ready.unwrap())
        };
        assert_eq!(d(3, 3, 3, false), ("Available".into(), KubeHealth::Ok, "3/3".into()));
        assert_eq!(d(3, 2, 1, false), ("Rolling out".into(), KubeHealth::Pending, "2/3".into()));
        assert_eq!(d(3, 2, 1, true).1, KubeHealth::Bad);
        assert_eq!(d(0, 0, 0, false).1, KubeHealth::Quiet);
    }

    #[test]
    fn a_service_shows_its_ports_and_a_node_its_readiness() {
        let svc = json!({"items": [{"metadata": {"name": "web"}, "spec": {"type": "LoadBalancer", "clusterIP": "10.1.0.9",
            "ports": [{"port": 80, "targetPort": 8080, "protocol": "TCP"}, {"port": 443, "targetPort": "https"}], "selector": {"app": "web"}}, "status": {}}]});
        let s = parse(KubeKind::Services, &svc).remove(0);
        assert_eq!((s.status.as_str(), s.health, s.ready.as_deref()), ("LoadBalancer (pending)", KubeHealth::Pending, Some("80→8080, 443→https")));
        let n = json!({"items": [{"metadata": {"name": "chase", "labels": {"node-role.kubernetes.io/worker": ""}}, "spec": {"unschedulable": true},
            "status": {"conditions": [{"type": "Ready", "status": "True"}], "nodeInfo": {"kubeletVersion": "v1.31.2"}, "capacity": {"cpu": "8", "memory": "16Gi"}}}]});
        let n = parse(KubeKind::Nodes, &n).remove(0);
        assert_eq!((n.status.as_str(), n.health), ("Ready, cordoned", KubeHealth::Pending));
        assert!(n.facts.contains(&("Capacity".into(), "8 cpu, 16.0 GiB".into())) && n.facts.contains(&("Roles".into(), "worker".into())));
    }

    #[test]
    fn usage_lands_on_its_object() {
        let mut objects = vec![
            KubeObject { name: "web-1".into(), namespace: Some("orchard".into()), ..Default::default() },
            KubeObject { name: "web-1".into(), namespace: Some("other".into()), ..Default::default() },
        ];
        apply_usage(&mut objects, "orchard   web-1   250m   128Mi\n", true);
        assert_eq!((objects[0].cpu_milli, objects[0].mem_bytes), (Some(250), Some(134_217_728)));
        assert_eq!(objects[1].cpu_milli, None, "the same name in another namespace is another pod");
        let mut nodes = vec![KubeObject { name: "chase".into(), ..Default::default() }];
        apply_usage(&mut nodes, "chase   1200m   15%   6Gi   40%\n", false);
        assert_eq!(nodes[0].cpu_milli, Some(1200));
    }
}
