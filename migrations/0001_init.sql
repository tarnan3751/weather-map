CREATE EXTENSION IF NOT EXISTS postgis;

-- ---------------------------------------------------------------------------
-- Frames: the catalog of tile sets currently on a layer's timeline. Mirrors
-- the in-memory FrameCatalog so a restart picks up where it left off.
-- ---------------------------------------------------------------------------
CREATE TABLE frames (
    layer       text        NOT NULL,
    id          text        NOT NULL,
    valid       timestamptz NOT NULL,
    kind        text        NOT NULL CHECK (kind IN ('observed', 'forecast')),
    source      text        NOT NULL,
    run         timestamptz,
    max_zoom    smallint    NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (layer, id)
);
CREATE INDEX frames_layer_valid_idx ON frames (layer, valid);

-- ---------------------------------------------------------------------------
-- NWS zones. Many alerts name forecast zones or counties instead of carrying
-- a polygon; their boundaries are cached here the first time they are needed.
-- ---------------------------------------------------------------------------
CREATE TABLE zones (
    id          text PRIMARY KEY,               -- UGC code: 'TXZ123' (zone), 'TXC201' (county)
    kind        text        NOT NULL,           -- forecast | county | fire | marine | ...
    name        text,
    state       text,
    geom        geometry(MultiPolygon, 4326) NOT NULL,
    fetched_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX zones_geom_idx ON zones USING gist (geom);

-- ---------------------------------------------------------------------------
-- Alerts from the NWS active-alerts feed.
-- ---------------------------------------------------------------------------
CREATE TABLE alerts (
    id            text PRIMARY KEY,             -- NWS id, 'urn:oid:2.49.0.1.840.0....'
    -- Identifies the hazard across its updates (from VTEC: office, phenomenon,
    -- significance, event number, year). Falls back to `id` when an alert has
    -- no VTEC. Notifications are sent once per event_key per place.
    event_key     text        NOT NULL,
    event         text        NOT NULL,         -- 'Tornado Warning'
    severity      text,
    urgency       text,
    certainty     text,
    status        text,
    message_type  text,
    headline      text,
    description   text,
    instruction   text,
    area_desc     text,
    sender_name   text,
    sent          timestamptz,
    effective     timestamptz,
    onset         timestamptz,
    expires       timestamptz,
    ends          timestamptz,
    ugc           text[]      NOT NULL DEFAULT '{}',
    geom          geometry(MultiPolygon, 4326), -- NULL until zones are resolved
    geom_source   text        NOT NULL DEFAULT 'none'
                  CHECK (geom_source IN ('polygon', 'zones', 'none')),
    active        boolean     NOT NULL DEFAULT true,
    first_seen    timestamptz NOT NULL DEFAULT now(),
    last_seen     timestamptz NOT NULL DEFAULT now(),
    ended_at      timestamptz,
    raw           jsonb
);
CREATE INDEX alerts_geom_idx ON alerts USING gist (geom);
CREATE INDEX alerts_active_idx ON alerts (active) WHERE active;
CREATE INDEX alerts_event_key_idx ON alerts (event_key);

-- ---------------------------------------------------------------------------
-- Devices, saved places and push subscriptions. There are no accounts: a
-- browser registers once and keeps a bearer token; only its hash is stored.
-- ---------------------------------------------------------------------------
CREATE TABLE devices (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    token_hash    bytea       NOT NULL UNIQUE,  -- SHA-256 of the bearer token
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_seen_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE places (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    device_id   uuid        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    name        text        NOT NULL,
    geom        geometry(Point, 4326) NOT NULL,
    notify      boolean     NOT NULL DEFAULT true,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX places_geom_idx ON places USING gist (geom);
CREATE INDEX places_device_idx ON places (device_id);

CREATE TABLE push_subscriptions (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    device_id        uuid        NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    endpoint         text        NOT NULL UNIQUE,
    p256dh           text        NOT NULL,      -- base64url, as the browser gives it
    auth             text        NOT NULL,      -- base64url
    user_agent       text,
    created_at       timestamptz NOT NULL DEFAULT now(),
    last_success_at  timestamptz,
    failure_count    integer     NOT NULL DEFAULT 0
);
CREATE INDEX push_subscriptions_device_idx ON push_subscriptions (device_id);

-- One row per (hazard, place): the primary key is what stops a warning that
-- gets updated five times from notifying five times.
CREATE TABLE notifications (
    event_key   text        NOT NULL,
    place_id    uuid        NOT NULL REFERENCES places (id) ON DELETE CASCADE,
    alert_id    text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    sent_at     timestamptz,
    status      text        NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'sent', 'failed', 'no_subscription')),
    error       text,
    PRIMARY KEY (event_key, place_id)
);
