//! other machines: `tesselctl host …` on the mac (what Settings › Hosts
//! does), `tesselctl remote …` and `tesselctl bridge` on the host.

use crate::Args;
use anyhow::{Context, bail};
use std::path::Path;
use tessel_remote::hosts::{Host, Hosts, Transport};
use tessel_remote::identity;
use tessel_remote::setup::{Options, Said};

/// on the host.
pub async fn remote(home: &Path, args: &mut Args) -> anyhow::Result<()> {
    let dir = identity::dir(home);
    match args.next("serve, install, allow, revoke, clients or host-key")?.as_str() {
        "serve" => {
            let port = args.opt("--port")?.map(|p| p.parse()).transpose()?.unwrap_or(tessel_remote::quic::DEFAULT_PORT);
            tessel_remote::service::serve(home, port).await
        }
        "serve-ws" => {
            // a sandbox's service: websockets through its https route, and quic by key
            // (iroh) beside them, which macs prefer. websockets carry on if udp can't.
            let port = args.opt("--port")?.map(|p| p.parse()).transpose()?.unwrap_or(tessel_remote::ws::PORT);
            let h = home.to_owned();
            tokio::spawn(async move {
                if let Err(e) = tessel_remote::p2p::serve(&h).await {
                    eprintln!("tessel remote: iroh stopped: {e:#}");
                }
            });
            tessel_remote::ws::serve(home, port).await
        }
        "bring" => {
            // what a mac brought (its claude, git identity, sign-ins), put in place here.
            let from = args.next("the folder it was unpacked in")?;
            for did in tessel_remote::carry::bring(std::path::Path::new(&from), home)? {
                println!("brought {did}");
            }
            Ok(())
        }
        "iroh-id" => {
            println!("{}", tessel_remote::p2p::host_id(home)?);
            Ok(())
        }
        "install" => {
            let port = args.opt("--port")?.map(|p| p.parse()).transpose()?.unwrap_or(tessel_remote::quic::DEFAULT_PORT);
            println!("{}", tessel_remote::service::install(home, port)?);
            Ok(())
        }
        "allow" => {
            let fp = args.next("a fingerprint (SHA256:…)")?;
            let name = args.rest.drain(..).collect::<Vec<_>>().join(" ");
            identity::allow(&dir, &fp, &name)?;
            println!("{fp} can connect");
            Ok(())
        }
        "revoke" => {
            let which = args.next("a fingerprint or name")?;
            if identity::revoke(&dir, &which)? {
                println!("{which} can't connect any more (it stays connected until it disconnects)");
            } else {
                println!("{which} wasn't allowed");
            }
            Ok(())
        }
        "clients" => {
            for a in identity::allowed(&dir) {
                println!("{}  {}", a.fingerprint, a.name);
            }
            Ok(())
        }
        "host-key" => {
            println!("{}", identity::Identity::load_or_make(&dir, "host")?.fingerprint());
            Ok(())
        }
        other => bail!("unknown remote command {other}"),
    }
}

pub fn protocol_word(major: u16, minor: u16, min_client_minor: u16) -> String {
    let mut word = format!("protocol {major}.{minor}");
    if !tessel_proto::compatible(major) {
        word.push_str(", won't talk to this mac: update needed");
    } else if tessel_proto::daemon_behind(major, minor) {
        word.push_str(", update needed (tesselctl host update)");
    } else if tessel_proto::client_too_old(min_client_minor) {
        word.push_str(", newer than this mac: update tessel here");
    }
    word
}

/// on the mac.
pub async fn host(home: &Path, args: &mut Args, json: bool) -> anyhow::Result<()> {
    let mut hosts = Hosts::load(home);
    match args.next("ls, add, setup, use, rm or key")?.as_str() {
        "ls" => {
            if json {
                println!("{}", serde_json::to_string_pretty(&hosts)?);
                return Ok(());
            }
            let current = hosts.current.clone();
            println!("{} this mac  (protocol {}.{})", if current.is_none() { "*" } else { " " }, tessel_proto::PROTO_MAJOR, tessel_proto::PROTO_MINOR);
            let mut probes = Vec::new();
            for h in hosts.hosts.iter().filter(|h| h.ready() && h.vercel.is_none()) {
                let (home, host) = (home.to_owned(), h.clone());
                probes.push((h.id.clone(), tokio::spawn(async move { tokio::time::timeout(std::time::Duration::from_secs(5), tessel_remote::connect(&home, &host, "tesselctl")).await.ok()?.ok().map(|c| c.welcome) })));
            }
            let mut daemons = std::collections::HashMap::new();
            for (id, probe) in probes {
                daemons.insert(id, probe.await.ok().flatten());
            }
            for h in &hosts.hosts {
                let how = match (h.ready(), h.transport) {
                    (false, _) => "not set up".to_owned(),
                    (true, Transport::Quic) => format!("quic {}", h.address.as_deref().unwrap_or("")),
                    (true, Transport::Ssh) => "through ssh".to_owned(),
                    (true, Transport::Vercel) => format!("vercel sandbox, {}", h.address.as_deref().unwrap_or("")),
                };
                let reach = if h.vercel.is_some() { String::new() } else { format!("ssh {}; ", h.ssh) };
                let daemon = match daemons.get(&h.id) {
                    Some(Some(w)) => format!("  {}", protocol_word(w.proto_major, w.proto_minor, w.min_client_minor)),
                    Some(None) => "  daemon not reached".to_owned(),
                    None => String::new(),
                };
                println!("{} {}  ({}, {reach}{how}){daemon}", if current.as_deref() == Some(&h.id) { "*" } else { " " }, h.id, h.name);
            }
            Ok(())
        }
        "add" if args.rest.iter().any(|a| a == "--vercel") => {
            let name = args.opt("--vercel")?.context("--vercel NAME: what to call the sandbox")?;
            let vcpus = args.opt("--vcpus")?.map(|v| v.parse()).transpose()?.unwrap_or(2);
            anyhow::ensure!(tessel_remote::vercel::SIZES.contains(&vcpus), "--vcpus: 1, 2, 4, 8, 16 or 32 (2 GB of memory each)");
            let mut host = Host::sandbox(&name, vcpus);
            // its disk is kept between sessions unless it's asked to start fresh each time.
            if args.flag("--fresh")
                && let Some(sb) = host.vercel.as_mut()
            {
                sb.persistent = false;
            }
            let id = hosts.add(host);
            hosts.save(home)?;
            println!("added {id}");
            set_up(home, &id, args).await
        }
        "net" => {
            // how sandboxes are reached: what Settings › Hosts › Sandbox connections sets.
            use tessel_remote::net::{Net, Relay, Via};
            let mut net = Net::load(home);
            let changes: Vec<String> = args.rest.drain(..).collect();
            for c in &changes {
                let (k, v) = c.split_once('=').with_context(|| format!("{c}: write it as key=value"))?;
                match k {
                    "via" => net.via = match v { "auto" => Via::Auto, "quic" => Via::Quic, "ws" | "websockets" => Via::Ws, _ => bail!("via: auto, quic or ws") },
                    "relay" => net.relay = match v { "n0" | "iroh" => Relay::N0, "none" | "off" => Relay::None, url => Relay::Own(url.to_owned()) },
                    "lookup" => net.lookup = matches!(v, "on" | "true" | "yes"),
                    other => bail!("no setting {other}: via, relay or lookup"),
                }
            }
            if !changes.is_empty() {
                net.save(home)?;
                for h in hosts.hosts.iter().filter(|h| h.vercel.is_some()) {
                    match tessel_remote::vercel::reconfigure(home, h).await {
                        Ok(true) => println!("{}: its service started again with these", h.name),
                        Ok(false) => {}
                        Err(e) => println!("{}: couldn't tell it ({e:#}); it gets them when it next starts", h.name),
                    }
                }
            }
            println!("via {:?}, relay {}, lookup {}", net.via, match &net.relay { Relay::N0 => "n0 (iroh's public relays)".to_owned(), Relay::None => "none".to_owned(), Relay::Own(u) => u.clone() }, if net.lookup { "on" } else { "off" });
            Ok(())
        }
        "bring" => {
            // bring your claude (and git identity, and sign-ins) to a host: what the setup flow offers.
            let which = args.next("which host")?;
            let host = hosts.find(&which).with_context(|| format!("no host {which}"))?.clone();
            let inv = tessel_remote::carry::inventory();
            let mut carry = carry_from(&inv, args)?;
            if let Some(t) = args.opt("--claude-token")? {
                // `-`: read from stdin, so it stays out of the shell's history.
                let t = if t == "-" {
                    let mut s = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
                    s
                } else {
                    t
                };
                let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
                anyhow::ensure!(t.starts_with("sk-ant-") && t.len() >= 40, "that isn't a claude token: they start with sk-ant-oat01- (make one with `claude setup-token`)");
                carry.claude_token = Some(t);
            } else if args.flag("--sign-in") {
                carry.claude_token = Some(tessel_remote::claude_login::interactive().await?);
            }
            anyhow::ensure!(!carry.is_empty(), "nothing to bring: --what claude-md,settings,skills,commands,agents,mcp,git,gh, --sign-in or --claude-token");
            let mut say = |s: Said| match s {
                Said::Step(s) => println!("· {s}"),
                Said::Note(n) => println!("    {n}"),
        Said::Progress(done, of) => print_progress(done, of),
            };
            tessel_remote::setup::bring(home, &host, carry, &mut say).await?;
            println!("brought to {}", host.name);
            Ok(())
        }
        "ping" => {
            // how fast it is: reaching it, the path, and a few requests there and back.
            let which = args.next("which host")?;
            let host = hosts.find(&which).with_context(|| format!("no host {which}"))?.clone();
            let t = std::time::Instant::now();
            let c = tessel_remote::connect(home, &host, "tesselctl").await?;
            let reached = t.elapsed();
            let via = c.link.as_ref().map_or("ssh", |l| l.kind());
            println!("{}: connected in {} ms, {via}", host.name, reached.as_millis());
            // a direct path is usually found within a second of connecting.
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            if let Some(l) = &c.link {
                println!("  path now: {}, rtt {} ms", l.kind(), l.rtt().as_millis());
            }
            let mut times = Vec::new();
            for _ in 0..10 {
                let t = std::time::Instant::now();
                c.client.request(tessel_proto::Request::TermList).await?;
                times.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            times.sort_by(|a, b| a.total_cmp(b));
            println!("  requests: median {:.0} ms, best {:.0} ms, worst {:.0} ms", times[5], times[0], times[9]);
            // again, as the window does when it reconnects: everything already warm.
            drop(c);
            let t = std::time::Instant::now();
            let c = tessel_remote::connect(home, &host, "tesselctl").await?;
            println!("  connecting again: {} ms, {}", t.elapsed().as_millis(), c.link.as_ref().map_or("ssh", |l| l.kind()));
            Ok(())
        }
        "keys" => {
            let which = args.next("which host")?;
            let off = args.flag("--off");
            let host = hosts.find(&which).with_context(|| format!("no host {which}"))?.clone();
            let (local, _, _) = tessel_client::connect_or_start(home, "tesselctl").await?;
            let tessel_proto::Response::Keys { keys, .. } = local.request(tessel_proto::Request::Keys).await? else { anyhow::bail!("unexpected reply") };
            let with: Vec<(tessel_proto::keys::KeyInfo, String)> = if off { Vec::new() } else { keys.iter().map(|k| (k.clone(), crate::key_secret(&k.name).unwrap_or_default())).filter(|(_, s)| !s.is_empty()).collect() };
            tessel_remote::vercel::share_keys(home, &host, &with).await?;
            let there = tessel_remote::connect(home, &host, "tesselctl").await?;
            let tessel_proto::Response::Keys { keys: theirs, .. } = there.client.request(tessel_proto::Request::Keys).await? else { anyhow::bail!("unexpected reply") };
            for k in theirs.iter().filter(|k| off || !with.iter().any(|(w, _)| w.name == k.name)) {
                there.client.request(tessel_proto::Request::KeyRemove { name: k.name.clone() }).await?;
            }
            for (k, _) in &with {
                there.client.request(tessel_proto::Request::KeySet { key: tessel_proto::keys::KeyInfo { stored: false, uses: 0, ..k.clone() }, value: None }).await?;
            }
            if off {
                println!("{} no longer gets your keys", host.name);
            } else {
                let names: Vec<&str> = with.iter().map(|(k, _)| k.name.as_str()).collect();
                println!("{} gets {}: added on the way out by vercel's firewall, so the secrets never enter it", host.name, if names.is_empty() { "no keys".to_owned() } else { names.join(", ") });
            }
            Ok(())
        }
        "stop" => {
            let which = args.next("which host")?;
            let host = hosts.find(&which).with_context(|| format!("no host {which}"))?.clone();
            if tessel_remote::vercel::stop(home, &host).await? {
                println!("{} stopped; its disk is kept, and showing it again starts it", host.name);
            } else {
                println!("{} wasn't running", host.name);
            }
            Ok(())
        }
        "add" => {
            let ssh = args.next("where ssh reaches it (user@host, or a name from ~/.ssh/config)")?;
            let name = args.opt("--name")?.unwrap_or_default();
            let key = args.opt("--key")?;
            let no_setup = args.flag("--no-setup");
            let mut host = Host::new(&name, &ssh);
            host.key = key;
            let id = hosts.add(host);
            hosts.save(home)?;
            println!("added {id}");
            if no_setup {
                return Ok(());
            }
            set_up(home, &id, args).await
        }
        "setup" | "update" => {
            let which = args.next("which host")?;
            let id = hosts.find(&which).with_context(|| format!("no host {which} (tesselctl host ls)"))?.id.clone();
            set_up(home, &id, args).await
        }
        "use" => {
            let which = args.next("a host, or `mac`")?;
            hosts.current = match which.as_str() {
                "mac" | "local" | "this" => None,
                w => Some(hosts.find(w).with_context(|| format!("no host {w}"))?.id.clone()),
            };
            hosts.save(home)?;
            println!("the window shows {}", hosts.current().map_or("this mac", |h| h.name.as_str()));
            Ok(())
        }
        "rm" => {
            // a sandbox goes from vercel too, its disk with it, unless it's kept.
            let keep = args.flag("--keep-sandbox");
            let which = args.next("which host")?;
            let host = hosts.find(&which).with_context(|| format!("no host {which}"))?.clone();
            let id = host.id.clone();
            if host.vercel.is_some() && !keep {
                tessel_remote::vercel::delete(home, &host).await.context("deleting the sandbox from vercel (--keep-sandbox removes it from tessel only)")?;
            }
            hosts.remove(&id);
            hosts.save(home)?;
            match (host.vercel.is_some(), keep) {
                (true, false) => println!("removed {id}, and deleted its sandbox from vercel"),
                (true, true) => println!("removed {id} from tessel; its sandbox is still in vercel"),
                _ => println!("removed {id} (tessel stays installed there: `tesselctl remote revoke` on it shuts this mac out)"),
            }
            Ok(())
        }
        "key" => {
            println!("{}", identity::Identity::load_or_make(&identity::dir(home), "client")?.fingerprint());
            Ok(())
        }
        other => bail!("unknown host command {other}"),
    }
}

async fn set_up(home: &Path, id: &str, args: &mut Args) -> anyhow::Result<()> {
    let mut opts = Options::default();
    if let Some(p) = args.opt("--port")? {
        opts.port = p.parse()?;
    }
    opts.claude = !args.flag("--no-claude");
    let mut host = Hosts::load(home).get(id).cloned().context("the host went")?;
    let mut say = |s: Said| match s {
        Said::Step(s) => println!("· {s}"),
        Said::Note(n) => println!("    {n}"),
        Said::Progress(done, of) => print_progress(done, of),
    };
    let result = tessel_remote::setup::setup(home, &mut host, &opts, &mut say).await;
    // what was learned is kept, even when a later step failed.
    let mut hosts = Hosts::load(home);
    hosts.put(host.clone());
    hosts.save(home)?;
    result?;
    println!(
        "{} is ready: {} (`tesselctl host use {}` shows it in the window)",
        host.name,
        match host.transport {
            Transport::Quic => format!("quic to {}", host.address.as_deref().unwrap_or("")),
            Transport::Ssh => "through ssh".into(),
            Transport::Vercel => format!("a vercel sandbox at {}", host.address.as_deref().unwrap_or("")),
        },
        host.id
    );
    Ok(())
}

/// what to bring, from `--what a,b,c` (else what's suggested for what's there).
fn carry_from(inv: &tessel_remote::carry::Inventory, args: &mut Args) -> anyhow::Result<tessel_remote::carry::Carry> {
    let Some(what) = args.opt("--what")? else { return Ok(tessel_remote::carry::Carry::suggested(inv)) };
    let mut c = tessel_remote::carry::Carry::default();
    for w in what.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        match w {
            "all" => {
                c = tessel_remote::carry::Carry::suggested(inv);
                c.mcp = inv.mcp > 0;
                c.gh = inv.gh;
            }
            "none" => c = tessel_remote::carry::Carry::default(),
            "claude-md" => c.claude_md = true,
            "settings" => c.settings = true,
            "skills" => c.skills = true,
            "commands" => c.commands = true,
            "agents" => c.agents = true,
            "mcp" => c.mcp = true,
            "git" => c.git = true,
            "gh" => c.gh = true,
            other => bail!("--what: no {other} (claude-md, settings, skills, commands, agents, mcp, git, gh, all, none)"),
        }
    }
    Ok(c)
}

/// how far an upload's got, on one line that keeps being rewritten.
fn print_progress(done: u64, of: u64) {
    use std::io::Write as _;
    let mb = |b: u64| b as f64 / 1_048_576.0;
    print!("\r    {:.1} of {:.1} MB", mb(done), mb(of));
    if done >= of {
        println!();
    }
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_proto::{PROTO_MAJOR, PROTO_MINOR};

    #[test]
    fn a_host_behind_this_mac_says_it_needs_an_update() {
        assert_eq!(protocol_word(PROTO_MAJOR, PROTO_MINOR, 0), format!("protocol {PROTO_MAJOR}.{PROTO_MINOR}"));
        assert!(protocol_word(PROTO_MAJOR, PROTO_MINOR - 1, 0).ends_with("update needed (tesselctl host update)"));
        assert!(protocol_word(PROTO_MAJOR + 1, 0, 0).contains("won't talk to this mac"));
        assert!(protocol_word(PROTO_MAJOR, PROTO_MINOR + 1, PROTO_MINOR + 1).contains("newer than this mac"));
    }
}
