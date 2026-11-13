//! restarting the daemon without stopping anything it runs.
//!
//! the old daemon starts the new one with one end of a socket pair, and over
//! it sends what it was running: each terminal's pty (its program never sees
//! a hangup) with everything that was on it, and each idle agent's pipes. the
//! files themselves go across as file descriptors (`SCM_RIGHTS`); a short
//! description of each goes first, as JSON, and the instance lock goes last.
//! then the old daemon leaves, and the new one carries on, holding the lock
//! all along and serving on the socket path (the old one leaves it be).
//!
//! an agent in the middle of a turn holds state that only exists in the old
//! daemon's memory (what it's streaming, a question waiting on you), so a
//! restart waits for turns to end first; one that can't wait cuts them short,
//! as a crash would, and they pick up again from their conversations.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use tessel_proto::SessionId;
use tessel_proto::term::TermInfo;

/// the environment variable that tells a new daemon which descriptor the old one is on.
pub const ENV: &str = "TESSEL_HANDOFF_FD";

/// what crosses over, besides the descriptors themselves (in this order:
/// each terminal's pty, then each agent's stdin, stdout and stderr).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub terms: Vec<Term>,
    pub agents: Vec<Agent>,
    pub next_term: u64,
    /// the single-instance lock comes too, as the last descriptor. it's held
    /// by the open file, so the new daemon has it before the old one lets go:
    /// there's no moment with no daemon for something else to start into.
    #[serde(default)]
    pub lock: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Term {
    pub info: TermInfo,
    pub pid: i32,
    /// everything on it, as a program would have written it (base64).
    pub screen: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Agent {
    pub session: SessionId,
    pub pid: u32,
    /// what it wrote that hadn't been read yet (base64).
    pub unread: String,
    /// what this process has cost so far: its next report counts from there.
    pub cost_process: f64,
}

/// how many descriptors go in one message.
const BATCH: usize = 32;

/// send the manifest, then the descriptors.
pub fn send(sock: &UnixStream, manifest: &Manifest, fds: &[OwnedFd]) -> io::Result<()> {
    let json = serde_json::to_vec(manifest)?;
    let mut s = sock;
    s.write_all(&(json.len() as u64).to_be_bytes())?;
    s.write_all(&(fds.len() as u64).to_be_bytes())?;
    s.write_all(&json)?;
    for chunk in fds.chunks(BATCH) {
        let raw: Vec<RawFd> = chunk.iter().map(AsRawFd::as_raw_fd).collect();
        send_fds(sock, &raw)?;
    }
    Ok(())
}

/// receive what `send` sent.
pub fn recv(sock: &UnixStream) -> io::Result<(Manifest, Vec<OwnedFd>)> {
    let mut s = sock;
    let mut n = [0u8; 8];
    s.read_exact(&mut n)?;
    let len = u64::from_be_bytes(n) as usize;
    s.read_exact(&mut n)?;
    let count = u64::from_be_bytes(n) as usize;
    let mut json = vec![0u8; len];
    s.read_exact(&mut json)?;
    let manifest: Manifest = serde_json::from_slice(&json)?;
    let mut fds = Vec::with_capacity(count);
    while fds.len() < count {
        let got = recv_fds(sock, BATCH)?;
        if got.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the old daemon stopped sending"));
        }
        fds.extend(got);
    }
    Ok((manifest, fds))
}

fn cmsg_space(n: usize) -> usize {
    unsafe { libc::CMSG_SPACE((n * std::mem::size_of::<RawFd>()) as u32) as usize }
}

fn send_fds(sock: &UnixStream, fds: &[RawFd]) -> io::Result<()> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
    let mut control = vec![0u8; cmsg_space(fds.len())];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = control.len() as _;
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN((fds.len() * std::mem::size_of::<RawFd>()) as u32) as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(c).cast::<RawFd>(), fds.len());
        if libc::sendmsg(sock.as_raw_fd(), &msg, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn recv_fds(sock: &UnixStream, max: usize) -> io::Result<Vec<OwnedFd>> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
    let mut control = vec![0u8; cmsg_space(max)];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = control.len() as _;
    let n = unsafe { libc::recvmsg(sock.as_raw_fd(), &mut msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut out = Vec::new();
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let bytes = (*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                let data = libc::CMSG_DATA(c).cast::<RawFd>();
                for i in 0..bytes / std::mem::size_of::<RawFd>() {
                    let fd = *data.add(i);
                    // the new daemon's own children mustn't inherit these.
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                    out.push(OwnedFd::from_raw_fd(fd));
                }
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_and_their_description_cross_over_intact() {
        let (a, b) = UnixStream::pair().unwrap();
        // forty pipes: more than one batch.
        let mut ends = Vec::new();
        let mut fds = Vec::new();
        for _ in 0..40 {
            let mut p = [0 as RawFd; 2];
            assert_eq!(unsafe { libc::pipe(p.as_mut_ptr()) }, 0);
            let (r, w) = unsafe { (OwnedFd::from_raw_fd(p[0]), OwnedFd::from_raw_fd(p[1])) };
            ends.push(w);
            fds.push(r);
        }
        let m = Manifest { next_term: 7, ..Default::default() };
        let sender = std::thread::spawn(move || send(&a, &m, &fds).unwrap());
        let (got, received) = recv(&b).unwrap();
        sender.join().unwrap();
        assert_eq!((got.next_term, received.len()), (7, 40));
        // what's written into an old end comes out of the new one.
        let mut w = std::fs::File::from(ends.remove(13));
        w.write_all(b"hi").unwrap();
        let mut r = std::fs::File::from(received.into_iter().nth(13).unwrap());
        let mut buf = [0u8; 2];
        r.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hi");
    }
}
