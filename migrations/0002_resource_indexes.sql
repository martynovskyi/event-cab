-- Summaries filter on the stored resource columns (by id, by URL, or both) instead of resource_key.
CREATE INDEX idx_events_topic_rid ON events (topic, resource_id, visitor_id, id DESC) WHERE resource_id IS NOT NULL;
CREATE INDEX idx_events_topic_url ON events (topic, resource_url, visitor_id, id DESC) WHERE resource_url IS NOT NULL;
DROP INDEX idx_events_key;
