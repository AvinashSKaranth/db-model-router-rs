CREATE TABLE likes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    post_id INTEGER NOT NULL,
    author_id INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    CONSTRAINT uq_post_author UNIQUE (post_id, author_id),
    FOREIGN KEY(post_id) REFERENCES posts(id),
    FOREIGN KEY(author_id) REFERENCES authors(id)
);
