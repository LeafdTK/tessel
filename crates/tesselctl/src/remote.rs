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
            println!("{} this mac", if current.is_none() { "*" } else { " " });
            for h in &hosts.hosts {
                let how = match (h.ready(), h.transport) {
                    (false, _) => "not set up".to_owned(),
                    (true, Transport::Quic) => format!("quic {}", h.address.as_deref().unwrap_or("")),
                    (true, Transport::Ssh) => "through ssh".to_owned(),
                    (true, Transport::Vercel) => format!("vercel sandbox, {}", h.address.as_deref().unwrap_or("")),
                };
                let reach = if h.vercel.is_some() { String::new() } else { format!("ssh {}; ", h.ssh) };
                println!("{} {}  ({}, {reach}{how})", if current.as_deref() == Some(&h.id) { "*" } else { " " }, h.id, h.name);
            }
            Ok(())
        }
        "add" if args.rest.iter().any(|a| a == "--vercel") => {
            let name = args.opt("--vercel")?.context("--vercel NAME: what to call the sandbox")?;
            let vcpus = args.opt("--vcpus")?.map(|v| v.parse()).transpose()?.unwrap_or(2);
            anyhow::ensure!(vcpus == 1 || (vcpus % 2 == 0 && vcpus <= 8), "--vcpus: 1, 2, 4 or 8");
            let id = hosts.add(Host::sandbox(&name, vcpus));
            hosts.save(home)?;
            println!("added {id}");
            set_up(home, &id, args).await
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
            let which = args.next("which host")?;
            let id = hosts.find(&which).with_context(|| format!("no host {which}"))?.id.clone();
            hosts.remove(&id);
            hosts.save(home)?;
            println!("removed {id} (tessel stays installed there: `tesselctl remote revoke` on it shuts this mac out)");
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
