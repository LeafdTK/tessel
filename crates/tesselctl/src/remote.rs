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
                };
                println!("{} {}  ({}, ssh {}; {how})", if current.as_deref() == Some(&h.id) { "*" } else { " " }, h.id, h.name, h.ssh);
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
        },
        host.id
    );
    Ok(())
}
