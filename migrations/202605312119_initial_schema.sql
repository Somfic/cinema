DO
$$
    BEGIN
        CREATE TYPE media_type AS ENUM ('movie', 'tv');
        CREATE TYPE download_status AS ENUM ('queued', 'downloading', 'paused', 'completed', 'cancelled', 'failed');
        CREATE TYPE collection_kind AS ENUM ('manual', 'ordered');
        CREATE TYPE transcoding_option AS ENUM ('enabled', 'only-audio', 'disabled');
    EXCEPTION
        WHEN duplicate_object THEN null;
    END
$$;

CREATE TABLE IF NOT EXISTS media_items
(
    id          SERIAL PRIMARY KEY,
    media_type  media_type               NOT NULL,
    tmdb_id     BIGINT                   NOT NULL,
    title       VARCHAR(255)             NOT NULL,
    poster_path VARCHAR(255),
    updated_at  TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (media_type, tmdb_id)
);

CREATE TABLE IF NOT EXISTS downloads
(
    id               SERIAL PRIMARY KEY,
    info_hash        VARCHAR(63)              NOT NULL,
    file_idx         INTEGER                  NOT NULL,

    name             VARCHAR(512),
    total_bytes      BIGINT,
    downloaded_bytes BIGINT                   NOT NULL DEFAULT 0,
    status           download_status          NOT NULL DEFAULT 'queued',
    error            TEXT,
    created_at       TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at     TIMESTAMP WITH TIME ZONE,

    UNIQUE (info_hash, file_idx)
);

CREATE TABLE IF NOT EXISTS download_meta
(
    info_hash  VARCHAR(63) NOT NULL,
    file_idx   INTEGER     NOT NULL,

    media_id   INTEGER     NOT NULL REFERENCES media_items (id) ON DELETE CASCADE,
    season     INTEGER,
    episode    INTEGER,
    resolution VARCHAR(15),

    PRIMARY KEY (info_hash, file_idx)
);

CREATE TABLE IF NOT EXISTS watch_history
(
    media_id     INTEGER PRIMARY KEY REFERENCES media_items (id) ON DELETE CASCADE,
    download_id  INTEGER                  REFERENCES downloads (id) ON DELETE SET NULL,
    season       INTEGER                  NOT NULL DEFAULT 0,
    episode      INTEGER                  NOT NULL DEFAULT 0,
    progress     REAL                     NOT NULL DEFAULT 0,
    duration     REAL                     NOT NULL DEFAULT 0,
    transcoding  transcoding_option       NOT NULL DEFAULT 'disabled',
    last_watched TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS collection_meta
(
    id         SERIAL PRIMARY KEY,
    slug       VARCHAR(255)             NOT NULL,
    title      VARCHAR(255)             NOT NULL,
    kind       collection_kind          NOT NULL DEFAULT 'manual',
    position   INTEGER                  NOT NULL DEFAULT 0,
    system     BOOLEAN                  NOT NULL DEFAULT FALSE,
    hidden     BOOLEAN                  NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (slug)
);

CREATE TABLE IF NOT EXISTS collections
(
    collection_slug VARCHAR(255)             NOT NULL REFERENCES collection_meta (slug) ON DELETE CASCADE,
    media_id        INTEGER                  NOT NULL REFERENCES media_items (id) ON DELETE CASCADE,
    position        INTEGER                  NOT NULL DEFAULT 0,
    added_at        TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (collection_slug, media_id)
);

INSERT INTO collection_meta (slug, title, kind, system, position)
VALUES ('watchlist', 'Watchlist', 'manual', true, 0),
       ('favorites', 'Favorites', 'manual', true, 0),
       ('watched', 'Watched', 'manual', true, 0),
       ('continue', 'Continue watching', 'manual', true, -1)
ON CONFLICT(slug) DO NOTHING;
