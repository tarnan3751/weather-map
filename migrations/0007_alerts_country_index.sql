-- `GET /api/alerts?country=..` lists one country's active alerts; without an
-- index that is a scan of every stored alert (with its large `raw` column).
CREATE INDEX IF NOT EXISTS alerts_country_idx ON alerts (country) WHERE active;
