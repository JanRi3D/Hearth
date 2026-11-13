//! The live side of the Hearth: who is connected, and a way to hand each of them an event.
//! Nothing here is stored. After a restart, clients reconnect and presence builds up again.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use hearth_api::social::Event;
use tokio::sync::mpsc;

/// Events waiting for one connection. A client that falls this far behind is disconnected
/// rather than allowed to pile up memory; it reconnects and catches up by asking.
const BACKLOG: usize = 256;

struct Conn {
    id: u64,
    session_id: i64,
    device_id: Option<i64>,
    /// Events as JSON, ready to send. Shared, so a broadcast serializes once.
    tx: mpsc::Sender<Arc<str>>,
}

/// What a person told us about themselves, kept while they are connected.
#[derive(Debug, Clone, Default)]
pub struct Told {
    pub away: bool,
    pub detail: String,
    pub rich: BTreeMap<String, String>,
    /// When they came online.
    pub since: i64,
}

#[derive(Default)]
struct Inner {
    conns: HashMap<i64, Vec<Conn>>,
    told: HashMap<i64, Told>,
}

#[derive(Default)]
pub struct Hub {
    inner: Mutex<Inner>,
    next: AtomicU64,
}

fn encode(event: &Event) -> Arc<str> {
    serde_json::to_string(event).expect("events are plain data").into()
}

impl Hub {
    /// Registers a connection. Returns its id, the stream of events for it, and whether this
    /// is the person's first connection, which means they just came online.
    pub fn connect(&self, user_id: i64, session_id: i64, device_id: Option<i64>, now: i64) -> (u64, mpsc::Receiver<Arc<str>>, bool) {
        let (tx, rx) = mpsc::channel(BACKLOG);
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();
        let conns = inner.conns.entry(user_id).or_default();
        let first = conns.is_empty();
        conns.push(Conn { id, session_id, device_id, tx });
        if first {
            inner.told.insert(user_id, Told { since: now, ..Told::default() });
        }
        (id, rx, first)
    }

    /// Forgets a connection. Returns whether it was the person's last, which means they went offline.
    pub fn disconnect(&self, user_id: i64, conn_id: u64) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let Some(conns) = inner.conns.get_mut(&user_id) else { return false };
        conns.retain(|conn| conn.id != conn_id);
        if !conns.is_empty() {
            return false;
        }
        inner.conns.remove(&user_id);
        inner.told.remove(&user_id);
        true
    }

    fn deliver(conns: &mut Vec<Conn>, json: &Arc<str>, only: impl Fn(&Conn) -> bool) {
        // A connection whose backlog is full, or whose socket is gone, is dropped. Dropping
        // the sender ends its task, which closes the socket.
        conns.retain(|conn| !only(conn) || conn.tx.try_send(json.clone()).is_ok());
    }

    pub fn send(&self, user_id: i64, event: &Event) {
        let json = encode(event);
        if let Some(conns) = self.inner.lock().unwrap().conns.get_mut(&user_id) {
            Self::deliver(conns, &json, |_| true);
        }
    }

    /// Sends to everyone connected, except `skip`.
    pub fn send_all(&self, event: &Event, skip: Option<i64>) {
        let json = encode(event);
        for (user_id, conns) in self.inner.lock().unwrap().conns.iter_mut() {
            if Some(*user_id) != skip {
                Self::deliver(conns, &json, |_| true);
            }
        }
    }

    /// Sends to one device of a person. Returns whether that device is connected.
    pub fn send_device(&self, user_id: i64, device_id: i64, event: &Event) -> bool {
        let json = encode(event);
        let mut inner = self.inner.lock().unwrap();
        let Some(conns) = inner.conns.get_mut(&user_id) else { return false };
        let reached = conns.iter().any(|conn| conn.device_id == Some(device_id));
        Self::deliver(conns, &json, |conn| conn.device_id == Some(device_id));
        reached
    }

    pub fn is_online(&self, user_id: i64) -> bool {
        self.inner.lock().unwrap().conns.get(&user_id).is_some_and(|conns| !conns.is_empty())
    }

    pub fn online(&self) -> Vec<i64> {
        self.inner.lock().unwrap().conns.iter().filter(|(_, conns)| !conns.is_empty()).map(|(user_id, _)| *user_id).collect()
    }

    pub fn device_online(&self, device_id: i64) -> bool {
        self.inner.lock().unwrap().conns.values().flatten().any(|conn| conn.device_id == Some(device_id))
    }

    /// Whether this connection is still registered. A connection that was kicked is not.
    pub fn has(&self, user_id: i64, conn_id: u64) -> bool {
        self.inner.lock().unwrap().conns.get(&user_id).is_some_and(|conns| conns.iter().any(|conn| conn.id == conn_id))
    }

    /// Closes the connections of one session, for a device that was signed out.
    pub fn kick_session(&self, session_id: i64) {
        for conns in self.inner.lock().unwrap().conns.values_mut() {
            conns.retain(|conn| conn.session_id != session_id);
        }
    }

    /// Closes every connection of a person, for an account that was removed.
    pub fn kick_user(&self, user_id: i64) {
        if let Some(conns) = self.inner.lock().unwrap().conns.get_mut(&user_id) {
            conns.clear();
        }
    }

    pub fn told(&self, user_id: i64) -> Option<Told> {
        self.inner.lock().unwrap().told.get(&user_id).cloned()
    }

    /// Changes what a connected person told us. Does nothing for someone who is offline.
    pub fn tell(&self, user_id: i64, change: impl FnOnce(&mut Told)) {
        if let Some(told) = self.inner.lock().unwrap().told.get_mut(&user_id) {
            change(told);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_reach_the_right_people_and_slow_connections_are_dropped() {
        let hub = Hub::default();
        let (_, mut robin_desktop, first) = hub.connect(1, 10, Some(100), 0);
        assert!(first);
        let (laptop_id, mut robin_laptop, first) = hub.connect(1, 11, Some(101), 0);
        assert!(!first, "a second device does not bring someone online again");
        let (_, mut noor, _) = hub.connect(2, 20, Some(200), 0);

        hub.send(1, &Event::Pong);
        assert!(robin_desktop.try_recv().is_ok() && robin_laptop.try_recv().is_ok() && noor.try_recv().is_err());
        hub.send_all(&Event::Pong, Some(1));
        assert!(noor.try_recv().is_ok() && robin_desktop.try_recv().is_err());
        assert!(hub.send_device(1, 101, &Event::Pong));
        assert!(robin_laptop.try_recv().is_ok() && robin_desktop.try_recv().is_err());
        assert!(!hub.send_device(1, 999, &Event::Pong));

        // Signing a device out closes its connection: the receiver sees the end of the stream.
        hub.kick_session(11);
        assert!(!hub.has(1, laptop_id));
        assert!(matches!(robin_laptop.try_recv(), Err(mpsc::error::TryRecvError::Disconnected)));
        assert!(hub.is_online(1));

        // Someone who never reads is cut off when the backlog is full.
        for _ in 0..=BACKLOG {
            hub.send(2, &Event::Pong);
        }
        assert!(!hub.is_online(2) && !hub.device_online(200));
        assert_eq!(hub.online(), [1]);

        assert!(!hub.disconnect(1, 999), "an unknown connection changes nothing");
        hub.tell(1, |told| told.away = true);
        assert!(hub.told(1).unwrap().away);
        let desktop_id = 0;
        assert!(hub.disconnect(1, desktop_id), "the last connection gone means offline");
        assert!(hub.told(1).is_none());
    }
}
