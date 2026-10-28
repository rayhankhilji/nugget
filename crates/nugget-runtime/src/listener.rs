//! Who owns the TCP listener on a loopback port? (Only for TCP listeners: Windows, or Unix with
//! [`crate::RuntimeOptions::unix_socket`] off. A Unix socket lives in a private directory, see
//! [`crate::socket`].)
//!
//! Between picking a free port and the server binding it, another local user can bind the port
//! first and answer `/health` with ok. Requests would then carry the API key, prompts and
//! documents to that process. So before a server is declared ready, the listener that answered
//! must belong to the server we spawned.
//!
//! - **Linux:** the listening sockets that a connection to `127.0.0.1:<port>` can reach (state
//!   `LISTEN` on `127.0.0.1`, `0.0.0.0`, `::` or `::ffff:127.0.0.1`, from `/proc/net/tcp{,6}`)
//!   must all be open in the child or a process of its process group (`/proc/<pid>/fd`, matched
//!   by socket inode). The child is our own unreaped child, so its pid cannot be recycled, and
//!   another user cannot join its process group.
//! - **macOS, Windows, other Unix:** not verified (that needs `libproc` / `GetExtendedTcpTable`,
//!   i.e. FFI, which this crate does not use). The supervisor still requires the child to be
//!   alive before and after the probe, and rotates the API key on every start.

/// What the listener check found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) enum Ownership {
    /// Every listener reachable on the port belongs to the child's process group.
    Child,
    /// Nothing is listening on the port (yet).
    NotListening,
    /// Some other process listens on the port (description for the error message).
    Foreign(String),
    /// This platform cannot tell.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Unverified,
}

/// Check who listens on `127.0.0.1:port`, given the server's pid (also its process group id).
#[cfg(target_os = "linux")]
pub(crate) fn owner(port: u16, child_pid: u32) -> Ownership {
    linux::owner(std::path::Path::new("/proc"), port, child_pid)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn owner(_port: u16, _child_pid: u32) -> Ownership {
    Ownership::Unverified
}

#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux {
    use std::collections::HashSet;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::path::Path;

    use super::Ownership;

    /// TCP state `LISTEN` in `/proc/net/tcp`.
    const LISTEN: &str = "0A";
    /// Lines read per table (a busy host has a few thousand sockets).
    const MAX_LINES: usize = 1 << 20;

    #[derive(Debug, PartialEq, Eq)]
    pub(super) struct Listener {
        pub inode: u64,
        pub uid: u32,
    }

    pub(super) fn owner(proc: &Path, port: u16, child_pid: u32) -> Ownership {
        let mut listeners = Vec::new();
        let mut read_any = false;
        for (table, v6) in [("net/tcp", false), ("net/tcp6", true)] {
            // tcp6 is missing when IPv6 is disabled; tcp is always there.
            if let Ok(text) = std::fs::read_to_string(proc.join(table)) {
                read_any = true;
                listeners.extend(parse_listeners(&text, port, v6));
            }
        }
        if !read_any {
            return Ownership::Foreign(format!(
                "cannot read {}/net/tcp to check who listens on port {port}",
                proc.display()
            ));
        }
        if listeners.is_empty() {
            return Ownership::NotListening;
        }
        let mut ours = socket_inodes(&proc.join(child_pid.to_string()));
        let mut scanned_group = false;
        for l in &listeners {
            if ours.contains(&l.inode) {
                continue;
            }
            if !scanned_group {
                // A worker process of the server (same process group) may hold the socket.
                scanned_group = true;
                for pid in group_members(proc, child_pid) {
                    ours.extend(socket_inodes(&proc.join(pid.to_string())));
                }
                if ours.contains(&l.inode) {
                    continue;
                }
            }
            return Ownership::Foreign(format!(
                "a process of uid {} that is not the server (pid {child_pid}) listens on \
                 127.0.0.1:{port}",
                l.uid
            ));
        }
        Ownership::Child
    }

    /// `LISTEN` sockets on `port` that a connection to `127.0.0.1:port` can reach.
    pub(super) fn parse_listeners(text: &str, port: u16, v6: bool) -> Vec<Listener> {
        let mut out = Vec::new();
        for line in text.lines().skip(1).take(MAX_LINES) {
            let f: Vec<&str> = line.split_whitespace().take(10).collect();
            if f.len() < 10 || f[3] != LISTEN {
                continue;
            }
            let Some((addr, p)) = f[1].split_once(':') else {
                continue;
            };
            if u16::from_str_radix(p, 16).ok() != Some(port) {
                continue;
            }
            let reaches = if v6 {
                parse_v6(addr).is_some_and(|a| {
                    a.is_unspecified() || a.to_ipv4_mapped() == Some(Ipv4Addr::LOCALHOST)
                })
            } else {
                parse_v4(addr).is_some_and(|a| a == Ipv4Addr::LOCALHOST || a.is_unspecified())
            };
            if !reaches {
                continue;
            }
            let (Ok(uid), Ok(inode)) = (f[7].parse::<u32>(), f[9].parse::<u64>()) else {
                continue;
            };
            out.push(Listener { inode, uid });
        }
        out
    }

    /// The kernel prints each 32-bit word of the address as a native-endian integer.
    fn parse_v4(hex: &str) -> Option<Ipv4Addr> {
        if hex.len() != 8 {
            return None;
        }
        let w = u32::from_str_radix(hex, 16).ok()?;
        Some(Ipv4Addr::from(w.to_ne_bytes()))
    }

    fn parse_v6(hex: &str) -> Option<Ipv6Addr> {
        if hex.len() != 32 || !hex.is_ascii() {
            return None;
        }
        let mut b = [0u8; 16];
        for i in 0..4 {
            let w = u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).ok()?;
            b[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
        }
        Some(Ipv6Addr::from(b))
    }

    /// Inodes of the sockets open in the process at `dir` (`/proc/<pid>`).
    fn socket_inodes(dir: &Path) -> HashSet<u64> {
        let Ok(fds) = std::fs::read_dir(dir.join("fd")) else {
            return HashSet::new();
        };
        fds.flatten()
            .filter_map(|e| std::fs::read_link(e.path()).ok())
            .filter_map(|t| {
                t.to_str()?
                    .strip_prefix("socket:[")?
                    .strip_suffix(']')?
                    .parse()
                    .ok()
            })
            .collect()
    }

    /// Pids (other than `pgid` itself) whose process group is `pgid`.
    fn group_members(proc: &Path, pgid: u32) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir(proc) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter(|&pid| pid != pgid)
            .filter(|pid| {
                std::fs::read_to_string(proc.join(pid.to_string()).join("stat"))
                    .ok()
                    .and_then(|s| stat_pgid(&s))
                    == Some(pgid)
            })
            .collect()
    }

    /// The process group from `/proc/<pid>/stat` (`pid (comm) state ppid pgrp ...`; `comm` may
    /// contain spaces and parentheses, so fields are counted after the last `)`).
    pub(super) fn stat_pgid(stat: &str) -> Option<u32> {
        let rest = &stat[stat.rfind(')')? + 1..];
        rest.split_whitespace().nth(2)?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::linux::*;

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 111 1 0000000000000000 100 0 0 10 0
   1: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1001        0 222 1 0000000000000000 100 0 0 10 0
   2: 0101A8C0:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1002        0 333 1 0000000000000000 100 0 0 10 0
   3: 0100007F:1F90 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 444 1 0000000000000000 100 0 0 10 0
   4: 0100007F:1F91 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 555 1 0000000000000000 100 0 0 10 0
   garbage
";

    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1003        0 666 1 0000000000000000 100 0 0 10 0
   1: 00000000000000000000000001000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1004        0 777 1 0000000000000000 100 0 0 10 0
   2: 0000000000000000FFFF00000100007F:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1005        0 888 1 0000000000000000 100 0 0 10 0
";

    #[test]
    #[cfg(target_endian = "little")]
    fn parses_the_listeners_that_loopback_v4_reaches() {
        let v4 = parse_listeners(TCP, 8080, false);
        let inodes: Vec<u64> = v4.iter().map(|l| l.inode).collect();
        // 127.0.0.1 and 0.0.0.0 in LISTEN on 8080; not 192.168.1.1, not an established
        // connection, not another port.
        assert_eq!(inodes, vec![111, 222]);
        assert_eq!(v4[1].uid, 1001);
        let v6: Vec<u64> = parse_listeners(TCP6, 8080, true)
            .iter()
            .map(|l| l.inode)
            .collect();
        // `::` and `::ffff:127.0.0.1`, not `::1`.
        assert_eq!(v6, vec![666, 888]);
        assert!(parse_listeners(TCP, 1, false).is_empty());
    }

    #[test]
    fn pgid_from_stat_with_awkward_names() {
        assert_eq!(stat_pgid("42 (llama-server) S 1 42 42 0 -1"), Some(42));
        assert_eq!(stat_pgid("43 (a) b (c)) R 42 42 7 0"), Some(42));
        assert_eq!(stat_pgid("garbage"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn own_listener_is_recognised_and_others_are_not() {
        use super::{Ownership, owner};
        // This test process listens: as "the child", it owns the port.
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let me = std::process::id();
        assert_eq!(owner(port, me), Ownership::Child);
        // Seen as somebody else's server (a pid that does not exist owns nothing).
        assert!(matches!(owner(port, 0x3fff_fff0), Ownership::Foreign(_)));
        drop(l);
        assert_eq!(owner(port, me), Ownership::NotListening);
    }
}
