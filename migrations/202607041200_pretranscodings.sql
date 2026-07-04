DO
$$
    BEGIN
        CREATE TYPE pretranscoding_status AS ENUM ('queued', 'transcoding', 'completed', 'cancelled', 'failed');
    EXCEPTION
        WHEN duplicate_object THEN null;
    END
$$;

CREATE TABLE IF NOT EXISTS pretranscodings
(
    id             SERIAL PRIMARY KEY,
    download_id    INTEGER                  NOT NULL REFERENCES downloads (id) ON DELETE CASCADE,
    audio_index    INTEGER                  NOT NULL,
    only_audio     BOOLEAN                  NOT NULL,

    transcoded_ms  BIGINT                   NOT NULL DEFAULT 0,
    total_ms       BIGINT,
    status         pretranscoding_status    NOT NULL DEFAULT 'queued',
    error          TEXT,
    created_at     TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at   TIMESTAMP WITH TIME ZONE,

    UNIQUE (download_id, only_audio, audio_index)
);

CREATE INDEX IF NOT EXISTS pretranscodings_status_idx ON pretranscodings (status);
CREATE INDEX IF NOT EXISTS pretranscodings_download_id_idx ON pretranscodings (download_id);
