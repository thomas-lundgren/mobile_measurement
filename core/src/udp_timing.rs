//! Socket helpers for the delivery-timing and back-pressure measurements on
//! realtime UDP flows, shared by the UE engine and the server's generator:
//!
//! - kernel receive timestamps (`SO_TIMESTAMPNS`): when the driver handed the
//!   packet to the network stack, as opposed to when the app read it;
//! - tagged sends: every attempt gets an outcome (`SendStatus`), the syscall
//!   duration and the bytes still queued below the socket (`SIOCOUTQ`: skbs
//!   not yet released by the qdisc/driver — on the phone, not yet taken by
//!   the modem);
//! - a small `SO_SNDBUF` makes the sender feel that queue as `EAGAIN`, and
//!   `IP_RECVERR` reports qdisc drops as `ENOBUFS` (silent otherwise);
//! - the IP TOS byte of sent and received packets (DSCP probes);
//! - `Pacer`: sub-millisecond send spacing (sleep, then spin the last bit).

use std::io;
use std::mem;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

/// Outcome of one send attempt, logged per packet so local drops can be told
/// apart from network loss by sequence number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendStatus {
    /// Accepted by the kernel.
    Ok,
    /// Socket send buffer full (non-blocking): the packets below the socket
    /// have not been released yet — back pressure. Dropped locally.
    Eagain,
    /// Dropped by the qdisc / driver queue (reported with `IP_RECVERR`).
    Enobufs,
    /// Any other error (e.g. ECONNREFUSED after an ICMP error). Not sent.
    Error,
}

impl SendStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SendStatus::Ok => "OK",
            SendStatus::Eagain => "EAGAIN",
            SendStatus::Enobufs => "ENOBUFS",
            SendStatus::Error => "ERROR",
        }
    }

    pub fn of(r: &io::Result<usize>) -> Self {
        match r {
            Ok(_) => SendStatus::Ok,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => SendStatus::Eagain,
            Err(e) if e.raw_os_error() == Some(libc::ENOBUFS) => SendStatus::Enobufs,
            Err(_) => SendStatus::Error,
        }
    }
}

fn setsockopt_int(
    fd: RawFd,
    level: libc::c_int,
    name: libc::c_int,
    val: libc::c_int,
) -> io::Result<()> {
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            &val as *const libc::c_int as *const libc::c_void,
            mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Ask for a kernel receive timestamp on every datagram (read by `recv_stamped`).
pub fn enable_rx_timestamps(fd: RawFd) -> io::Result<()> {
    setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_TIMESTAMPNS, 1)
}

/// Report each received packet's IP TOS byte (read by `recv_stamped`).
pub fn enable_recv_tos(fd: RawFd) -> io::Result<()> {
    setsockopt_int(fd, libc::IPPROTO_IP, libc::IP_RECVTOS, 1)
}

/// Mark this socket's next packets with `tos` (`dscp << 2`, ECN bits zero).
pub fn set_tos(fd: RawFd, tos: u8) -> io::Result<()> {
    setsockopt_int(fd, libc::IPPROTO_IP, libc::IP_TOS, tos as libc::c_int)
}

/// Back-pressure mode for a sending socket: `SO_SNDBUF` = `bytes` (the kernel
/// doubles it and enforces a small minimum) and qdisc drops reported via
/// `IP_RECVERR`. Returns the effective buffer size the kernel reports.
pub fn limit_send_buffer(fd: RawFd, bytes: u32) -> io::Result<u32> {
    setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_SNDBUF, bytes as libc::c_int)?;
    setsockopt_int(fd, libc::IPPROTO_IP, libc::IP_RECVERR, 1)?;
    let mut val: libc::c_int = 0;
    let mut len = mem::size_of::<libc::c_int>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &mut val as *mut libc::c_int as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(val as u32)
}

/// Bytes sent on this socket that the stack below has not released yet
/// (`SIOCOUTQ`). None if the ioctl is refused (e.g. by an SELinux policy).
pub fn outq_bytes(fd: RawFd) -> Option<u32> {
    let mut val: libc::c_int = 0;
    let rc = unsafe { libc::ioctl(fd, libc::TIOCOUTQ as _, &mut val as *mut libc::c_int) };
    (rc == 0 && val >= 0).then_some(val as u32)
}

/// One received datagram: length, source address, and — when enabled on the
/// socket — the kernel receive time (UTC ns, `SO_TIMESTAMPNS`) and the IP TOS
/// byte (`IP_RECVTOS`).
pub struct Received {
    pub n: usize,
    pub from: Option<SocketAddr>,
    pub kernel_utc_ns: Option<u64>,
    pub tos: Option<u8>,
}

/// `recvmsg` returning the source and the enabled ancillary data alongside
/// the data. Honours `SO_RCVTIMEO` / non-blocking like `recv`.
pub fn recv_stamped(fd: RawFd, buf: &mut [u8]) -> io::Result<Received> {
    recv_stamped_flags(fd, buf, 0)
}

/// `recv_stamped` that never blocks (`MSG_DONTWAIT`): WouldBlock when nothing
/// is queued. Leaves the socket's own blocking mode alone.
pub fn recv_stamped_nowait(fd: RawFd, buf: &mut [u8]) -> io::Result<Received> {
    recv_stamped_flags(fd, buf, libc::MSG_DONTWAIT)
}

/// Wait until `fd` has something to read (or an error to report), at most
/// `timeout`. False on timeout.
pub fn wait_readable(fd: RawFd, timeout: Duration) -> bool {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ts = libc::timespec {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_nsec: timeout.subsec_nanos() as _,
    };
    unsafe { libc::ppoll(&mut p, 1, &ts, std::ptr::null()) > 0 }
}

fn recv_stamped_flags(fd: RawFd, buf: &mut [u8], flags: libc::c_int) -> io::Result<Received> {
    let mut addr: libc::sockaddr_storage = unsafe { mem::zeroed() };
    let mut cmsg_buf = [0u64; 16]; // 128 bytes, aligned for cmsghdr
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_name = &mut addr as *mut _ as *mut libc::c_void;
    msg.msg_namelen = mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = mem::size_of_val(&cmsg_buf) as _;

    let n = unsafe { libc::recvmsg(fd, &mut msg, flags) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut kernel_utc_ns = None;
    let mut tos = None;
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    while !cmsg.is_null() {
        let (level, typ) = unsafe { ((*cmsg).cmsg_level, (*cmsg).cmsg_type) };
        if level == libc::SOL_SOCKET && typ == libc::SCM_TIMESTAMPNS {
            let ts: libc::timespec =
                unsafe { std::ptr::read_unaligned(libc::CMSG_DATA(cmsg) as *const libc::timespec) };
            kernel_utc_ns = Some(ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64);
        } else if level == libc::IPPROTO_IP && typ == libc::IP_TOS {
            tos = Some(unsafe { *libc::CMSG_DATA(cmsg) });
        }
        cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
    }
    Ok(Received {
        n: n as usize,
        from: sockaddr_to_addr(&addr, msg.msg_namelen),
        kernel_utc_ns,
        tos,
    })
}

fn sockaddr_to_addr(ss: &libc::sockaddr_storage, len: libc::socklen_t) -> Option<SocketAddr> {
    match ss.ss_family as libc::c_int {
        libc::AF_INET if len as usize >= mem::size_of::<libc::sockaddr_in>() => {
            let a = unsafe { &*(ss as *const _ as *const libc::sockaddr_in) };
            Some(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr)),
                u16::from_be(a.sin_port),
            )))
        }
        libc::AF_INET6 if len as usize >= mem::size_of::<libc::sockaddr_in6>() => {
            let a = unsafe { &*(ss as *const _ as *const libc::sockaddr_in6) };
            Some(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(a.sin6_addr.s6_addr),
                u16::from_be(a.sin6_port),
                a.sin6_flowinfo,
                a.sin6_scope_id,
            )))
        }
        _ => None,
    }
}

/// Raise the calling thread's scheduling priority (nice −10) so a paced
/// sender is not preempted between packets. Best-effort: false if refused.
pub fn raise_thread_priority() -> bool {
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as libc::id_t;
    unsafe { libc::setpriority(libc::PRIO_PROCESS, tid, -10) == 0 }
}

/// Spin this close to a deadline instead of sleeping: thread wake-ups are
/// late by ~50 µs (timer slack) up to ~1 ms (phone CPU idle states).
const SPIN: Duration = Duration::from_micros(300);

/// Fixed-rate send schedule. Periods of 5 ms and up just sleep; shorter ones
/// sleep until `SPIN` before the deadline and spin the rest, so 1 ms spacing
/// holds to a few µs. Falls back to "now" after a stall instead of bursting
/// to catch up.
pub struct Pacer {
    period: Duration,
    next: Instant,
}

impl Pacer {
    pub fn new(period: Duration) -> Self {
        Self {
            period,
            next: Instant::now(),
        }
    }

    pub fn wait(&mut self) {
        self.next += self.period;
        let now = Instant::now();
        if self.next <= now {
            self.next = now;
            return;
        }
        if self.period >= Duration::from_millis(5) {
            std::thread::sleep(self.next - now);
            return;
        }
        if self.next - now > SPIN {
            std::thread::sleep(self.next - now - SPIN);
        }
        while Instant::now() < self.next {
            std::hint::spin_loop();
        }
    }
}

/// Write size and period for a rate-capped bulk TCP sender: 5 ms worth of
/// bytes per write, at most `max_chunk` (the period stretches at high rates).
pub fn bulk_pacing(rate_kbps: u32, max_chunk: usize) -> (usize, Duration) {
    let bytes_per_s = rate_kbps.max(8) as u64 * 1000 / 8;
    let chunk = ((bytes_per_s / 200) as usize).clamp(1, max_chunk);
    (
        chunk,
        Duration::from_nanos(chunk as u64 * 1_000_000_000 / bytes_per_s),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_pacing_keeps_the_rate() {
        for (kbps, max) in [
            (2_000, 65_536),
            (10_000, 65_536),
            (500_000, 65_536),
            (8, 65_536),
        ] {
            let (chunk, period) = bulk_pacing(kbps, max);
            assert!(chunk >= 1 && chunk <= max);
            let rate = chunk as f64 * 8.0 / period.as_secs_f64() / 1000.0;
            assert!(
                (rate - kbps as f64).abs() / (kbps as f64) < 0.01,
                "{kbps}: {rate}"
            );
        }
        assert_eq!(bulk_pacing(2_000, 65_536), (1250, Duration::from_millis(5)));
        assert_eq!(bulk_pacing(500_000, 65_536).0, 65_536);
    }
    use std::net::UdpSocket;
    use std::os::fd::AsRawFd;

    #[test]
    fn kernel_timestamp_and_source_come_back() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        enable_rx_timestamps(rx.as_raw_fd()).unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        tx.send_to(b"hello", rx.local_addr().unwrap()).unwrap();
        let mut buf = [0u8; 64];
        let r = recv_stamped(rx.as_raw_fd(), &mut buf).unwrap();
        assert_eq!(&buf[..r.n], b"hello");
        assert_eq!(r.from, Some(tx.local_addr().unwrap()));
        let now = crate::timestamp::Timestamp::now().utc_ns;
        let k = r.kernel_utc_ns.expect("kernel timestamp");
        assert!(
            k <= now && now - k < 1_000_000_000,
            "kernel {k} vs now {now}"
        );
    }

    #[test]
    fn nowait_receive_and_wait_readable() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut buf = [0u8; 16];
        let e = recv_stamped_nowait(rx.as_raw_fd(), &mut buf).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
        let t0 = Instant::now();
        assert!(!wait_readable(rx.as_raw_fd(), Duration::from_millis(20)));
        assert!(t0.elapsed() >= Duration::from_millis(19));
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        tx.send_to(b"x", rx.local_addr().unwrap()).unwrap();
        assert!(wait_readable(rx.as_raw_fd(), Duration::from_secs(1)));
        assert_eq!(recv_stamped_nowait(rx.as_raw_fd(), &mut buf).unwrap().n, 1);
    }

    #[test]
    fn tos_and_timestamp_come_back_together() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        enable_rx_timestamps(rx.as_raw_fd()).unwrap();
        enable_recv_tos(rx.as_raw_fd()).unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        set_tos(tx.as_raw_fd(), 46 << 2).unwrap();
        tx.send_to(b"ef", rx.local_addr().unwrap()).unwrap();
        let mut buf = [0u8; 64];
        let r = recv_stamped(rx.as_raw_fd(), &mut buf).unwrap();
        assert_eq!(r.tos, Some(46 << 2));
        assert!(r.kernel_utc_ns.is_some());
    }

    #[test]
    fn small_send_buffer_is_applied_and_outq_readable() {
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let eff = limit_send_buffer(tx.as_raw_fd(), 4096).unwrap();
        assert!(eff >= 4096 && eff <= 3 * 4096, "effective SO_SNDBUF {eff}");
        assert_eq!(outq_bytes(tx.as_raw_fd()), Some(0));
    }

    #[test]
    fn send_status_classifies_errors() {
        assert_eq!(SendStatus::of(&Ok(10)), SendStatus::Ok);
        let wb = io::Error::from(io::ErrorKind::WouldBlock);
        assert_eq!(SendStatus::of(&Err(wb)), SendStatus::Eagain);
        let nb = io::Error::from_raw_os_error(libc::ENOBUFS);
        assert_eq!(SendStatus::of(&Err(nb)), SendStatus::Enobufs);
        let cr = io::Error::from_raw_os_error(libc::ECONNREFUSED);
        assert_eq!(SendStatus::of(&Err(cr)), SendStatus::Error);
    }

    #[test]
    fn pacer_holds_one_millisecond() {
        let mut p = Pacer::new(Duration::from_millis(1));
        let start = Instant::now();
        for _ in 0..200 {
            p.wait();
        }
        let ms = start.elapsed().as_secs_f64() * 1e3;
        assert!((199.0..215.0).contains(&ms), "200 periods took {ms:.1} ms");
    }
}
