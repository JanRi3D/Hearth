//! The Bonfire relay: one UDP socket that tells a client its public address, carries
//! peer-to-peer traffic when no direct path works, and answers discovery broadcasts so
//! clients on the same network find the Hearth without typing an address.

use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use hearth_api::multiplayer::{Discovered, RELAY_MAGIC, RELAY_MAX_PAYLOAD, relay_packet};
use tokio::net::UdpSocket;

use crate::{
    auth::is_private,
    multiplayer::{Relayed, Token},
    routes::VERSION,
    state::AppState,
};

const HEADER: usize = RELAY_MAGIC.len() + 1;
const TOKEN: usize = 16;

/// Runs until shutdown. Binds again when the relay port in Settings changes.
pub async fn relay(state: AppState) {
    let mut settings = state.watch_settings();
    loop {
        let port = state.opts.relay_port.unwrap_or(settings.borrow_and_update().relay_port);
        let socket = match UdpSocket::bind((state.opts.bind, port)).await {
            Ok(socket) => socket,
            Err(err) => {
                tracing::error!(port, error = %err, "the relay could not listen. Pick another relay port under Settings");
                state.multiplayer.set_relay_port(None);
                tokio::select! {
                    _ = settings.changed() => continue,
                    _ = tokio::time::sleep(Duration::from_secs(30)) => continue,
                    _ = state.shutdown.cancelled() => return,
                }
            }
        };
        let bound = socket.local_addr().map(|addr| addr.port()).unwrap_or(port);
        state.multiplayer.set_relay_port(Some(bound));
        tracing::info!(port = bound, "relay listening");

        // One datagram more than the largest packet the relay accepts, so oversized ones are seen as such.
        let mut buffer = [0u8; HEADER + TOKEN + RELAY_MAX_PAYLOAD + 1];
        loop {
            tokio::select! {
                received = socket.recv_from(&mut buffer) => {
                    // An error here is usually an ICMP "port unreachable" from a peer that left. Nothing to do.
                    if let Ok((length, from)) = received {
                        handle(&state, &socket, &buffer[..length], from).await;
                    }
                }
                changed = settings.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    if state.opts.relay_port.is_none() && settings.borrow().relay_port != bound {
                        break;
                    }
                }
                _ = state.shutdown.cancelled() => {
                    state.multiplayer.set_relay_port(None);
                    return;
                }
            }
        }
    }
}

async fn handle(state: &AppState, socket: &UdpSocket, packet: &[u8], from: SocketAddr) {
    if packet.len() < HEADER || packet[..RELAY_MAGIC.len()] != RELAY_MAGIC {
        return;
    }
    let body = &packet[HEADER..];
    match packet[RELAY_MAGIC.len()] {
        relay_packet::DISCOVER => {
            // Only neighbours get an answer, so this cannot be used to bounce traffic at strangers.
            if is_private(from.ip()) && state.setup_complete() {
                let _ = socket.send_to(&discovery_reply(state), from).await;
            }
        }
        // Without a valid token there is no answer at all: the relay never talks to strangers.
        _ if !state.settings().relay_enabled => {}
        relay_packet::PROBE if body.len() == TOKEN => {
            let token: Token = body.try_into().expect("length checked");
            if state.multiplayer.relay_heard(&token, from) {
                let _ = socket.send_to(&probe_reply(from), from).await;
            }
        }
        relay_packet::DATA if body.len() > TOKEN && body.len() <= TOKEN + RELAY_MAX_PAYLOAD => {
            let token: Token = body[..TOKEN].try_into().expect("length checked");
            if let Relayed::Forward { to, token } = state.multiplayer.relay_data(&token, from, body.len() - TOKEN) {
                let mut out = Vec::with_capacity(packet.len());
                out.extend_from_slice(&RELAY_MAGIC);
                out.push(relay_packet::DATA);
                out.extend_from_slice(&token);
                out.extend_from_slice(&body[TOKEN..]);
                let _ = socket.send_to(&out, to).await;
            }
        }
        _ => {}
    }
}

/// The address the relay saw a packet come from, which is the sender's public address.
fn probe_reply(from: SocketAddr) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + 19);
    out.extend_from_slice(&RELAY_MAGIC);
    out.push(relay_packet::PROBE_REPLY);
    match from.ip() {
        IpAddr::V4(ip) => {
            out.push(4);
            out.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            out.push(6);
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&from.port().to_be_bytes());
    out
}

fn discovery_reply(state: &AppState) -> Vec<u8> {
    let bound = state.bound();
    let found = Discovered {
        name: state.settings().name.clone(),
        version: VERSION.into(),
        port: bound.as_ref().map_or(state.settings().port, |bound| bound.addr.port()),
        https: bound.as_ref().is_some_and(|bound| bound.https),
        cert_sha256: bound.and_then(|bound| bound.cert_sha256),
    };
    let mut out = Vec::new();
    out.extend_from_slice(&RELAY_MAGIC);
    out.push(relay_packet::DISCOVER_REPLY);
    out.extend_from_slice(&serde_json::to_vec(&found).expect("plain data"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_reply_spells_out_the_address() {
        let reply = probe_reply("203.0.113.9:40000".parse().unwrap());
        assert_eq!(&reply[..4], b"BNF1");
        assert_eq!(reply[4..], [relay_packet::PROBE_REPLY, 4, 203, 0, 113, 9, 0x9c, 0x40]);
        let reply = probe_reply("[2001:db8::1]:7301".parse().unwrap());
        assert_eq!((reply[5], reply.len()), (6, 4 + 1 + 1 + 16 + 2));
    }
}
