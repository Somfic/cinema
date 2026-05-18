INSERT INTO collection_meta (slug, title, kind, system, position) VALUES
    ('continue', 'Continue watching', 'manual', 1, -1)
ON CONFLICT(slug) DO NOTHING;
