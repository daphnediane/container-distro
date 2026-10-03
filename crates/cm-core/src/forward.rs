/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! A minimal userspace TCP forwarder: `127.0.0.1:<host>` → `<guest-ip>:<guest>`.
//!
//! Stands in for WSL's localhost forwarding. One thread per listener, two
//! per connection; std only.

use std::io;
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::str::FromStr;
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, bail};

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

/// Accept connections on `listener` forever, proxying each to `target`.
pub fn spawn_forwarder(listener: TcpListener, target: SocketAddr) -> JoinHandle<()> {
    thread::spawn(move || {
        for client in listener.incoming() {
            match client {
                Ok(client) => {
                    thread::spawn(move || {
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
}
