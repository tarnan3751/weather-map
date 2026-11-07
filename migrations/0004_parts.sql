-- The catalog now remembers parts (one source's data for one layer at one
-- valid time); timelines of frames are rebuilt from them at startup.

DROP TABLE IF EXISTS frames;

CREATE TABLE parts (
    layer       text             NOT NULL,
    id          text             NOT NULL,
    source      text             NOT NULL,
    valid       timestamptz      NOT NULL,
    run         timestamptz,
    kind        text             NOT NULL CHECK (kind IN ('observed', 'forecast')),
    west        double precision NOT NULL,
    south       double precision NOT NULL,
    east        double precision NOT NULL,
    north       double precision NOT NULL,
    -- The solid box, all four set or all four null.
    solid_west  double precision,
    solid_south double precision,
    solid_east  double precision,
    solid_north double precision,
    max_zoom    smallint         NOT NULL,
    PRIMARY KEY (layer, id)
);
