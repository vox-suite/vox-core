-- Namespace imported identity by the connection while retaining every history ID,
-- local annotation, timestamp and encrypted account byte. Repeated sync must
-- update the imported row rather than insert a second copy after the upgrade.
UPDATE spans s SET source_ref=CASE
    WHEN s.source='playstation' AND s.source_ref LIKE 'history:%'
      THEN 'history:' || c.id::text || ':' || substring(s.source_ref FROM 9)
    ELSE c.id::text || ':' || s.source_ref END
FROM vox_connections c
WHERE s.user_id=c.user_id AND s.data->>'connection_id'=c.id::text
  AND s.source_ref IS NOT NULL AND s.source IN ('google_calendar','playstation','swiggy','zomato','spotify','youtube')
  AND s.source_ref NOT LIKE c.id::text || ':%'
  AND s.source_ref NOT LIKE 'history:' || c.id::text || ':%';

UPDATE inbound_events e SET source_id=CASE
    WHEN e.source_kind IN ('spotify','youtube') THEN c.id::text || ':' || e.source_kind
    WHEN e.source_kind='google_calendar' THEN c.id::text || substring(e.source_id FROM position(':' IN e.source_id))
    ELSE e.source_id END,
    external_event_id=CASE WHEN e.source_kind IN ('playstation','swiggy','zomato')
      AND e.external_event_id NOT LIKE c.id::text || ':%'
      THEN c.id::text || ':' || e.external_event_id ELSE e.external_event_id END
FROM vox_connections c
WHERE e.user_id=c.user_id
  AND COALESCE(e.payload->>'connection_id',e.payload->'activity'->>'connection_id')=c.id::text
  AND e.source_kind IN ('google_calendar','playstation','swiggy','zomato','spotify','youtube');
