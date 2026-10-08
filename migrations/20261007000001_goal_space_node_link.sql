ALTER TABLE pulse_goals ADD COLUMN space_node_id uuid;
CREATE UNIQUE INDEX pulse_goals_space_node_idx ON pulse_goals(space_node_id) WHERE space_node_id IS NOT NULL;
