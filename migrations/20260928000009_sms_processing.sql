CREATE TABLE sms_processed (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    digest TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('written', 'merged', 'otp', 'irrelevant')),
    span_id UUID REFERENCES spans(id) ON DELETE SET NULL,
    processed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, digest)
);

CREATE INDEX spans_sms_fingerprint_idx
    ON spans (user_id, (data->>'fingerprint'))
    WHERE source = 'sms';

CREATE INDEX spans_sms_category_idx
    ON spans (user_id, category)
    WHERE source = 'sms';
