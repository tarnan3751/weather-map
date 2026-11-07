-- ---------------------------------------------------------------------------
-- Alerts from the rest of the world (CAP feeds listed in the WMO register)
-- share the alerts table with the NWS path. The defaults describe NWS rows,
-- so the NWS code needs no change to fill these columns.
-- ---------------------------------------------------------------------------
ALTER TABLE alerts
    ADD COLUMN origin         text NOT NULL DEFAULT 'nws' CHECK (origin IN ('nws', 'cap')),
    -- WMO register sourceId ('de-dwd-en'); 'us-nws' for the NWS path.
    ADD COLUMN source         text NOT NULL DEFAULT 'us-nws',
    -- ISO 3166-1 alpha-2 of the issuing authority, lower case.
    ADD COLUMN country        text DEFAULT 'us',
    -- Language of the info block shown (English when the message has one).
    ADD COLUMN language       text NOT NULL DEFAULT 'en-US',
    -- CAP category of that block, lower case ('met', 'geo', 'fire', ...).
    ADD COLUMN category       text,
    -- Every info block of the message, as plain text (detail endpoint).
    ADD COLUMN infos          jsonb,
    ADD COLUMN cap_sender     text,
    ADD COLUMN cap_identifier text,
    -- 'sender,identifier': what other messages reference this one by.
    ADD COLUMN cap_key        text,
    -- Keys of the messages this one references.
    ADD COLUMN cap_references text[] NOT NULL DEFAULT '{}',
    -- Sender, event and area fingerprint: the thread key fallback for
    -- feeds that never use references.
    ADD COLUMN area_key       text,
    -- 'VALUENAME:value' geocodes the message names ('NUTS3:BG332').
    ADD COLUMN region_codes   text[] NOT NULL DEFAULT '{}',
    -- The CAP document's URL.
    ADD COLUMN cap_url        text,
    -- The feed no longer lists the document (for feeds that list current
    -- alerts only, that ends it).
    ADD COLUMN unlisted       boolean NOT NULL DEFAULT false,
    -- Where an alert is listed: a point on its shape, or for an alert with
    -- no shape the register's point for its country.
    ADD COLUMN anchor         geometry(Point, 4326),
    -- Shapes in Web Mercator for vector tiles, clipped to the Mercator
    -- range and lightly simplified (and a coarse copy for low zooms). Kept
    -- by the trigger below whatever path writes geom.
    ADD COLUMN geom_mvt       geometry(MultiPolygon, 3857),
    ADD COLUMN geom_mvt_low   geometry(MultiPolygon, 3857);

ALTER TABLE alerts DROP CONSTRAINT alerts_geom_source_check;
ALTER TABLE alerts ADD CONSTRAINT alerts_geom_source_check
    CHECK (geom_source IN ('polygon', 'zones', 'cap', 'nuts', 'none'));

CREATE UNIQUE INDEX alerts_cap_key_idx ON alerts (cap_key) WHERE origin = 'cap';
CREATE INDEX alerts_cap_references_idx ON alerts USING gin (cap_references) WHERE origin = 'cap';
CREATE INDEX alerts_area_key_idx ON alerts (area_key) WHERE origin = 'cap' AND active;
CREATE INDEX alerts_source_idx ON alerts (source) WHERE active;
CREATE INDEX alerts_mvt_idx ON alerts USING gist (geom_mvt) WHERE active;
CREATE INDEX alerts_anchor_idx ON alerts USING gist (anchor) WHERE active;

CREATE FUNCTION alerts_display_shapes() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    clipped geometry;
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.geom IS NOT DISTINCT FROM OLD.geom AND OLD.geom_mvt IS NOT NULL THEN
        RETURN NEW;
    END IF;
    IF NEW.geom IS NULL THEN
        NEW.geom_mvt := NULL;
        NEW.geom_mvt_low := NULL;
        RETURN NEW;
    END IF;
    NEW.anchor := ST_PointOnSurface(NEW.geom);
    BEGIN
        clipped := ST_Transform(
            ST_Intersection(NEW.geom, ST_MakeEnvelope(-180, -85.0511287798, 180, 85.0511287798, 4326)), 3857);
        -- About 1/4 pixel at zoom 12, and at zoom 5.
        NEW.geom_mvt := ST_Multi(ST_CollectionExtract(ST_MakeValid(ST_SimplifyPreserveTopology(clipped, 10)), 3));
        NEW.geom_mvt_low := ST_Multi(ST_CollectionExtract(ST_MakeValid(ST_SimplifyPreserveTopology(clipped, 1200)), 3));
    EXCEPTION WHEN OTHERS THEN
        -- A shape GEOS cannot simplify is drawn as stored.
        NEW.geom_mvt := ST_Multi(ST_CollectionExtract(ST_Transform(
            ST_ClipByBox2D(NEW.geom, ST_MakeEnvelope(-180, -85.0511287798, 180, 85.0511287798, 4326)), 3857), 3));
        NEW.geom_mvt_low := NEW.geom_mvt;
    END;
    IF ST_IsEmpty(NEW.geom_mvt) THEN
        NEW.geom_mvt := NULL;
    END IF;
    IF NEW.geom_mvt_low IS NULL OR ST_IsEmpty(NEW.geom_mvt_low) THEN
        NEW.geom_mvt_low := NEW.geom_mvt;
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER alerts_display_shapes
    BEFORE INSERT OR UPDATE OF geom ON alerts
    FOR EACH ROW EXECUTE FUNCTION alerts_display_shapes();

-- Shapes stored before this migration get theirs now.
UPDATE alerts SET geom = geom WHERE geom IS NOT NULL;

-- ---------------------------------------------------------------------------
-- The alert set's revision: raised whenever what the map shows changes, so
-- tile URLs that carry it can be cached for ever. Never goes backwards.
-- ---------------------------------------------------------------------------
CREATE TABLE alerts_revision (
    singleton   boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    revision    bigint      NOT NULL,
    changed_at  timestamptz NOT NULL DEFAULT now()
);
INSERT INTO alerts_revision (revision) VALUES (1);

-- ---------------------------------------------------------------------------
-- The WMO register as last fetched, so a restart does not depend on its host.
-- ---------------------------------------------------------------------------
CREATE TABLE alert_register (
    singleton      boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    body           bytea       NOT NULL,
    etag           text,
    last_modified  text,
    fetched_at     timestamptz NOT NULL
);

-- ---------------------------------------------------------------------------
-- CAP documents fetched, so each is fetched once: a message never changes
-- under its identifier. Failed fetches are retried on a schedule.
-- ---------------------------------------------------------------------------
CREATE TABLE cap_documents (
    url         text PRIMARY KEY,
    source      text        NOT NULL,
    -- Nothing more to do with it (stored, already known, or ignored).
    done        boolean     NOT NULL,
    attempts    integer     NOT NULL DEFAULT 0,
    retry_at    timestamptz,
    outcome     text,
    fetched_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX cap_documents_fetched_idx ON cap_documents (fetched_at);

-- ---------------------------------------------------------------------------
-- NUTS 3 region boundaries (Eurostat GISCO, 1:1 million), cached the first
-- time an alert names a region, as NWS zones are.
-- Licence: non-commercial use, credited "(c) EuroGeographics for the
-- administrative boundaries".
-- ---------------------------------------------------------------------------
CREATE TABLE nuts_regions (
    code        text PRIMARY KEY,             -- 'BG332'
    edition     smallint    NOT NULL,         -- NUTS edition it was found in
    name        text,
    geom        geometry(MultiPolygon, 4326) NOT NULL,
    fetched_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX nuts_regions_geom_idx ON nuts_regions USING gist (geom);
