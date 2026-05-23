ALTER TABLE collection_meta ADD COLUMN system INTEGER NOT NULL DEFAULT 0;
ALTER TABLE collection_meta ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;
UPDATE collection_meta SET system = 1 WHERE slug IN ('watchlist', 'favorites', 'watched');
