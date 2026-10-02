CREATE TABLE pending_phone_links (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    normalized_phone TEXT NOT NULL CHECK (length(btrim(normalized_phone)) > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX pending_phone_links_phone_idx ON pending_phone_links (normalized_phone);

ALTER TABLE pending_phone_links ENABLE ROW LEVEL SECURITY;

DO $$
DECLARE
    fk record;
BEGIN
    FOR fk IN
        SELECT DISTINCT c.conrelid::regclass::text AS tbl, c.conname
        FROM pg_constraint c
        JOIN pg_attribute pa ON pa.attrelid = c.confrelid AND pa.attnum = ANY (c.confkey)
        WHERE c.contype = 'f'
          AND c.connamespace = 'public'::regnamespace
          AND pa.attname = 'user_id'
          AND NOT c.condeferrable
    LOOP
        EXECUTE format(
            'ALTER TABLE %s ALTER CONSTRAINT %I DEFERRABLE INITIALLY IMMEDIATE',
            fk.tbl, fk.conname
        );
    END LOOP;
END
$$;

CREATE FUNCTION merge_user_accounts(old_user UUID, new_user UUID) RETURNS VOID
LANGUAGE plpgsql
SET search_path = public, pg_temp
AS $$
DECLARE
    old_ctx UUID;
    new_ctx UUID;
    fk record;
    set_parts TEXT[];
    where_parts TEXT[];
    set_clause TEXT;
    where_clause TEXT;
    parent_col TEXT;
    child_col TEXT;
    idx INT;
    row_ref record;
BEGIN
    IF old_user = new_user THEN
        RETURN;
    END IF;

    PERFORM 1 FROM users WHERE id IN (old_user, new_user) ORDER BY id FOR UPDATE;
    SELECT id INTO old_ctx FROM user_contexts WHERE user_id = old_user;
    SELECT id INTO new_ctx FROM user_contexts WHERE user_id = new_user;
    IF new_ctx IS NULL THEN
        RAISE EXCEPTION 'target user % has no context', new_user;
    END IF;

    SET CONSTRAINTS ALL DEFERRED;

    FOR fk IN
        SELECT c.conrelid::regclass::text AS tbl,
               CASE
                   WHEN c.confrelid = 'users'::regclass THEN 'users'
                   WHEN c.confrelid = 'user_contexts'::regclass THEN 'ctx'
                   ELSE 'other'
               END AS kind,
               array_agg(a.attname::text ORDER BY k.ord) AS child_cols,
               array_agg(pa.attname::text ORDER BY k.ord) AS parent_cols
        FROM pg_constraint c
        CROSS JOIN LATERAL unnest(c.conkey, c.confkey) WITH ORDINALITY AS k(child_att, parent_att, ord)
        JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.child_att
        JOIN pg_attribute pa ON pa.attrelid = c.confrelid AND pa.attnum = k.parent_att
        WHERE c.contype = 'f'
          AND c.connamespace = 'public'::regnamespace
          AND c.conrelid NOT IN ('users'::regclass, 'user_contexts'::regclass)
        GROUP BY c.oid, c.conrelid, c.confrelid
        HAVING c.confrelid IN ('users'::regclass, 'user_contexts'::regclass)
            OR 'user_id' = ANY (array_agg(pa.attname::text))
        ORDER BY
            CASE
                WHEN c.confrelid = 'user_contexts'::regclass
                     AND 'user_id' = ANY (array_agg(pa.attname::text)) THEN 0
                WHEN c.confrelid = 'user_contexts'::regclass THEN 1
                WHEN c.confrelid = 'users'::regclass THEN 2
                ELSE 3
            END,
            c.conrelid::regclass::text
    LOOP
        set_parts := ARRAY[]::TEXT[];
        where_parts := ARRAY[]::TEXT[];
        FOR idx IN 1 .. cardinality(fk.child_cols) LOOP
            child_col := fk.child_cols[idx];
            parent_col := fk.parent_cols[idx];
            IF fk.kind = 'users' THEN
                set_parts := set_parts || format('%I = %L::uuid', child_col, new_user);
                where_parts := where_parts || format('%I = %L::uuid', child_col, old_user);
            ELSIF fk.kind = 'ctx' AND parent_col = 'id' THEN
                set_parts := set_parts || format('%I = %L::uuid', child_col, new_ctx);
                where_parts := where_parts || format('%I = %L::uuid', child_col, old_ctx);
            ELSIF parent_col = 'user_id' THEN
                set_parts := set_parts || format('%I = %L::uuid', child_col, new_user);
                where_parts := where_parts || format('%I = %L::uuid', child_col, old_user);
            END IF;
        END LOOP;
        CONTINUE WHEN cardinality(set_parts) = 0;
        set_clause := array_to_string(set_parts, ', ');
        where_clause := array_to_string(where_parts, ' AND ');

        BEGIN
            EXECUTE format('UPDATE %s SET %s WHERE %s', fk.tbl, set_clause, where_clause);
        EXCEPTION WHEN unique_violation THEN
            FOR row_ref IN EXECUTE format('SELECT ctid AS ref FROM %s WHERE %s', fk.tbl, where_clause) LOOP
                BEGIN
                    EXECUTE format('UPDATE %s SET %s WHERE ctid = $1', fk.tbl, set_clause) USING row_ref.ref;
                EXCEPTION WHEN unique_violation THEN
                    EXECUTE format('DELETE FROM %s WHERE ctid = $1', fk.tbl) USING row_ref.ref;
                END;
            END LOOP;
        END;
    END LOOP;

    SET CONSTRAINTS ALL IMMEDIATE;

    DELETE FROM user_contexts WHERE user_id = old_user;
    DELETE FROM users WHERE id = old_user;
END
$$;
