-- Zone boundaries are refreshed every 90 days. A refresh attempt is recorded
-- whatever its outcome: a zone the NWS has retired answers 404 for ever, and
-- without this the oldest such zones would be picked again on every sweep and
-- hold up refreshing all the others.
ALTER TABLE zones ADD COLUMN refresh_attempted_at timestamptz;

-- Zones an alert names that were left out of its shape because they could not
-- be fetched (yet): the shape is then the union of the zones that are known,
-- and is redone when the missing ones arrive. Empty for a complete shape.
ALTER TABLE alerts ADD COLUMN zones_missing text[] NOT NULL DEFAULT '{}';
