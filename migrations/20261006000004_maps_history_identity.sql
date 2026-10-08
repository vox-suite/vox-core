-- Maps imports landed after the recovery history migration was published.
UPDATE spans s SET source_ref=c.id::text||':'||s.source_ref
FROM vox_connections c WHERE s.user_id=c.user_id AND s.data->>'connection_id'=c.id::text
 AND s.source='google_maps' AND s.source_ref IS NOT NULL
 AND s.source_ref NOT LIKE c.id::text||':%';
UPDATE inbound_events e SET source_id=c.id::text||':google_maps'
FROM vox_connections c WHERE e.user_id=c.user_id AND e.source_kind='google_maps'
 AND COALESCE(e.payload->>'connection_id',e.payload->'activity'->>'connection_id')=c.id::text;
