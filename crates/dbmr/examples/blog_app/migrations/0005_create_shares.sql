CREATE TABLE shares (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    post_id INTEGER NOT NULL,
    author_id INTEGER NOT NULL,
    platform TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY(post_id) REFERENCES posts(id),
    FOREIGN KEY(author_id) REFERENCES authors(id)
);
