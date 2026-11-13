//! The HTTP listener. It binds from the current settings and binds again when address, port
//! or encryption change, so the owner never has to restart the program by hand.

use std::{
    io,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use axum::Router;
use axum_server::Handle;
use hearth_api::settings::TlsMode;
use tokio::{net::TcpListener, sync::oneshot};

use crate::state::{AppState, Bound};

pub async fn run(state: AppState, app: Router, ready: oneshot::Sender<io::Result<SocketAddr>>) {
    let mut ready = Some(ready);
    // Tests ask for "any free port". Binding again must keep the one they were given.
    let mut picked_port = None;
    loop {
        let settings = state.settings();
        // Until an admin exists there is no certificate to speak of: the wizard runs over plain HTTP.
        let mode = if state.setup_complete() { settings.tls } else { TlsMode::Proxy };
        let port = match state.opts.port {
            Some(0) => picked_port.unwrap_or(0),
            Some(port) => port,
            None => settings.port,
        };

        let listener = match bind(state.opts.bind, port, state.opts.port.is_none()).await {
            Ok(listener) => listener,
            Err(err) => {
                match ready.take() {
                    Some(ready) => {
                        let _ = ready.send(Err(err));
                        return;
                    }
                    None => tracing::error!(port, error = %err, "could not listen, trying again in 5 seconds"),
                }
                if wait(&state, Duration::from_secs(5)).await {
                    return;
                }
                continue;
            }
        };
        let addr = listener.local_addr().expect("a bound listener has an address");
        picked_port = Some(addr.port());

        // The TLS setup, and the fingerprint clients pin when the certificate is the Hearth's own.
        let mut renewing = None;
        let tls = match mode {
            TlsMode::Proxy => None,
            TlsMode::SelfSigned => match crate::tls::self_signed(&state.paths, &settings).await {
                Ok(tls) => Some((tls.config, Some(tls.sha256))),
                Err(err) => {
                    tracing::error!(error = %err, "could not prepare the certificate, serving plain HTTP");
                    None
                }
            },
            TlsMode::LetsEncrypt => match crate::tls::lets_encrypt(&state.paths, &settings).await {
                Ok(acme) => {
                    renewing = Some(acme.task);
                    Some((acme.config, None))
                }
                Err(err) => {
                    tracing::error!(error = %err, "could not set up Let's Encrypt, serving plain HTTP");
                    None
                }
            },
        };
        state.set_bound(Bound { addr, https: tls.is_some(), cert_sha256: tls.as_ref().and_then(|(_, sha256)| sha256.clone()) });
        tracing::info!(%addr, https = tls.is_some(), "listening");
        if let Some(ready) = ready.take() {
            let _ = ready.send(Ok(addr));
        }

        let handle = Handle::new();
        let service = app.clone().into_make_service_with_connect_info::<SocketAddr>();
        let std_listener = listener.into_std().expect("a tokio listener converts to std");
        let server = {
            let handle = handle.clone();
            async move {
                match tls {
                    Some((config, _)) => axum_server::from_tcp_rustls(std_listener, config)?.handle(handle).serve(service).await,
                    None => axum_server::from_tcp(std_listener)?.handle(handle).serve(service).await,
                }
            }
        };
        tokio::pin!(server);

        let stopping = tokio::select! {
            result = &mut server => {
                if let Err(err) = result {
                    tracing::error!(error = %err, "the listener stopped, starting it again");
                }
                wait(&state, Duration::from_secs(2)).await
            }
            _ = state.rebind.notified() => {
                tracing::info!("settings changed, binding again");
                handle.graceful_shutdown(Some(Duration::from_secs(3)));
                let _ = server.await;
                false
            }
            _ = state.shutdown.cancelled() => {
                handle.graceful_shutdown(Some(Duration::from_secs(1)));
                let _ = server.await;
                true
            }
        };
        // The certificate is renewed by a task of its own, which ends with the listener it served.
        if let Some(renewing) = renewing {
            renewing.abort();
        }
        if stopping {
            return;
        }
    }
}

/// Sleeps, and says whether the Hearth is shutting down.
async fn wait(state: &AppState, time: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(time) => false,
        _ = state.shutdown.cancelled() => true,
    }
}

/// Binds the wanted port. If something else holds it and the port was not forced, the next
/// free one is taken, so a first start never dies on a busy port.
async fn bind(ip: IpAddr, port: u16, may_move: bool) -> io::Result<TcpListener> {
    let first = TcpListener::bind((ip, port)).await;
    let Err(err) = first else { return first };
    if !may_move || err.kind() != io::ErrorKind::AddrInUse {
        return Err(err);
    }
    for candidate in port.saturating_add(1)..=port.saturating_add(20) {
        if let Ok(listener) = TcpListener::bind((ip, candidate)).await {
            tracing::warn!("port {port} is in use, listening on {candidate} instead. Change the port under Settings.");
            return Ok(listener);
        }
    }
    Err(err)
}
