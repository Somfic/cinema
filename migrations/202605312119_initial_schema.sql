CREATE TYPE media_type AS ENUM ('movie', 'tv');

CREATE TABLE IF NOT EXISTS watch_history
(
    id           SERIAL PRIMARY KEY,
    media_type   media_type               NOT NULL,
    tmdb_id      BIGINT                   NOT NULL,
    title        VARCHAR(255)             NOT NULL,
    poster_path  VARCHAR(255),
    season       INTEGER                  NOT NULL DEFAULT 0,
    episode      INTEGER                  NOT NULL DEFAULT 0,
    info_hash    VARCHAR(63),
    file_idx     INTEGER                  NOT NULL DEFAULT 0,
    progress     REAL                     NOT NULL DEFAULT 0,
    duration     REAL                     NOT NULL DEFAULT 0,
    last_watched TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT (CURRENT_TIMESTAMP),
    UNIQUE (media_type, tmdb_id)
);


CREATE TABLE IF NOT EXISTS collections
(
    id          SERIAL PRIMARY KEY,
    collection  VARCHAR(255)             NOT NULL,
    media_type  media_type               NOT NULL,
    tmdb_id     BIGINT                   NOT NULL,
    title       VARCHAR(255)             NOT NULL,
    poster_path VARCHAR(255)             NOT NULL,
    position    INTEGER                  NOT NULL DEFAULT 0,
    added_at    TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT (CURRENT_TIMESTAMP),
    UNIQUE (collection, media_type, tmdb_id)
);

CREATE TYPE download_status AS ENUM ('queued', 'downloading', 'paused', 'completed', 'cancelled', 'failed');

CREATE TABLE IF NOT EXISTS downloads
(
    id               SERIAL PRIMARY KEY,
    media_type       media_type               NOT NULL,
    tmdb_id          BIGINT                   NOT NULL,
    title            VARCHAR(255)             NOT NULL,
    poster_path      VARCHAR(255),
    season           INTEGER                  NOT NULL DEFAULT 0,
    episode          INTEGER                  NOT NULL DEFAULT 0,
    resolution       VARCHAR(15),
    info_hash        VARCHAR(63)              NOT NULL,
    file_idx         INTEGER                  NOT NULL,
    file_path        VARCHAR(255)             NOT NULL,
    total_bytes      BIGINT,
    downloaded_bytes BIGINT                   NOT NULL DEFAULT 0,
    status           download_status          NOT NULL DEFAULT 'queued',
    error            TEXT,
    created_at       TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT (CURRENT_TIMESTAMP),
    completed_at     TIMESTAMP WITH TIME ZONE,
    UNIQUE (media_type, tmdb_id, season, episode)
);


CREATE TYPE collection_kind AS ENUM ('manual', 'ordered');

CREATE TABLE IF NOT EXISTS collection_meta
(
    id         SERIAL PRIMARY KEY,
    slug       VARCHAR(255)             NOT NULL,
    title      VARCHAR(255)             NOT NULL,
    kind       collection_kind          NOT NULL DEFAULT 'manual',
    position   INTEGER                  NOT NULL DEFAULT 0,
    system     BOOLEAN                  NOT NULL DEFAULT FALSE,
    hidden     BOOLEAN                  NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT (CURRENT_TIMESTAMP),
    UNIQUE (slug)
);

INSERT INTO collection_meta (slug, title, kind, system, position)
VALUES ('watchlist', 'Watchlist', 'manual', true, 0),
       ('favorites', 'Favorites', 'manual', true, 0),
       ('watched', 'Watched', 'manual', true, 0),
       ('continue', 'Continue watching', 'manual', true, -1)
ON CONFLICT(slug) DO NOTHING;
