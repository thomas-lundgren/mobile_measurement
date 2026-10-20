//! Root-qdisc statistics over rtnetlink (`RTM_GETQDISC` dump) — what
//! `tc -s qdisc show` prints, without starting a process per sample.

use std::io;
use std::mem;

const RTM_NEWQDISC: u16 = 36;
const RTM_GETQDISC: u16 = 38;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_DUMP: u16 = 0x300;
const TC_H_ROOT: u32 = 0xFFFF_FFFF;
const TCA_KIND: u16 = 1;
const TCA_STATS2: u16 = 7;
const TCA_STATS_BASIC: u16 = 1;
const TCA_STATS_QUEUE: u16 = 3;
const NLMSG_HDR: usize = 16;
const TCMSG: usize = 20;

#[derive(Debug, Clone, Default)]
pub struct Qdisc {
    pub dev: String,
    pub kind: String,
    pub bytes: u64,
    pub packets: u32,
    pub qlen: u32,
    pub backlog: u32,
    pub drops: u32,
    pub requeues: u32,
    pub overlimits: u32,
}

pub struct QdiscDumper {
    fd: i32,
    seq: u32,
    buf: Vec<u8>,
}

impl Drop for QdiscDumper {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_ne_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_ne_bytes(b[i..i + 4].try_into().unwrap())
}

/// (type, payload) of each rtattr in `b`.
fn attrs(b: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= b.len() {
        let len = u16_at(b, i) as usize;
        if len < 4 || i + len > b.len() {
            break;
        }
        out.push((u16_at(b, i + 2) & 0x3fff, &b[i + 4..i + len]));
        i += align4(len);
    }
    out
}

fn ifname(index: i32) -> String {
    let mut buf = [0 as libc::c_char; libc::IF_NAMESIZE];
    let p = unsafe { libc::if_indextoname(index as u32, buf.as_mut_ptr()) };
    if p.is_null() {
        return format!("if{index}");
    }
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

/// Parse one RTM_NEWQDISC message body (after the nlmsghdr).
pub fn parse_qdisc(body: &[u8]) -> Option<(i32, u32, Qdisc)> {
    if body.len() < TCMSG {
        return None;
    }
    let ifindex = i32::from_ne_bytes(body[4..8].try_into().ok()?);
    let parent = u32_at(body, 12);
    let mut q = Qdisc::default();
    for (ty, val) in attrs(&body[TCMSG..]) {
        match ty {
            TCA_KIND => q.kind = String::from_utf8_lossy(val).trim_end_matches('\0').to_string(),
            TCA_STATS2 => {
                for (sty, sval) in attrs(val) {
                    if sty == TCA_STATS_BASIC && sval.len() >= 12 {
                        q.bytes = u64::from_ne_bytes(sval[0..8].try_into().ok()?);
                        q.packets = u32_at(sval, 8);
                    } else if sty == TCA_STATS_QUEUE && sval.len() >= 20 {
                        q.qlen = u32_at(sval, 0);
                        q.backlog = u32_at(sval, 4);
                        q.drops = u32_at(sval, 8);
                        q.requeues = u32_at(sval, 12);
                        q.overlimits = u32_at(sval, 16);
                    }
                }
            }
            _ => {}
        }
    }
    Some((ifindex, parent, q))
}

impl QdiscDumper {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, libc::NETLINK_ROUTE) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut addr: libc::sockaddr_nl = unsafe { mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as u16;
        let rc = unsafe {
            libc::bind(fd, &addr as *const _ as *const libc::sockaddr, mem::size_of::<libc::sockaddr_nl>() as u32)
        };
        if rc != 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }
        Ok(Self { fd, seq: 1, buf: vec![0u8; 64 * 1024] })
    }

    /// Root qdisc of every device.
    pub fn dump_roots(&mut self) -> io::Result<Vec<Qdisc>> {
        self.seq = self.seq.wrapping_add(1);
        let mut req = [0u8; NLMSG_HDR + TCMSG];
        req[0..4].copy_from_slice(&((NLMSG_HDR + TCMSG) as u32).to_ne_bytes());
        req[4..6].copy_from_slice(&RTM_GETQDISC.to_ne_bytes());
        req[6..8].copy_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_ne_bytes());
        req[8..12].copy_from_slice(&self.seq.to_ne_bytes());
        let n = unsafe { libc::send(self.fd, req.as_ptr() as *const libc::c_void, req.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut out = Vec::new();
        loop {
            let n = unsafe { libc::recv(self.fd, self.buf.as_mut_ptr() as *mut libc::c_void, self.buf.len(), 0) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            let b = &self.buf[..n as usize];
            let mut i = 0;
            while i + NLMSG_HDR <= b.len() {
                let len = u32_at(b, i) as usize;
                let ty = u16_at(b, i + 4);
                if len < NLMSG_HDR || i + len > b.len() {
                    break;
                }
                match ty {
                    NLMSG_DONE => return Ok(out),
                    NLMSG_ERROR => {
                        let errno = i32::from_ne_bytes(b[i + NLMSG_HDR..i + NLMSG_HDR + 4].try_into().unwrap());
                        if errno != 0 {
                            return Err(io::Error::from_raw_os_error(-errno));
                        }
                        return Ok(out);
                    }
                    RTM_NEWQDISC => {
                        if let Some((ifindex, parent, mut q)) = parse_qdisc(&b[i + NLMSG_HDR..i + len]) {
                            if parent == TC_H_ROOT {
                                q.dev = ifname(ifindex);
                                out.push(q);
                            }
                        }
                    }
                    _ => {}
                }
                i += align4(len);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dumps_root_qdiscs_on_this_host() {
        // Any Linux host has at least the loopback device's root qdisc.
        let mut d = QdiscDumper::new().expect("netlink");
        let qs = d.dump_roots().expect("dump");
        assert!(qs.iter().any(|q| q.dev == "lo"), "{qs:?}");
        assert!(qs.iter().all(|q| !q.kind.is_empty()));
    }
}
