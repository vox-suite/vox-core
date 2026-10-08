INSERT INTO data_schemas (user_id, namespace, name, version, description, json_schema, color_token, icon_token)
SELECT NULL, 'finance', seed.name, 1, seed.description, '{"type":"object","properties":{"amount":{"type":"number"},"currency":{"type":"string"},"direction":{"type":"string","enum":["credit","debit","due","info"]},"merchant":{"type":"string"},"account_hint":{"type":"string"},"reference":{"type":"string"},"status":{"type":"string"},"due_date":{"type":"string"},"occurred_at":{"type":"string"}}}'::jsonb, seed.color_token, seed.icon_token
FROM (VALUES
    ('bank_credit', 'Money credited to a bank account: salary, transfers received, deposits, interest. Use for any SMS saying an amount was credited or received.', 8, 0),
    ('bank_debit', 'Money debited from a bank account that is not a card or UPI purchase: ATM withdrawals, NEFT/IMPS transfers sent, fees and charges.', 0, 0),
    ('upi_payment', 'A completed UPI payment sent to a person or merchant, including the payee and UPI reference.', 14, 0),
    ('card_payment', 'A debit or credit card purchase or transaction at a merchant, online or in store.', 16, 13),
    ('autopay_mandate_setup', 'A recurring payment, autopay, e-mandate or subscription being registered, authorised or changed; no money has moved yet.', 4, 0),
    ('subscription_charge', 'A recurring or subscription charge that was actually collected, such as a streaming, app store or utility autopay deduction.', 18, 0),
    ('bill_due', 'An upcoming bill, EMI, loan or card payment that is due, with an amount and due date.', 2, 0),
    ('card_bill_payment', 'A payment received towards a credit card bill, confirming how much was paid and the remaining balance.', 12, 0),
    ('reward_points', 'Loyalty or reward points earned, redeemed or expiring on a card or programme.', 6, 0),
    ('investment_activity', 'Investment account activity: mutual fund SIP or redemption, IPO allotment or credit, broker balance or contract notes.', 20, 0),
    ('refund', 'Money returned to the user for a cancelled order, failed payment or reversed transaction.', 10, 0)
) AS seed(name, description, color_token, icon_token)
WHERE NOT EXISTS (
    SELECT 1 FROM data_schemas existing
    WHERE existing.user_id IS NULL AND existing.namespace = 'finance' AND existing.name = seed.name
);
