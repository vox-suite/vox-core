-- One-time cleanup of money items the event agent saved as free text: canonical categories,
-- structured amount and currency, and completed payments marked done. Safe to re-run.

UPDATE spans
SET category = CASE
        WHEN category ~* 'subscri' THEN 'subscription'
        WHEN category ~* 'bill' THEN 'bill'
        ELSE 'expense'
    END
WHERE source = 'event_agent'
  AND category ~* '(subscri|bill|expens|spend|purchase|payment|paid)'
  AND category NOT IN ('subscription', 'bill', 'expense');

DROP TABLE IF EXISTS event_agent_money;
CREATE TEMP TABLE event_agent_money AS
SELECT s.id,
       s.user_id,
       replace(m[2], ',', '')::numeric AS amount,
       (s.title || ' ' || s.notes) ~* '(spent|paid|debited|charged|completed|successful)'
           AND (s.title || ' ' || s.notes) !~* '(due|will be|upcoming|overdue|failed|reminder|is active)' AS paid_like,
       COALESCE(s.start_at, s.created_at) AS at
FROM spans s
CROSS JOIN LATERAL (
    SELECT regexp_match(s.title || ' ' || s.notes, '(rs\.?|inr|₹)\s*([0-9][0-9,]*(?:\.[0-9]+)?)', 'i') AS m
) parsed
WHERE s.source = 'event_agent'
  AND NOT (s.data ? 'amount')
  AND s.category IN ('bill', 'subscription', 'expense')
  AND parsed.m IS NOT NULL;

-- A payment already stored from the bank message is not stored a second time.
UPDATE spans s
SET data = s.data || jsonb_build_object('duplicate_of', o.id::text),
    version = s.version + 1,
    updated_at = now()
FROM event_agent_money e
JOIN LATERAL (
    SELECT o.id
    FROM spans o
    WHERE o.user_id = e.user_id
      AND o.source = 'sms'
      AND CASE WHEN jsonb_typeof(o.data -> 'amount') = 'number' THEN round((o.data ->> 'amount')::numeric, 2) END = round(e.amount, 2)
      AND abs(extract(epoch FROM (COALESCE(o.start_at, o.created_at) - e.at))) < 129600
    ORDER BY o.created_at
    LIMIT 1
) o ON true
WHERE s.id = e.id AND e.paid_like;

UPDATE spans s
SET data = s.data || jsonb_build_object(
        'amount', e.amount,
        'currency', 'INR',
        'direction', CASE WHEN e.paid_like THEN 'debit' ELSE 'due' END),
    status = CASE WHEN e.paid_like AND s.status = 'planned' THEN 'done' ELSE s.status END,
    completed_at = CASE WHEN e.paid_like AND s.status = 'planned' THEN COALESCE(s.completed_at, now()) ELSE s.completed_at END,
    version = s.version + 1,
    updated_at = now()
FROM event_agent_money e
WHERE s.id = e.id
  AND COALESCE(s.data ->> 'duplicate_of', '') = '';

DROP TABLE event_agent_money;
