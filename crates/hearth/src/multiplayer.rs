//! Lobbies, peer-to-peer sessions and session tickets. All of it lives in memory: it belongs
//! to people who are connected right now, and is gone with a restart like the games themselves.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    net::SocketAddr,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use hearth_api::multiplayer::{Compare, Lobby, LobbyFilter, LobbyKind, LobbyMember, LobbyPatch, LobbySearch, P2pSession, Route, TicketInfo};

use crate::tls::hex;

pub type Token = [u8; 16];

/// A session nobody sent a packet or request for in this long is closed.
pub const P2P_IDLE: Duration = Duration::from_secs(300);
const TICKET_SECONDS: i64 = 24 * 3600;

pub struct LobbyState {
    pub lobby: Lobby,
    /// People who may join although the lobby is private.
    pub invited: HashSet<i64>,
}

struct Side {
    user_id: i64,
    token: Token,
    /// Where the relay last heard from this side.
    addr: Option<SocketAddr>,
}

struct P2p {
    id: i64,
    game_id: i64,
    lobby_id: Option<i64>,
    sides: [Side; 2],
    route: Route,
    last_used: Instant,
}

impl P2p {
    fn view(&self, user_id: i64, relay_port: Option<u16>) -> Option<P2pSession> {
        let mine = self.sides.iter().position(|side| side.user_id == user_id)?;
        Some(P2pSession {
            id: self.id,
            peer_id: self.sides[1 - mine].user_id,
            game_id: self.game_id,
            lobby_id: self.lobby_id,
            token: hex(&self.sides[mine].token),
            relay_port,
            route: self.route,
        })
    }
}

#[derive(Default)]
struct P2pTable {
    sessions: HashMap<i64, P2p>,
    /// Token to (session, which side), for the relay's lookups.
    tokens: HashMap<Token, (i64, usize)>,
}

struct TicketEntry {
    user_id: i64,
    username: String,
    game_id: i64,
    expires_at: i64,
}

/// What the relay should do with a packet.
pub enum Relayed {
    /// The token is not known. The packet is dropped without an answer.
    Unknown,
    /// The other side has not been heard from yet, so there is nowhere to send it.
    NoPeer,
    Forward { to: SocketAddr, token: Token },
}

#[derive(Default)]
pub struct Multiplayer {
    lobbies: Mutex<HashMap<i64, LobbyState>>,
    p2p: Mutex<P2pTable>,
    tickets: Mutex<HashMap<String, TicketEntry>>,
    /// The UDP port the relay is bound to, or 0 when it is not listening.
    relay_port: AtomicU64,
    pub relay_bytes: AtomicU64,
    /// Bits per second through the relay over the last minute, and the byte count it was worked out from.
    relay_rate: AtomicU64,
    relay_bytes_seen: AtomicU64,
    calls: Mutex<Calls>,
}

/// A short memory of what the Steam API replacement asked for, for the admin's networking page.
#[derive(Default)]
struct Calls {
    recent: VecDeque<hearth_api::admin::Call>,
    /// The day the counters are for, as days since 1970.
    day: i64,
    today: u64,
    failed: u64,
}

fn new_id() -> i64 {
    // Random, so an id cannot be guessed, and below 2^52 so it survives any JSON parser.
    rand::random_range(1..(1i64 << 52))
}

fn clip_pairs(pairs: BTreeMap<String, String>) -> BTreeMap<String, String> {
    pairs.into_iter().filter(|(key, _)| !key.is_empty() && key.len() <= 255).map(|(key, value)| (key, value.chars().take(8192).collect())).collect()
}

impl Multiplayer {
    // ---------- lobbies ----------

    pub fn create_lobby(&self, game_id: i64, owner: LobbyMember, kind: LobbyKind, max_members: u32, data: BTreeMap<String, String>, now: i64) -> Lobby {
        let lobby = Lobby {
            id: new_id(),
            game_id,
            owner_id: owner.user_id,
            kind,
            max_members: max_members.clamp(1, 250),
            joinable: true,
            members: vec![owner],
            data: clip_pairs(data),
            created_at: now,
        };
        self.lobbies.lock().unwrap().insert(lobby.id, LobbyState { lobby: lobby.clone(), invited: HashSet::new() });
        lobby
    }

    pub fn lobby(&self, id: i64) -> Option<Lobby> {
        self.lobbies.lock().unwrap().get(&id).map(|state| state.lobby.clone())
    }

    pub fn lobbies(&self) -> Vec<Lobby> {
        self.lobbies.lock().unwrap().values().map(|state| state.lobby.clone()).collect()
    }

    /// Runs `change` on a lobby and returns the lobby as it is afterwards.
    pub fn with_lobby<T>(&self, id: i64, change: impl FnOnce(&mut LobbyState) -> T) -> Option<(T, Lobby)> {
        let mut lobbies = self.lobbies.lock().unwrap();
        let state = lobbies.get_mut(&id)?;
        let result = change(state);
        Some((result, state.lobby.clone()))
    }

    /// Takes someone out of a lobby. The lobby passes to the member who has been in it longest,
    /// and closes when the last one leaves. Returns the lobby as it is afterwards, if it still exists.
    pub fn leave_lobby(&self, id: i64, user_id: i64) -> Option<Option<Lobby>> {
        let mut lobbies = self.lobbies.lock().unwrap();
        let state = lobbies.get_mut(&id)?;
        let before = state.lobby.members.len();
        state.lobby.members.retain(|member| member.user_id != user_id);
        if state.lobby.members.len() == before {
            return None;
        }
        if state.lobby.members.is_empty() {
            lobbies.remove(&id);
            return Some(None);
        }
        if state.lobby.owner_id == user_id {
            state.lobby.owner_id = state.lobby.members[0].user_id;
        }
        Some(Some(state.lobby.clone()))
    }

    pub fn lobbies_of(&self, user_id: i64) -> Vec<i64> {
        self.lobbies.lock().unwrap().values().filter(|state| state.lobby.members.iter().any(|member| member.user_id == user_id)).map(|state| state.lobby.id).collect()
    }

    /// A lobby friends can join through this person, for their presence.
    pub fn joinable_lobby_of(&self, user_id: i64) -> Option<i64> {
        self.lobbies
            .lock()
            .unwrap()
            .values()
            .map(|state| &state.lobby)
            .find(|lobby| {
                lobby.members.iter().any(|member| member.user_id == user_id)
                    && matches!(lobby.kind, LobbyKind::Friends | LobbyKind::Public)
                    && lobby.joinable
                    && (lobby.members.len() as u32) < lobby.max_members
            })
            .map(|lobby| lobby.id)
    }

    pub fn search(&self, search: &LobbySearch) -> Vec<Lobby> {
        let mut found: Vec<Lobby> = self
            .lobbies
            .lock()
            .unwrap()
            .values()
            .map(|state| &state.lobby)
            .filter(|lobby| {
                lobby.game_id == search.game_id
                    && matches!(lobby.kind, LobbyKind::Friends | LobbyKind::Public)
                    && lobby.joinable
                    && lobby.max_members.saturating_sub(lobby.members.len() as u32) >= search.slots_available.max(1)
                    && search.filters.iter().all(|filter| passes(lobby, filter))
            })
            .cloned()
            .collect();
        // Closest to the wanted numbers first, then the oldest lobby, so results are stable.
        let distance = |lobby: &Lobby| -> f64 {
            search.near.iter().map(|near| lobby.data.get(&near.key).and_then(|value| value.parse::<f64>().ok()).map_or(f64::MAX / 4.0, |value| (value - near.value).abs())).sum()
        };
        found.sort_by(|a, b| distance(a).total_cmp(&distance(b)).then(a.created_at.cmp(&b.created_at)).then(a.id.cmp(&b.id)));
        found.truncate(search.limit.unwrap_or(50).clamp(1, 200) as usize);
        found
    }

    // ---------- peer to peer ----------

    pub fn relay_port(&self) -> Option<u16> {
        Some(self.relay_port.load(Ordering::Relaxed) as u16).filter(|port| *port != 0)
    }

    pub fn set_relay_port(&self, port: Option<u16>) {
        self.relay_port.store(port.unwrap_or(0) as u64, Ordering::Relaxed);
    }

    /// Opens a session between two people, or returns the one they already have for this game.
    /// The second value says whether it is new, which is when the peer has to be told.
    pub fn open_p2p(&self, user_id: i64, peer_id: i64, game_id: i64, lobby_id: Option<i64>, relay_port: Option<u16>) -> (P2pSession, bool) {
        let mut table = self.p2p.lock().unwrap();
        let existing = table.sessions.values_mut().find(|session| {
            session.game_id == game_id && session.sides.iter().any(|side| side.user_id == user_id) && session.sides.iter().any(|side| side.user_id == peer_id)
        });
        if let Some(session) = existing {
            session.last_used = Instant::now();
            return (session.view(user_id, relay_port).expect("the caller is a side"), false);
        }
        let token = || {
            let mut token = Token::default();
            rand::fill(&mut token);
            token
        };
        let session = P2p {
            id: new_id(),
            game_id,
            lobby_id,
            sides: [Side { user_id, token: token(), addr: None }, Side { user_id: peer_id, token: token(), addr: None }],
            route: Route::Pending,
            last_used: Instant::now(),
        };
        let view = session.view(user_id, relay_port).expect("the caller is a side");
        for (index, side) in session.sides.iter().enumerate() {
            table.tokens.insert(side.token, (session.id, index));
        }
        table.sessions.insert(session.id, session);
        (view, true)
    }

    /// A session as one of its two people sees it. `None` for anyone else.
    pub fn p2p(&self, id: i64, user_id: i64, relay_port: Option<u16>) -> Option<P2pSession> {
        let mut table = self.p2p.lock().unwrap();
        let session = table.sessions.get_mut(&id)?;
        session.last_used = Instant::now();
        session.view(user_id, relay_port)
    }

    pub fn set_route(&self, id: i64, user_id: i64, route: Route) -> bool {
        let mut table = self.p2p.lock().unwrap();
        match table.sessions.get_mut(&id).filter(|session| session.sides.iter().any(|side| side.user_id == user_id)) {
            Some(session) => {
                // What the relay saw wins: if packets went through it, the route is the relay.
                if session.route != Route::Relay {
                    session.route = route;
                }
                true
            }
            None => false,
        }
    }

    fn remove_p2p(table: &mut P2pTable, id: i64) -> Option<[i64; 2]> {
        let session = table.sessions.remove(&id)?;
        for side in &session.sides {
            table.tokens.remove(&side.token);
        }
        Some([session.sides[0].user_id, session.sides[1].user_id])
    }

    /// Closes a session. Returns the peer to tell, if the caller was part of it.
    pub fn close_p2p(&self, id: i64, user_id: i64) -> Option<i64> {
        let mut table = self.p2p.lock().unwrap();
        table.sessions.get(&id).filter(|session| session.sides.iter().any(|side| side.user_id == user_id))?;
        Self::remove_p2p(&mut table, id).map(|[a, b]| if a == user_id { b } else { a })
    }

    /// Closes the sessions that fit `gone` and returns (session, the two people) for each.
    fn close_p2p_where(&self, gone: impl Fn(&P2p) -> bool) -> Vec<(i64, [i64; 2])> {
        let mut table = self.p2p.lock().unwrap();
        let ids: Vec<i64> = table.sessions.values().filter(|session| gone(session)).map(|session| session.id).collect();
        ids.into_iter().filter_map(|id| Self::remove_p2p(&mut table, id).map(|people| (id, people))).collect()
    }

    pub fn close_idle_p2p(&self) -> Vec<(i64, [i64; 2])> {
        self.close_p2p_where(|session| session.last_used.elapsed() > P2P_IDLE)
    }

    pub fn close_p2p_of(&self, user_id: i64) -> Vec<(i64, [i64; 2])> {
        self.close_p2p_where(|session| session.sides.iter().any(|side| side.user_id == user_id))
    }

    /// (lobby, route) of every session, for the admin's list of open lobbies.
    pub fn routes(&self) -> Vec<(Option<i64>, Route)> {
        self.p2p.lock().unwrap().sessions.values().map(|session| (session.lobby_id, session.route)).collect()
    }

    pub fn p2p_count(&self) -> (usize, usize) {
        let table = self.p2p.lock().unwrap();
        (table.sessions.len(), table.sessions.values().filter(|session| session.route == Route::Relay).count())
    }

    /// The relay heard from a token. Remembers where from. Returns false for a token nobody holds.
    pub fn relay_heard(&self, token: &Token, from: SocketAddr) -> bool {
        let mut table = self.p2p.lock().unwrap();
        let Some(&(id, side)) = table.tokens.get(token) else { return false };
        let Some(session) = table.sessions.get_mut(&id) else { return false };
        session.sides[side].addr = Some(from);
        session.last_used = Instant::now();
        true
    }

    /// The relay got data for the other side of `token`'s session.
    pub fn relay_data(&self, token: &Token, from: SocketAddr, bytes: usize) -> Relayed {
        let mut table = self.p2p.lock().unwrap();
        let Some(&(id, side)) = table.tokens.get(token) else { return Relayed::Unknown };
        let Some(session) = table.sessions.get_mut(&id) else { return Relayed::Unknown };
        session.sides[side].addr = Some(from);
        session.last_used = Instant::now();
        let peer = &session.sides[1 - side];
        let Some(to) = peer.addr else { return Relayed::NoPeer };
        let token = peer.token;
        session.route = Route::Relay;
        self.relay_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        Relayed::Forward { to, token }
    }

    // ---------- figures for the admin ----------

    pub fn record_call(&self, interface: &str, call: &str, status: u16, millis: u32, now: i64) {
        let mut calls = self.calls.lock().unwrap();
        if calls.day != now / 86400 {
            (calls.day, calls.today, calls.failed) = (now / 86400, 0, 0);
        }
        calls.today += 1;
        if status >= 400 {
            calls.failed += 1;
        }
        calls.recent.push_front(hearth_api::admin::Call { at: now, interface: interface.into(), call: call.into(), status, millis });
        calls.recent.truncate(40);
    }

    /// (calls today, of which failed, the most recent ones).
    pub fn calls(&self, now: i64) -> (u64, u64, Vec<hearth_api::admin::Call>) {
        let calls = self.calls.lock().unwrap();
        let today = calls.day == now / 86400;
        (if today { calls.today } else { 0 }, if today { calls.failed } else { 0 }, calls.recent.iter().cloned().collect())
    }

    /// Called once a minute: turns the bytes relayed since the last call into a rate.
    pub fn sample_relay_rate(&self) {
        let total = self.relay_bytes.load(Ordering::Relaxed);
        let before = self.relay_bytes_seen.swap(total, Ordering::Relaxed);
        self.relay_rate.store(total.saturating_sub(before) * 8 / 60, Ordering::Relaxed);
    }

    pub fn relay_rate(&self) -> u64 {
        self.relay_rate.load(Ordering::Relaxed)
    }

    // ---------- tickets ----------

    pub fn issue_ticket(&self, user_id: i64, username: String, game_id: i64, now: i64) -> (String, i64) {
        let ticket = crate::auth::new_token();
        let expires_at = now + TICKET_SECONDS;
        let mut tickets = self.tickets.lock().unwrap();
        tickets.retain(|_, entry| entry.expires_at > now);
        tickets.insert(ticket.clone(), TicketEntry { user_id, username, game_id, expires_at });
        (ticket, expires_at)
    }

    pub fn verify_ticket(&self, ticket: &str, now: i64) -> Option<TicketInfo> {
        let tickets = self.tickets.lock().unwrap();
        let entry = tickets.get(ticket).filter(|entry| entry.expires_at > now)?;
        Some(TicketInfo { user_id: entry.user_id, username: entry.username.clone(), game_id: entry.game_id, owned: true })
    }

    /// Cancels a ticket. Only its owner can.
    pub fn cancel_ticket(&self, ticket: &str, user_id: i64) {
        let mut tickets = self.tickets.lock().unwrap();
        if tickets.get(ticket).is_some_and(|entry| entry.user_id == user_id) {
            tickets.remove(ticket);
        }
    }

    pub fn cancel_tickets_of(&self, user_id: i64) {
        self.tickets.lock().unwrap().retain(|_, entry| entry.user_id != user_id);
    }
}

/// Applies the owner's changes to a lobby. Fails with a sentence for the person when a value is not allowed.
pub fn patch_lobby(lobby: &mut Lobby, patch: LobbyPatch) -> Result<(), &'static str> {
    if let Some(owner_id) = patch.owner_id {
        if !lobby.members.iter().any(|member| member.user_id == owner_id) {
            return Err("Only a member of the lobby can become its owner.");
        }
        lobby.owner_id = owner_id;
    }
    if let Some(max) = patch.max_members {
        if !(1..=250).contains(&max) {
            return Err("A lobby holds 1 to 250 people.");
        }
        lobby.max_members = max;
    }
    if let Some(kind) = patch.kind {
        lobby.kind = kind;
    }
    if let Some(joinable) = patch.joinable {
        lobby.joinable = joinable;
    }
    for key in patch.delete {
        lobby.data.remove(&key);
    }
    lobby.data.extend(clip_pairs(patch.set));
    if lobby.data.len() > 200 {
        return Err("A lobby holds up to 200 pairs of data.");
    }
    Ok(())
}

pub fn clip_member_data(pairs: BTreeMap<String, String>) -> BTreeMap<String, String> {
    clip_pairs(pairs)
}

fn passes(lobby: &Lobby, filter: &LobbyFilter) -> bool {
    let Some(value) = lobby.data.get(&filter.key) else { return false };
    let ordering = if filter.numeric {
        match (value.parse::<f64>(), filter.value.parse::<f64>()) {
            (Ok(have), Ok(want)) => have.partial_cmp(&want),
            _ => None,
        }
    } else {
        Some(value.to_lowercase().cmp(&filter.value.to_lowercase()))
    };
    let Some(ordering) = ordering else { return false };
    match filter.compare {
        Compare::Equal => ordering.is_eq(),
        Compare::NotEqual => ordering.is_ne(),
        Compare::Less => ordering.is_lt(),
        Compare::LessOrEqual => ordering.is_le(),
        Compare::Greater => ordering.is_gt(),
        Compare::GreaterOrEqual => ordering.is_ge(),
    }
}

#[cfg(test)]
mod tests {
    use hearth_api::multiplayer::LobbyNear;

    use super::*;

    fn member(user_id: i64) -> LobbyMember {
        LobbyMember { user_id, username: format!("user{user_id}"), data: BTreeMap::new() }
    }

    fn pairs(list: &[(&str, &str)]) -> BTreeMap<String, String> {
        list.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect()
    }

    #[test]
    fn lobbies_pass_to_the_longest_member_and_close_when_empty() {
        let mp = Multiplayer::default();
        let lobby = mp.create_lobby(7, member(1), LobbyKind::Public, 4, BTreeMap::new(), 100);
        mp.with_lobby(lobby.id, |state| state.lobby.members.extend([member(2), member(3)])).unwrap();
        assert_eq!(mp.joinable_lobby_of(2), Some(lobby.id));

        let after = mp.leave_lobby(lobby.id, 1).unwrap().unwrap();
        assert_eq!((after.owner_id, after.members.len()), (2, 2), "the owner left, the next in line owns it");
        assert!(mp.leave_lobby(lobby.id, 1).is_none(), "leaving twice changes nothing");
        mp.leave_lobby(lobby.id, 2).unwrap();
        assert_eq!(mp.leave_lobby(lobby.id, 3), Some(None), "the last one out closes the lobby");
        assert!(mp.lobby(lobby.id).is_none() && mp.lobbies_of(3).is_empty());
    }

    #[test]
    fn search_filters_and_sorts_like_a_matchmaker() {
        let mp = Multiplayer::default();
        let a = mp.create_lobby(7, member(1), LobbyKind::Public, 4, pairs(&[("map", "Nauvis"), ("level", "10")]), 1);
        let b = mp.create_lobby(7, member(2), LobbyKind::Friends, 4, pairs(&[("map", "nauvis"), ("level", "30")]), 2);
        let c = mp.create_lobby(7, member(3), LobbyKind::Public, 1, pairs(&[("map", "Vulcanus"), ("level", "25")]), 3);
        let _private = mp.create_lobby(7, member(4), LobbyKind::Private, 4, pairs(&[("map", "Nauvis")]), 4);
        let _other_game = mp.create_lobby(8, member(5), LobbyKind::Public, 4, pairs(&[("map", "Nauvis")]), 5);

        let ids = |search: LobbySearch| mp.search(&search).iter().map(|lobby| lobby.id).collect::<Vec<_>>();
        let filter = |key: &str, value: &str, compare, numeric| LobbyFilter { key: key.into(), value: value.into(), compare, numeric };
        assert_eq!(ids(LobbySearch { game_id: 7, ..Default::default() }), [a.id, b.id], "private, full and other games' lobbies stay out");
        assert_eq!(ids(LobbySearch { game_id: 7, filters: vec![filter("map", "NAUVIS", Compare::Equal, false)], ..Default::default() }), [a.id, b.id]);
        assert_eq!(ids(LobbySearch { game_id: 7, filters: vec![filter("level", "9", Compare::Greater, true), filter("level", "20", Compare::Less, true)], ..Default::default() }), [a.id]);
        assert_eq!(ids(LobbySearch { game_id: 7, filters: vec![filter("level", "9", Compare::Greater, false)], ..Default::default() }), Vec::<i64>::new(), "as text, 10 and 30 sort before 9");
        assert_eq!(ids(LobbySearch { game_id: 7, filters: vec![filter("missing", "x", Compare::NotEqual, false)], ..Default::default() }), Vec::<i64>::new());
        assert_eq!(ids(LobbySearch { game_id: 7, near: vec![LobbyNear { key: "level".into(), value: 28.0 }], ..Default::default() }), [b.id, a.id]);
        assert_eq!(ids(LobbySearch { game_id: 7, slots_available: 3, limit: Some(1), ..Default::default() }), [a.id]);
        let _ = c;
    }

    #[test]
    fn the_relay_forwards_only_between_the_two_sides_of_a_session() {
        let mp = Multiplayer::default();
        let (robin, new) = mp.open_p2p(1, 2, 7, None, Some(7301));
        assert!(new);
        let (again, new) = mp.open_p2p(2, 1, 7, None, Some(7301));
        assert!(!new && again.id == robin.id && again.token != robin.token, "each side has its own token for the same session");
        assert!(mp.p2p(robin.id, 3, None).is_none(), "a third person sees nothing");

        let token = |hex: &str| -> Token { std::array::from_fn(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap()) };
        let (robin_token, noor_token) = (token(&robin.token), token(&again.token));
        let (robin_addr, noor_addr): (SocketAddr, SocketAddr) = ("10.0.0.1:5000".parse().unwrap(), "10.0.0.2:6000".parse().unwrap());
        assert!(matches!(mp.relay_data(&[9; 16], robin_addr, 10), Relayed::Unknown));
        assert!(matches!(mp.relay_data(&robin_token, robin_addr, 10), Relayed::NoPeer), "Noor has not been heard from yet");
        assert!(mp.relay_heard(&noor_token, noor_addr));
        match mp.relay_data(&robin_token, robin_addr, 10) {
            Relayed::Forward { to, token } => assert_eq!((to, token), (noor_addr, noor_token)),
            _ => panic!("the packet should be forwarded to Noor"),
        }
        assert_eq!(mp.p2p(robin.id, 1, None).unwrap().route, Route::Relay);
        assert!(mp.set_route(robin.id, 1, Route::Direct) && mp.p2p(robin.id, 1, None).unwrap().route == Route::Relay, "what the relay saw wins");
        assert_eq!(mp.relay_bytes.load(Ordering::Relaxed), 10);

        assert_eq!(mp.close_p2p(robin.id, 3), None);
        assert_eq!(mp.close_p2p(robin.id, 2), Some(1));
        assert!(!mp.relay_heard(&robin_token, robin_addr), "a closed session's tokens are worthless");
    }

    #[test]
    fn tickets_say_who_someone_is_until_they_expire_or_are_cancelled() {
        let mp = Multiplayer::default();
        let (ticket, expires_at) = mp.issue_ticket(1, "robin".into(), 7, 1000);
        assert_eq!(mp.verify_ticket(&ticket, 1001), Some(TicketInfo { user_id: 1, username: "robin".into(), game_id: 7, owned: true }));
        assert_eq!(mp.verify_ticket("made up", 1001), None);
        assert_eq!(mp.verify_ticket(&ticket, expires_at), None);
        mp.cancel_ticket(&ticket, 2);
        assert!(mp.verify_ticket(&ticket, 1001).is_some(), "only the owner cancels a ticket");
        mp.cancel_ticket(&ticket, 1);
        assert!(mp.verify_ticket(&ticket, 1001).is_none());
    }
}
