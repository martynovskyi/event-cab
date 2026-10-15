CREATE TABLE topics (
  name        TEXT PRIMARY KEY,
  status      TEXT NOT NULL DEFAULT 'pending'
              CHECK (status IN ('pending', 'approved', 'blocked')),
  mode        TEXT NOT NULL DEFAULT 'append'
              CHECK (mode IN ('append', 'toggle')),
  key_field   TEXT,                       -- for toggle: field name in data (e.g. 'type')
  public      BOOLEAN NOT NULL DEFAULT false,
  created_ip_hash TEXT,                   -- client key hash of the creator, never exposed
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  CONSTRAINT toggle_needs_key CHECK (mode <> 'toggle' OR key_field IS NOT NULL)
);

CREATE TABLE events (
  id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  topic        TEXT NOT NULL REFERENCES topics(name) ON DELETE CASCADE,
  resource_id  TEXT,
  resource_url TEXT,
  resource_key TEXT GENERATED ALWAYS AS (
                 COALESCE('id:' || resource_id, 'url:' || resource_url)
               ) STORED NOT NULL,
  visitor_id   TEXT NOT NULL,
  data         JSONB NOT NULL DEFAULT '{}',
  ip_hash      TEXT,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  CONSTRAINT resource_present CHECK (resource_id IS NOT NULL OR resource_url IS NOT NULL)
);

CREATE INDEX idx_topics_creator ON topics (created_ip_hash, created_at);

CREATE INDEX idx_events_key     ON events (topic, resource_key, visitor_id, id DESC);
CREATE INDEX idx_events_topic   ON events (topic, id);
CREATE INDEX idx_events_url     ON events (resource_url) WHERE resource_url IS NOT NULL;
CREATE INDEX idx_events_visitor ON events (visitor_id);
