-- Chat and notifications. Presence is not stored: it follows from who is connected.

CREATE TABLE messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    sender_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    recipient_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    body         TEXT NOT NULL,
    sent_at      INTEGER NOT NULL,
    read_at      INTEGER
);
CREATE INDEX messages_pair ON messages(sender_id, recipient_id, id);
CREATE INDEX messages_inbox ON messages(recipient_id, read_at);

CREATE TABLE notifications (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    title      TEXT NOT NULL,
    body       TEXT NOT NULL DEFAULT '',
    -- JSON object of ids the notification is about.
    data       TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    read_at    INTEGER
);
CREATE INDEX notifications_user ON notifications(user_id, id);
