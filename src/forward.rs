/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! A minimal userspace TCP forwarder: `127.0.0.1:<host>` → `<guest-ip>:<guest>`.
//!
//! Stands in for WSL's localhost forwarding. One thread per listener, two
//! per connection, with in-flight connections bounded by
//! [`MAX_CONNECTIONS`]; std only. No read/idle timeouts: forwarded ssh
//! and dev-server sessions are legitimately long-lived, so the bound —
//! not a timer — is the resource guard.

use std::io;
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::str::FromStr;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, bail};

/// Max simultaneous proxied connections per listener. Excess
/// connections wait in the kernel accept backlog — the bound turns a
/// stalled/flooding client from a thread leak into mere queueing.
const MAX_CONNECTIONS: usize = 64;

/// Counting semaphore over Mutex+Condvar; one [`Permit`] per in-flight
/// connection.
struct Slots {
    free: Mutex<usize>,
    avail: Condvar,
}

impl Slots {
    /// Take a slot, blocking until one frees up.
    fn acquire(self: &Arc<Self>) -> Permit {
        let mut free = self.free.lock().unwrap();
        while *free == 0 {
            free = self.avail.wait(free).unwrap();
        }
        *free -= 1;
        Permit(Arc::clone(self))
    }
}

/// Returns its slot to the pool on drop.
struct Permit(Arc<Slots>);

impl Drop for Permit {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap() += 1;
        self.0.avail.notify_one();
    }
}

/// A `HOST_PORT[:GUEST_PORT]` mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortMapping {
    pub host: u16,
    pub guest: u16,
}

impl FromStr for PortMapping {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let port = |p: &str| -> Result<u16> {
            match p.parse::<u16>() {
                Ok(0) | Err(_) => bail!("invalid port `{p}` in `{s}`"),
                Ok(n) => Ok(n),
            }
        };
        match s.split_once(':') {
            Some((h, g)) => Ok(PortMapping {
                host: port(h)?,
                guest: port(g)?,
            }),
            None => {
                let p = port(s)?;
                Ok(PortMapping { host: p, guest: p })
            }
        }
    }
}

/// Copy one direction until EOF, then half-close the destination so the
/// peer sees end-of-stream.
fn pump(mut from: TcpStream, mut to: TcpStream) {
    let _ = io::copy(&mut from, &mut to);
    let _ = to.shutdown(Shutdown::Write);
}

fn proxy(client: TcpStream, target: SocketAddr) -> io::Result<()> {
    let upstream = TcpStream::connect(target)?;
    let (c2, u2) = (client.try_clone()?, upstream.try_clone()?);
    let a = thread::spawn(move || pump(client, upstream));
    pump(u2, c2);
    let _ = a.join();
    Ok(())
}

/// Accept connections on `listener` forever, proxying each to `target`
/// with at most `max_connections` in flight.
fn spawn_forwarder_bounded(
    listener: TcpListener,
    target: SocketAddr,
    max_connections: usize,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let slots = Arc::new(Slots {
            free: Mutex::new(max_connections),
            avail: Condvar::new(),
        });
        loop {
            // Acquire before accept: while at cap, new connections sit in
            // the kernel backlog rather than spawning threads.
            let permit = slots.acquire();
            match listener.accept() {
                Ok((client, _)) => {
                    thread::spawn(move || {
                        let _permit = permit;
                        if let Err(e) = proxy(client, target) {
                            eprintln!("cm: forward to {target} failed: {e}");
                        }
                    });
                }
                Err(e) => eprintln!("cm: accept failed: {e}"),
            }
        }
    })
}

/// Accept connections on `listener` forever, proxying each to `target`.
pub fn spawn_forwarder(listener: TcpListener, target: SocketAddr) -> JoinHandle<()> {
    spawn_forwarder_bounded(listener, target, MAX_CONNECTIONS)
}

/// Bind every mapping on `bind` and forward to `guest`; blocks until all
/// listeners exit (i.e. until the process is interrupted).
pub fn forward(bind: IpAddr, guest: IpAddr, mappings: &[PortMapping]) -> Result<()> {
    let mut handles = Vec::new();
    for m in mappings {
        let addr = SocketAddr::new(bind, m.host);
        let listener =
            TcpListener::bind(addr).with_context(|| format!("failed to listen on {addr}"))?;
        let target = SocketAddr::new(guest, m.guest);
        eprintln!("Forwarding {addr} -> {target}");
        handles.push(spawn_forwarder(listener, target));
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn parse_mappings() {
        assert_eq!(
            "8080".parse::<PortMapping>().unwrap(),
            PortMapping {
                host: 8080,
                guest: 8080
            }
        );
        assert_eq!(
            "3000:80".parse::<PortMapping>().unwrap(),
            PortMapping {
                host: 3000,
                guest: 80
            }
        );
        for bad in ["", "0", "x", "1:", "70000", "1:2:3"] {
            assert!(bad.parse::<PortMapping>().is_err(), "{bad}");
        }
    }

    #[test]
    fn forwards_echo_round_trip() {
        let echo = TcpListener::bind("127.0.0.1:0").unwrap();
        let echo_addr = echo.local_addr().unwrap();
        thread::spawn(move || {
            let (mut s, _) = echo.accept().unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).unwrap();
            s.write_all(&buf).unwrap();
        });

        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let front_addr = front.local_addr().unwrap();
        spawn_forwarder(front, echo_addr);

        let mut c = TcpStream::connect(front_addr).unwrap();
        c.write_all(b"hello through the forwarder").unwrap();
        c.shutdown(Shutdown::Write).unwrap();
        let mut got = String::new();
        c.read_to_string(&mut got).unwrap();
        assert_eq!(got, "hello through the forwarder");
    }

    fn wait_until(cond: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if cond() {
                return true;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    /// At the connection cap a new client isn't proxied upstream until a
    /// slot frees — excess connections wait in the accept backlog.
    #[test]
    fn forwarder_bounds_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Upstream: count accepts, then read until the client goes away
        // and close — like a real server, so the proxy finishes.
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let up_addr = upstream.local_addr().unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let accept_seen = Arc::clone(&seen);
        thread::spawn(move || {
            for s in upstream.incoming() {
                let n = Arc::clone(&accept_seen);
                thread::spawn(move || {
                    let mut s = s.unwrap();
                    n.fetch_add(1, Ordering::SeqCst);
                    let mut buf = Vec::new();
                    let _ = s.read_to_end(&mut buf);
                });
            }
        });

        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let front_addr = front.local_addr().unwrap();
        spawn_forwarder_bounded(front, up_addr, 2);

        let c1 = TcpStream::connect(front_addr).unwrap();
        let c2 = TcpStream::connect(front_addr).unwrap();
        assert!(wait_until(|| seen.load(Ordering::SeqCst) == 2));

        // Third connection: TCP handshake completes (kernel backlog)
        // but the forwarder must not connect upstream yet.
        let _c3 = TcpStream::connect(front_addr).unwrap();
        thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(seen.load(Ordering::SeqCst), 2);

        // Releasing a slot lets the queued connection through.
        drop(c1);
        drop(c2);
        assert!(wait_until(|| seen.load(Ordering::SeqCst) == 3));
    }
}
