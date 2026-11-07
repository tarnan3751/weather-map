-- The tile-shape trigger, made cheaper. It runs inside every write of an
-- alert's shape, and on the largest NWS zone unions (69,000 vertices) it
-- took about 200 ms; now about 15 ms. The time went on steps tiles do not
-- need:
--
-- * clipping to the Mercator latitude range is an overlay operation, and
--   only a shape reaching past 85 degrees needs it;
-- * topology-preserving simplification, and repairing its output: tiles
--   only need the shape drawn right, and ST_AsMVTGeom makes what goes into
--   a tile valid anyway, so plain Douglas-Peucker (keeping parts that
--   collapse) does. Point queries and notifications never use these
--   columns: they use the stored, valid `geom`.
--
-- Shapes stored by 0003 stay as they are; they draw the same.
CREATE OR REPLACE FUNCTION alerts_display_shapes() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    merc geometry;
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.geom IS NOT DISTINCT FROM OLD.geom AND OLD.geom_mvt IS NOT NULL THEN
        RETURN NEW;
    END IF;
    IF NEW.geom IS NULL THEN
        NEW.geom_mvt := NULL;
        NEW.geom_mvt_low := NULL;
        RETURN NEW;
    END IF;
    BEGIN
        NEW.anchor := ST_PointOnSurface(NEW.geom);
        IF ST_YMin(NEW.geom) < -85.0511287798 OR ST_YMax(NEW.geom) > 85.0511287798 THEN
            merc := ST_Transform(
                ST_Intersection(NEW.geom, ST_MakeEnvelope(-180, -85.0511287798, 180, 85.0511287798, 4326)), 3857);
        ELSE
            merc := ST_Transform(NEW.geom, 3857);
        END IF;
        -- About 1/4 pixel at zoom 12, and at zoom 5.
        NEW.geom_mvt := ST_Multi(ST_CollectionExtract(ST_Simplify(merc, 10, true), 3));
        NEW.geom_mvt_low := ST_Multi(ST_CollectionExtract(ST_Simplify(merc, 1200, true), 3));
    EXCEPTION WHEN OTHERS THEN
        -- A shape GEOS cannot handle is drawn as stored. Nothing here may
        -- fail: the alert itself must still be written.
        NEW.anchor := ST_Centroid(NEW.geom);
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
