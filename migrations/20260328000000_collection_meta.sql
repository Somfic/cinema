CREATE TABLE IF NOT EXISTS collection_meta (
    id INTEGER PRIMARY KEY,
    slug TEXT NOT NULL,
    title TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'manual',
    created_at TEXT NOT NULL DEFAULT (CURRENT_TIMESTAMP),
    UNIQUE(slug)
);

INSERT INTO collection_meta (slug, title, kind) VALUES
    ('watchlist', 'Watchlist', 'manual'),
    ('favorites', 'Favorites', 'manual'),
    ('watched', 'Watched', 'manual')
ON CONFLICT(slug) DO NOTHING;
