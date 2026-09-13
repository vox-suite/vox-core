CREATE UNIQUE INDEX jobs_summary_unique_idx
    ON jobs (payload_reference_id)
    WHERE kind = 'summarize_conversation';
