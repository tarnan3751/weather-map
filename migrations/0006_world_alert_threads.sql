-- World alert threading, second round.
--
-- * Within one source a reference also matches on the identifier alone
--   (some senders refer to their messages under another sender address),
--   and messages that reference the same message belong together: both
--   need the referenced identifiers on their own.
-- * A message whose blocks all say the hazard is over (urgency Past, an
--   AllClear response, a cancellation block) ends its thread, as a Cancel
--   does.
ALTER TABLE alerts
    ADD COLUMN cap_reference_ids text[] NOT NULL DEFAULT '{}',
    ADD COLUMN in_effect boolean NOT NULL DEFAULT true;

UPDATE alerts
SET cap_reference_ids = ARRAY(SELECT DISTINCT substr(r, strpos(r, ',') + 1) FROM unnest(cap_references) r)
WHERE origin = 'cap' AND cardinality(cap_references) > 0;

-- Messages stored before blocks were read this way: those whose every
-- block is past or a cancellation. (AllClear was not kept; ECCC sends it
-- with urgency Past, which was.) The server re-threads at start and ends
-- them then.
UPDATE alerts
SET in_effect = false
WHERE origin = 'cap' AND infos IS NOT NULL AND jsonb_array_length(infos) > 0
  AND NOT EXISTS (
      SELECT 1 FROM jsonb_array_elements(infos) b
      WHERE (b->>'urgency') IS DISTINCT FROM 'Past'
        AND lower(ltrim(coalesce(b->>'headline', ''))) NOT LIKE 'cancel%');

CREATE INDEX alerts_cap_reference_ids_idx ON alerts USING gin (cap_reference_ids) WHERE origin = 'cap';
CREATE INDEX alerts_cap_identifier_idx ON alerts (source, cap_identifier) WHERE origin = 'cap';
