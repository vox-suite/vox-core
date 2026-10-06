DELETE FROM spans WHERE source = 'location';
DELETE FROM data_source_consents WHERE data_source = 'location';
ALTER TABLE data_source_consents DROP CONSTRAINT IF EXISTS data_source_consents_data_source_check;
ALTER TABLE data_source_consents ADD CONSTRAINT data_source_consents_data_source_check CHECK (data_source IN ('sms'));
