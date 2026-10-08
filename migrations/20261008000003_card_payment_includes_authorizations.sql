UPDATE data_schemas
SET description = 'A debit or credit card purchase or transaction at a merchant, online or in store. Includes OTP or authorisation messages that state the amount and merchant of a card payment; those count as spend and are merged with the later debit message.'
WHERE user_id IS NULL AND namespace = 'finance' AND name = 'card_payment';
