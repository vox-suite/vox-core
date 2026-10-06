CREATE TABLE pulse_goals (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title text NOT NULL CHECK(length(btrim(title)) BETWEEN 1 AND 120),
    kind text NOT NULL CHECK(kind IN ('metric','saving')),
    direction text NOT NULL DEFAULT 'at_least' CHECK(direction IN ('at_least','at_most')),
    period text CHECK(period IN ('week','month')),
    target double precision NOT NULL CHECK(target > 0 AND target < 1e12),
    unit text NOT NULL CHECK(length(btrim(unit)) BETWEEN 1 AND 16),
    definition jsonb CHECK(definition IS NULL OR jsonb_typeof(definition)='object'),
    starts_on date NOT NULL,
    deadline date,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK((kind='metric') = (definition IS NOT NULL))
);
CREATE INDEX pulse_goals_user_idx ON pulse_goals(user_id,created_at);
CREATE TABLE pulse_goal_entries (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    goal_id uuid NOT NULL REFERENCES pulse_goals(id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    amount double precision NOT NULL CHECK(amount BETWEEN -1e12 AND 1e12 AND amount <> 0),
    note text NOT NULL DEFAULT '' CHECK(length(note) <= 200),
    occurred_on date NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX pulse_goal_entries_goal_idx ON pulse_goal_entries(goal_id,occurred_on);
ALTER TABLE pulse_goals ENABLE ROW LEVEL SECURITY;
ALTER TABLE pulse_goal_entries ENABLE ROW LEVEL SECURITY;
CREATE POLICY pulse_goals_service ON pulse_goals FOR ALL TO service_role USING(true) WITH CHECK(true);
CREATE POLICY pulse_goal_entries_service ON pulse_goal_entries FOR ALL TO service_role USING(true) WITH CHECK(true);
