-- TODO [32]: full-text search over `messages.content`.
--
-- We use the `simple` configuration (no language-specific stemming or stop
-- word removal) because chat content is mixed CJK/English/emoji and any
-- linguistic normaliser would mangle tokens unevenly. `simple` indexes raw
-- whitespace-separated tokens — good enough for the substring + prefix
-- recall the search endpoint needs.
--
-- The GIN index is built on the `to_tsvector('simple', content)`
-- *expression* (no generated column) — keeps the schema migration narrow
-- and matches the query the handler emits.

CREATE INDEX idx_messages_content_fts
  ON messages
  USING GIN (to_tsvector('simple', content));
