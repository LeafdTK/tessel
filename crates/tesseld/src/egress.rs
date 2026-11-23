use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tessel_proto::SessionId;
use tessel_proto::keys::{KeyInfo, KeyUse};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const ALWAYS: [&str; 4] = ["anthropic.com", "*.anthropic.com", "claude.ai", "*.claude.ai"];
const USES_KEPT: usize = 500;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Scope {
    pub session: Option<SessionId>,
    pub sealed: bool,
    #[serde(default)]
    pub allow: Vec<String>,
}

struct Ca {
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
    der: Vec<u8>,
    hosts: Vec<String>,
}

struct Shared {
    dir: PathBuf,
    keys: RwLock<Vec<(KeyInfo, String)>>,
    scopes: RwLock<HashMap<String, Scope>>,
    uses: Mutex<VecDeque<KeyUse>>,
    ca: RwLock<Option<Ca>>,
    leaves: Mutex<HashMap<String, (Vec<u8>, Vec<u8>)>>,
    upstream: Arc<rustls::ClientConfig>,
}

#[derive(Clone)]
pub struct Egress {
    pub port: u16,
    shared: Arc<Shared>,
}

type Body = BoxBody<Bytes, hyper::Error>;

fn pem(label: &str, der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

fn civil(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
    (y, m, d)
}

fn ymd(offset: i64) -> (i32, u8, u8) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    civil(now / 86_400 + offset)
}

fn ca_params(hosts: &[String]) -> rcgen::CertificateParams {
    let mut p = rcgen::CertificateParams::default();
    p.distinguished_name = rcgen::DistinguishedName::new();
    p.distinguished_name.push(rcgen::DnType::CommonName, "tessel keys");
    p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    p.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign, rcgen::KeyUsagePurpose::CrlSign];
    p.name_constraints = Some(rcgen::NameConstraints {
        permitted_subtrees: hosts.iter().map(|h| rcgen::GeneralSubtree::DnsName(h.trim_start_matches("*.").to_owned())).collect(),
        excluded_subtrees: Vec::new(),
    });
    p
}

fn load_or_make_ca(dir: &Path, hosts: &[String]) -> Option<Ca> {
    if hosts.is_empty() {
        return None;
    }
    let (key_path, cert_path, hosts_path) = (dir.join("ca.key"), dir.join("ca.der"), dir.join("ca.hosts"));
    let same = std::fs::read_to_string(&hosts_path).is_ok_and(|h| h == hosts.join("\n"));
    if same
        && let (Ok(key), Ok(der)) = (std::fs::read(&key_path), std::fs::read(&cert_path))
        && let Ok(kp) = rcgen::KeyPair::try_from(key.as_slice())
    {
        return Some(Ca { issuer: rcgen::Issuer::new(ca_params(hosts), kp), der, hosts: hosts.to_vec() });
    }
    let kp = rcgen::KeyPair::generate().ok()?;
    let mut params = ca_params(hosts);
    params.not_before = { let (y, m, d) = ymd(-1); rcgen::date_time_ymd(y, m, d) };
    params.not_after = { let (y, m, d) = ymd(3650); rcgen::date_time_ymd(y, m, d) };
    let cert = params.self_signed(&kp).ok()?;
    let der = cert.der().to_vec();
    std::fs::create_dir_all(dir).ok()?;
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&key_path).ok()?;
        std::io::Write::write_all(&mut f, &kp.serialize_der()).ok()?;
    }
    std::fs::write(&cert_path, &der).ok()?;
    std::fs::write(dir.join("ca.pem"), pem("CERTIFICATE", &der)).ok()?;
    std::fs::write(&hosts_path, hosts.join("\n")).ok()?;
    Some(Ca { issuer: rcgen::Issuer::new(params, kp), der, hosts: hosts.to_vec() })
}

fn system_roots() -> String {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("security").args(["find-certificate", "-a", "-p", "/System/Library/Keychains/SystemRootCertificates.keychain"]).output();
        if let Ok(o) = out
            && o.status.success()
        {
            return String::from_utf8_lossy(&o.stdout).into_owned();
        }
    }
    for p in ["/etc/ssl/certs/ca-certificates.crt", "/etc/pki/tls/certs/ca-bundle.crt", "/etc/ssl/cert.pem"] {
        if let Ok(t) = std::fs::read_to_string(p) {
            return t;
        }
    }
    String::new()
}

pub fn allowed_host(scope: &Scope, keys: &[(KeyInfo, String)], host: &str) -> bool {
    !scope.sealed
        || host == "localhost"
        || host == "127.0.0.1"
        || ALWAYS.iter().any(|h| crate::keys::host_matches(h, host))
        || scope.allow.iter().any(|h| crate::keys::host_matches(h, host))
        || keys.iter().any(|(k, _)| k.hosts.iter().any(|h| crate::keys::host_matches(h, host)))
}

impl Egress {
    pub async fn start(dir: PathBuf, keys: Vec<(KeyInfo, String)>) -> std::io::Result<Self> {
        Self::start_with(dir, keys, Vec::new()).await
    }

    pub async fn start_with(dir: PathBuf, keys: Vec<(KeyInfo, String)>, extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for r in extra_roots {
            let _ = roots.add(r);
        }
        let mut upstream = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(std::io::Error::other)?
            .with_root_certificates(roots)
            .with_no_client_auth();
        upstream.alpn_protocols = vec![b"http/1.1".to_vec()];
        let scopes = std::fs::read_to_string(dir.join("scopes.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let shared = Arc::new(Shared {
            dir: dir.clone(),
            keys: RwLock::new(Vec::new()),
            scopes: RwLock::new(scopes),
            uses: Mutex::new(VecDeque::new()),
            ca: RwLock::new(None),
            leaves: Mutex::new(HashMap::new()),
            upstream: Arc::new(upstream),
        });
        let egress = Egress { port, shared };
        egress.set_keys(keys);
        let roots = system_roots();
        let ca = std::fs::read_to_string(dir.join("ca.pem")).unwrap_or_default();
        std::fs::write(dir.join("bundle.pem"), format!("{roots}\n{ca}"))?;
        let shared = egress.shared.clone();
        tokio::spawn(async move {
            while let Ok((conn, _)) = listener.accept().await {
                let shared = shared.clone();
                tokio::spawn(async move {
                    let _ = serve(shared, conn).await;
                });
            }
        });
        Ok(egress)
    }

    pub fn set_keys(&self, keys: Vec<(KeyInfo, String)>) {
        let mut hosts: Vec<String> = keys.iter().flat_map(|(k, _)| k.hosts.clone()).collect();
        hosts.sort();
        hosts.dedup();
        let changed = self.shared.ca.read().unwrap().as_ref().map(|c| c.hosts.clone()) != Some(hosts.clone());
        if changed {
            *self.shared.ca.write().unwrap() = load_or_make_ca(&self.shared.dir, &hosts);
            self.shared.leaves.lock().unwrap().clear();
            let roots = system_roots();
            let ca = std::fs::read_to_string(self.shared.dir.join("ca.pem")).unwrap_or_default();
            let _ = std::fs::write(self.shared.dir.join("bundle.pem"), format!("{roots}\n{ca}"));
        }
        *self.shared.keys.write().unwrap() = keys;
    }

    pub fn certificate(&self) -> PathBuf {
        self.shared.dir.join("ca.pem")
    }

    pub fn scope(&self, scope: Scope) -> String {
        let mut scopes = self.shared.scopes.write().unwrap();
        let token = match scopes.iter().find(|(_, s)| s.session.is_some() && s.session == scope.session) {
            Some((t, _)) => t.clone(),
            None => uuid::Uuid::new_v4().simple().to_string(),
        };
        scopes.insert(token.clone(), scope);
        if let Ok(text) = serde_json::to_string(&*scopes) {
            use std::os::unix::fs::OpenOptionsExt;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(self.shared.dir.join("scopes.json")) {
                let _ = std::io::Write::write_all(&mut f, text.as_bytes());
            }
        }
        token
    }

    pub fn uses(&self) -> Vec<KeyUse> {
        self.shared.uses.lock().unwrap().iter().cloned().collect()
    }

    pub fn env(&self, token: &str, sealed: bool, keys: &[KeyInfo]) -> Vec<(String, String)> {
        let proxy = format!("http://tessel:{token}@127.0.0.1:{}", self.port);
        let bundle = self.shared.dir.join("bundle.pem").to_string_lossy().into_owned();
        let ca = self.certificate().to_string_lossy().into_owned();
        let no_proxy = if sealed { "localhost,127.0.0.1,::1".to_owned() } else { "localhost,127.0.0.1,::1,anthropic.com,.anthropic.com,claude.ai,.claude.ai".to_owned() };
        let mut env: Vec<(String, String)> = [
            ("HTTPS_PROXY", proxy.clone()),
            ("https_proxy", proxy.clone()),
            ("HTTP_PROXY", proxy.clone()),
            ("http_proxy", proxy),
            ("NO_PROXY", no_proxy.clone()),
            ("no_proxy", no_proxy),
            ("NODE_USE_ENV_PROXY", "1".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        if !keys.is_empty() {
            for k in ["SSL_CERT_FILE", "REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE", "GIT_SSL_CAINFO"] {
                env.push((k.to_owned(), bundle.clone()));
            }
            env.push(("NODE_EXTRA_CA_CERTS".into(), ca));
        }
        for k in keys {
            if let Some(name) = &k.env {
                env.push((name.clone(), k.placeholder()));
            }
        }
        env
    }

    pub fn note(&self, u: KeyUse) {
        note(&self.shared, u);
    }
}

fn note(shared: &Shared, u: KeyUse) {
    let mut uses = shared.uses.lock().unwrap();
    uses.push_back(u);
    while uses.len() > USES_KEPT {
        uses.pop_front();
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

async fn read_head(conn: &mut TcpStream) -> std::io::Result<(String, Vec<(String, String)>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        let n = conn.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let rest = buf[end + 4..].to_vec();
            let mut lines = head.split("\r\n");
            let first = lines.next().unwrap_or("").to_owned();
            let headers = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))).collect();
            return Ok((first, headers, rest));
        }
        if buf.len() > 32 * 1024 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    }
}

fn token_of(headers: &[(String, String)]) -> Option<String> {
    let v = headers.iter().find(|(k, _)| k == "proxy-authorization")?.1.clone();
    let b64 = v.strip_prefix("Basic ").or_else(|| v.strip_prefix("basic "))?;
    let raw = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    String::from_utf8(raw).ok()?.split_once(':').map(|(_, t)| t.to_owned())
}

async fn serve(shared: Arc<Shared>, mut conn: TcpStream) -> std::io::Result<()> {
    let (first, headers, rest) = read_head(&mut conn).await?;
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_owned(), parts.next().unwrap_or("").to_owned());
    let scope = token_of(&headers).and_then(|t| shared.scopes.read().unwrap().get(&t).cloned()).unwrap_or(Scope { session: None, sealed: false, allow: Vec::new() });
    let keys = shared.keys.read().unwrap().clone();
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = target.rsplit_once(':').map(|(h, p)| (h.trim_matches(['[', ']']).to_owned(), p.parse().unwrap_or(443))).unwrap_or((target.clone(), 443));
        if !allowed_host(&scope, &keys, &host) {
            note(&shared, KeyUse { at_ms: now_ms(), session: scope.session, key: None, host: host.clone(), method: "CONNECT".into(), path: String::new(), allowed: false });
            conn.write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\ntessel: this agent is sealed, and that host isn't in its allow list\n").await?;
            return Ok(());
        }
        let key = crate::keys::for_host(&keys.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(), &host).cloned();
        match key {
            Some(k) => {
                let secret = keys.iter().find(|(x, _)| x.name == k.name).map(|(_, s)| s.clone()).unwrap_or_default();
                conn.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
                broker(shared, conn, host, port, k, secret, scope).await
            }
            None => {
                let mut up = TcpStream::connect((host.as_str(), port)).await?;
                conn.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
                if !rest.is_empty() {
                    up.write_all(&rest).await?;
                }
                tokio::io::copy_bidirectional(&mut conn, &mut up).await.map(|_| ())
            }
        }
    } else {
        let url = target.strip_prefix("http://").unwrap_or(&target);
        let (authority, path) = url.split_once('/').map(|(a, p)| (a.to_owned(), format!("/{p}"))).unwrap_or((url.to_owned(), "/".into()));
        let (host, port) = authority.rsplit_once(':').map(|(h, p)| (h.to_owned(), p.parse().unwrap_or(80))).unwrap_or((authority.clone(), 80));
        if !allowed_host(&scope, &keys, &host) {
            note(&shared, KeyUse { at_ms: now_ms(), session: scope.session, key: None, host: host.clone(), method: method.clone(), path: path.clone(), allowed: false });
            conn.write_all(b"HTTP/1.1 403 Forbidden\r\nconnection: close\r\n\r\n").await?;
            return Ok(());
        }
        let mut up = TcpStream::connect((host.as_str(), port)).await?;
        let mut head = format!("{method} {path} HTTP/1.1\r\n");
        for (k, v) in headers.iter().filter(|(k, _)| k != "proxy-authorization" && k != "proxy-connection" && k != "connection") {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("connection: close\r\n\r\n");
        up.write_all(head.as_bytes()).await?;
        up.write_all(&rest).await?;
        tokio::io::copy_bidirectional(&mut conn, &mut up).await.map(|_| ())
    }
}

fn leaf(shared: &Shared, host: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    if let Some(l) = shared.leaves.lock().unwrap().get(host) {
        return Some(l.clone());
    }
    let ca = shared.ca.read().unwrap();
    let ca = ca.as_ref()?;
    let kp = rcgen::KeyPair::generate().ok()?;
    let mut params = rcgen::CertificateParams::new(vec![host.to_owned()]).ok()?;
    params.distinguished_name.push(rcgen::DnType::CommonName, host);
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    params.not_before = { let (y, m, d) = ymd(-1); rcgen::date_time_ymd(y, m, d) };
    params.not_after = { let (y, m, d) = ymd(30); rcgen::date_time_ymd(y, m, d) };
    let cert = params.signed_by(&kp, &ca.issuer).ok()?;
    let out = (cert.der().to_vec(), kp.serialize_der());
    shared.leaves.lock().unwrap().insert(host.to_owned(), out.clone());
    Some(out)
}

async fn broker(shared: Arc<Shared>, conn: TcpStream, host: String, port: u16, key: KeyInfo, secret: String, scope: Scope) -> std::io::Result<()> {
    let (cert, pkey) = leaf(&shared, &host).ok_or_else(|| std::io::Error::other("no certificate"))?;
    let ca = shared.ca.read().unwrap().as_ref().map(|c| c.der.clone()).unwrap_or_default();
    let chain = vec![rustls::pki_types::CertificateDer::from(cert), rustls::pki_types::CertificateDer::from(ca)];
    let pk = rustls::pki_types::PrivateKeyDer::Pkcs8(pkey.into());
    let mut server = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(std::io::Error::other)?
        .with_no_client_auth()
        .with_single_cert(chain, pk)
        .map_err(std::io::Error::other)?;
    server.alpn_protocols = vec![b"http/1.1".to_vec()];
    let tls = tokio_rustls::TlsAcceptor::from(Arc::new(server)).accept(conn).await?;
    let sender: Arc<tokio::sync::Mutex<Option<hyper::client::conn::http1::SendRequest<Incoming>>>> = Arc::new(tokio::sync::Mutex::new(None));
    let svc = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
        let (shared, host, key, secret, scope, sender) = (shared.clone(), host.clone(), key.clone(), secret.clone(), scope.clone(), sender.clone());
        async move { Ok::<_, hyper::Error>(forward(shared, (host, port), key, secret, scope, sender, req).await) }
    });
    hyper::server::conn::http1::Builder::new().keep_alive(true).serve_connection(TokioIo::new(tls), svc).await.map_err(std::io::Error::other)
}

fn header_value(format: &str, secret: &str) -> String {
    let mut out = format.replace("{key}", secret);
    while let Some(start) = out.find("{basic:") {
        let Some(len) = out[start..].find('}') else { break };
        let user = out[start + 7..start + len].to_owned();
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{secret}"));
        out.replace_range(start..start + len + 1, &encoded);
    }
    out
}

fn plain(status: u16, text: String) -> hyper::Response<Body> {
    let mut r = hyper::Response::new(Full::new(Bytes::from(text)).map_err(|never| match never {}).boxed());
    *r.status_mut() = hyper::StatusCode::from_u16(status).unwrap_or(hyper::StatusCode::BAD_GATEWAY);
    r
}

async fn forward(
    shared: Arc<Shared>,
    (host, port): (String, u16),
    key: KeyInfo,
    secret: String,
    scope: Scope,
    sender: Arc<tokio::sync::Mutex<Option<hyper::client::conn::http1::SendRequest<Incoming>>>>,
    mut req: hyper::Request<Incoming>,
) -> hyper::Response<Body> {
    let method = req.method().as_str().to_owned();
    let path = req.uri().path_and_query().map(|p| p.as_str().to_owned()).unwrap_or_else(|| "/".into());
    let ok = crate::keys::allows(&key, &method, &path) && !secret.is_empty();
    note(&shared, KeyUse { at_ms: now_ms(), session: scope.session, key: Some(key.name.clone()), host: host.clone(), method: method.clone(), path: path.clone(), allowed: ok });
    if !ok {
        let why = if secret.is_empty() { format!("tessel: the {} key has no secret stored", key.name) } else { format!("tessel: the {} key isn't allowed to {method} {path}", key.name) };
        return plain(403, why);
    }
    *req.uri_mut() = path.parse().unwrap_or_default();
    let name = hyper::header::HeaderName::from_bytes(key.header.as_bytes()).unwrap_or(hyper::header::AUTHORIZATION);
    req.headers_mut().remove(&name);
    if let Ok(v) = hyper::header::HeaderValue::from_str(&header_value(&key.format, &secret)) {
        req.headers_mut().insert(name, v);
    }
    if !req.headers().contains_key(hyper::header::HOST)
        && let Ok(h) = hyper::header::HeaderValue::from_str(&host)
    {
        req.headers_mut().insert(hyper::header::HOST, h);
    }
    let mut guard = sender.lock().await;
    if guard.as_ref().is_none_or(|s| s.is_closed()) {
        let made = async {
            let tcp = TcpStream::connect((host.as_str(), port)).await?;
            let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(std::io::Error::other)?;
            let tls = tokio_rustls::TlsConnector::from(shared.upstream.clone()).connect(name, tcp).await?;
            let (s, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls)).await.map_err(std::io::Error::other)?;
            tokio::spawn(conn);
            Ok::<_, std::io::Error>(s)
        }
        .await;
        match made {
            Ok(s) => *guard = Some(s),
            Err(e) => return plain(502, format!("tessel: couldn't reach {host}: {e}")),
        }
    }
    let s = guard.as_mut().expect("just made");
    match s.send_request(req).await {
        Ok(resp) => resp.map(|b| b.boxed()),
        Err(e) => plain(502, format!("tessel: {host} didn't answer: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn upstream() -> (u16, rustls::pki_types::CertificateDer<'static>) {
        let kp = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap().self_signed(&kp).unwrap();
        let der = cert.der().clone();
        let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![der.clone()], rustls::pki_types::PrivateKeyDer::Pkcs8(kp.serialize_der().into()))
            .unwrap();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((c, _)) = l.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(c).await else { return };
                    let svc = hyper::service::service_fn(|req: hyper::Request<Incoming>| async move {
                        let auth = req.headers().get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("none").to_owned();
                        let body = format!("{} {} {auth}", req.method(), req.uri());
                        Ok::<_, hyper::Error>(hyper::Response::new(Full::new(Bytes::from(body))))
                    });
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tls), svc).await;
                });
            }
        });
        (port, der)
    }

    async fn through(eg: &Egress, token: &str, port: u16, method: &str, path: &str) -> String {
        let mut c = TcpStream::connect(("127.0.0.1", eg.port)).await.unwrap();
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("tessel:{token}"));
        c.write_all(format!("CONNECT localhost:{port} HTTP/1.1\r\nhost: localhost:{port}\r\nproxy-authorization: Basic {auth}\r\n\r\n").as_bytes()).await.unwrap();
        let mut buf = [0u8; 39];
        c.read_exact(&mut buf).await.unwrap();
        assert!(String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 200"));
        let mut roots = rustls::RootCertStore::empty();
        roots.add(rustls::pki_types::CertificateDer::from(std::fs::read(eg.shared.dir.join("ca.der")).unwrap())).unwrap();
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider())).with_safe_default_protocol_versions().unwrap().with_root_certificates(roots).with_no_client_auth();
        let tls = tokio_rustls::TlsConnector::from(Arc::new(cfg)).connect(rustls::pki_types::ServerName::try_from("localhost").unwrap(), c).await.expect("the agent trusts tessel's certificate");
        let (mut s, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tls)).await.unwrap();
        tokio::spawn(conn);
        let req = hyper::Request::builder().method(method).uri(path).header("host", "localhost").header("authorization", "Bearer tessel-key-test").body(Full::new(Bytes::new())).unwrap();
        let resp = s.send_request(req).await.unwrap();
        let status = resp.status().as_u16();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        format!("{status} {}", String::from_utf8_lossy(&body))
    }

    #[tokio::test]
    async fn the_secret_is_added_on_the_way_out_and_only_where_the_key_allows() {
        let (port, upstream_ca) = upstream().await;
        let dir = tempfile::tempdir().unwrap();
        let key = KeyInfo { name: "test".into(), hosts: vec!["localhost".into()], header: "authorization".into(), format: "Bearer {key}".into(), methods: vec!["GET".into()], paths: vec!["/ok*".into()], ..Default::default() };
        let eg = Egress::start_with(dir.path().to_path_buf(), vec![(key, "s3cret".into())], vec![upstream_ca]).await.unwrap();
        let token = eg.scope(Scope { session: None, sealed: false, allow: vec![] });
        assert_eq!(through(&eg, &token, port, "GET", "/ok/1?x=2").await, "200 GET /ok/1?x=2 Bearer s3cret", "the real secret, swapped in for the placeholder");
        let no = through(&eg, &token, port, "GET", "/admin").await;
        assert!(no.starts_with("403") && no.contains("isn't allowed to GET /admin"), "{no}");
        assert!(through(&eg, &token, port, "POST", "/ok").await.starts_with("403"));
        let uses = eg.uses();
        assert_eq!(uses.iter().filter(|u| u.allowed).count(), 1);
        assert_eq!(uses.iter().filter(|u| !u.allowed).count(), 2);
        let pem = std::fs::read_to_string(eg.certificate()).unwrap();
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----"));
        let env = eg.env(&token, false, &[KeyInfo { name: "test".into(), env: Some("TEST_TOKEN".into()), ..Default::default() }]);
        assert!(env.iter().any(|(k, v)| k == "TEST_TOKEN" && v == "tessel-key-test"), "agents see a placeholder, never the secret");
        assert!(!env.iter().any(|(_, v)| v.contains("s3cret")));
    }

    #[tokio::test]
    async fn a_sealed_agent_is_refused_anywhere_else() {
        let dir = tempfile::tempdir().unwrap();
        let eg = Egress::start(dir.path().to_path_buf(), vec![]).await.unwrap();
        let token = eg.scope(Scope { session: None, sealed: true, allow: vec![] });
        let mut c = TcpStream::connect(("127.0.0.1", eg.port)).await.unwrap();
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("tessel:{token}"));
        c.write_all(format!("CONNECT example.com:443 HTTP/1.1\r\nproxy-authorization: Basic {auth}\r\n\r\n").as_bytes()).await.unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).await.unwrap();
        assert!(out.starts_with("HTTP/1.1 403") && out.contains("sealed"), "{out}");
        assert!(eg.uses().iter().any(|u| !u.allowed && u.host == "example.com"));
    }

    #[test]
    fn dates_come_out_right() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_000), (2024, 10, 4));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    #[test]
    fn a_sealed_agent_reaches_only_its_model_its_keys_and_what_it_was_allowed() {
        let k = KeyInfo { name: "gh".into(), hosts: vec!["api.github.com".into()], ..Default::default() };
        let keys = vec![(k, "s".to_owned())];
        let sealed = Scope { session: None, sealed: true, allow: vec!["*.npmjs.org".into()] };
        for ok in ["api.anthropic.com", "claude.ai", "api.github.com", "registry.npmjs.org", "localhost"] {
            assert!(allowed_host(&sealed, &keys, ok), "{ok}");
        }
        for no in ["evil.com", "github.com", "npmjs.org.evil.com"] {
            assert!(!allowed_host(&sealed, &keys, no), "{no}");
        }
        assert!(allowed_host(&Scope { sealed: false, ..sealed }, &keys, "evil.com"), "an open agent goes anywhere; keys are only added where they belong");
    }
}
