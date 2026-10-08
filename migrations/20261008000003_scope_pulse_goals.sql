-- Goals introduced after platform recovery retain host ownership. Ambiguous
-- legacy rows survive but remain unavailable until explicit reassociation.
ALTER TABLE pulse_goals ADD COLUMN user_context_id UUID;
UPDATE pulse_goals g SET user_context_id=(SELECT min(id::text)::uuid FROM user_contexts u WHERE u.user_id=g.user_id)
WHERE (SELECT count(*) FROM user_contexts u WHERE u.user_id=g.user_id)=1;
ALTER TABLE pulse_goals ADD CONSTRAINT pulse_goals_context_owner FOREIGN KEY(user_context_id,user_id) REFERENCES user_contexts(id,user_id) DEFERRABLE INITIALLY IMMEDIATE;
ALTER TABLE pulse_goals ADD CONSTRAINT pulse_goals_context_identity UNIQUE(id,user_context_id,user_id);
CREATE TRIGGER pulse_goals_context BEFORE INSERT OR UPDATE OF user_id,user_context_id ON pulse_goals FOR EACH ROW EXECUTE FUNCTION resource_context_compatibility();
CREATE INDEX pulse_goals_context_created ON pulse_goals(user_context_id,created_at,id);
ALTER TABLE pulse_goal_entries ADD COLUMN user_context_id UUID;
UPDATE pulse_goal_entries e SET user_context_id=g.user_context_id FROM pulse_goals g WHERE e.goal_id=g.id AND e.user_id=g.user_id;
ALTER TABLE pulse_goal_entries ADD CONSTRAINT pulse_goal_entries_scoped_goal FOREIGN KEY(goal_id,user_context_id,user_id) REFERENCES pulse_goals(id,user_context_id,user_id) ON DELETE CASCADE DEFERRABLE INITIALLY IMMEDIATE;
CREATE TRIGGER pulse_goal_entries_context BEFORE INSERT OR UPDATE OF user_id,user_context_id ON pulse_goal_entries FOR EACH ROW EXECUTE FUNCTION resource_context_compatibility();
