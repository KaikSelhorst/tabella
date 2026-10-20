-- Northstar Global sample data, part 2: invoicing, general ledger, budgets, support, marketing and
-- the audit trail. Runs after 02-seed.sql (uses its `seed` helpers, dropped at the end).

\set ON_ERROR_STOP on
SET client_min_messages = warning;
SET session_replication_role = replica;
SET work_mem = '256MB';
-- Docker's small default /dev/shm can't hold big parallel hash joins.
SET max_parallel_workers_per_gather = 0;
\o /dev/null
SELECT setseed(0.1717);

-- ---------------------------------------------------------------- finance: invoices

-- Business customers are invoiced; some invoices are still open, a few are overdue.
INSERT INTO finance.invoices (invoice_number, order_id, customer_id, issued_on, due_on, amount, currency_code, paid_on)
SELECT 'INV-' || to_char(o.ordered_at, 'YYYY') || '-' || lpad((row_number() OVER (ORDER BY o.id))::text, 7, '0'),
       o.id, o.customer_id, o.ordered_at::date, o.ordered_at::date + t.days, o.total, o.currency_code,
       CASE WHEN o.ordered_at::date + t.days > date '2026-10-14' AND random() < 0.6 THEN NULL
            WHEN random() < 0.03 THEN NULL
            ELSE least(o.ordered_at::date + seed.rint(3, t.days + 20), date '2026-10-14') END
FROM sales.orders o
JOIN sales.customers c ON c.id = o.customer_id
CROSS JOIN LATERAL (SELECT (array[30, 30, 45, 60])[seed.rint(1, 4) + (o.id * 0)::int] AS days) t
WHERE c.segment <> 'consumer' AND o.status NOT IN ('cancelled', 'pending');

-- ---------------------------------------------------------------- finance: chart of accounts

INSERT INTO finance.accounts (code, name, type, parent_code) VALUES
    ('1000', 'Assets', 'asset', NULL),
    ('1010', 'Cash and Cash Equivalents', 'asset', '1000'),
    ('1200', 'Accounts Receivable', 'asset', '1000'),
    ('1300', 'Inventory', 'asset', '1000'),
    ('1500', 'Property, Plant & Equipment', 'asset', '1000'),
    ('2000', 'Liabilities', 'liability', NULL),
    ('2100', 'Accounts Payable', 'liability', '2000'),
    ('2200', 'Sales Tax Payable', 'liability', '2000'),
    ('2300', 'Accrued Payroll', 'liability', '2000'),
    ('3000', 'Equity', 'equity', NULL),
    ('3100', 'Retained Earnings', 'equity', '3000'),
    ('4000', 'Revenue', 'revenue', NULL),
    ('4100', 'Product Sales', 'revenue', '4000'),
    ('4200', 'Shipping Revenue', 'revenue', '4000'),
    ('4900', 'Sales Returns & Allowances', 'revenue', '4000'),
    ('5000', 'Cost of Goods Sold', 'expense', NULL),
    ('5100', 'COGS - Merchandise', 'expense', '5000'),
    ('6000', 'Operating Expenses', 'expense', NULL),
    ('6100', 'Salaries & Wages', 'expense', '6000'),
    ('6150', 'Employee Benefits', 'expense', '6000'),
    ('6200', 'Rent & Facilities', 'expense', '6000'),
    ('6300', 'Utilities', 'expense', '6000'),
    ('6400', 'Travel & Entertainment', 'expense', '6000'),
    ('6500', 'Software & Subscriptions', 'expense', '6000'),
    ('6600', 'Advertising & Marketing', 'expense', '6000'),
    ('6700', 'Professional Services', 'expense', '6000'),
    ('6800', 'Depreciation', 'expense', '6000');

CREATE TABLE seed.finance_staff AS
SELECT array_agg(e.id) AS a FROM hr.employees e JOIN hr.departments d ON d.id = e.department_id
WHERE d.code IN ('FIN-ACC', 'FIN-AR', 'FIN-FPA') ;

-- Every journal entry balances: its debits equal its credits.
CREATE TABLE seed.je (entry_date date, description text, source text, lines jsonb);

-- Daily sales per region, in USD at that day's rate.
INSERT INTO seed.je
SELECT d.day, 'Daily sales summary - ' || d.region, 'sales',
       jsonb_build_array(
           jsonb_build_object('account', '1200', 'debit', d.net + d.shipping + d.tax),
           jsonb_build_object('account', '4100', 'credit', d.net),
           jsonb_build_object('account', '4200', 'credit', d.shipping),
           jsonb_build_object('account', '2200', 'credit', d.tax))
FROM (
    SELECT o.ordered_at::date AS day, co.region::text AS region,
           round(sum(o.tax / r.usd_rate), 2) AS tax,
           round(sum((o.subtotal - o.discount) / r.usd_rate), 2) AS net,
           round(sum(o.shipping_fee / r.usd_rate), 2) AS shipping
    FROM sales.orders o
    JOIN sales.customers c ON c.id = o.customer_id
    JOIN core.countries co ON co.code = c.country_code
    JOIN core.exchange_rates r ON r.currency_code = o.currency_code AND r.rate_date = o.ordered_at::date
    WHERE o.status <> 'cancelled'
    GROUP BY 1, 2
) d;

-- Where the books started.
INSERT INTO seed.je VALUES (date '2019-01-01', 'Opening balances', 'adjustment', jsonb_build_array(
    jsonb_build_object('account', '1010', 'debit', 1450000000),
    jsonb_build_object('account', '1200', 'debit', 210000000),
    jsonb_build_object('account', '1300', 'debit', 690000000),
    jsonb_build_object('account', '1500', 'debit', 1880000000),
    jsonb_build_object('account', '2100', 'credit', 430000000),
    jsonb_build_object('account', '3100', 'credit', 3800000000)));

-- Store sales come from the point-of-sale system as daily totals per region (they aren't orders):
-- busier on weekends and in December, growing ~9% a year.
CREATE TABLE seed.pos AS
SELECT d::date AS day, r.region,
       round((11500000 * r.share * power(1.09, (d::date - date '2019-01-01') / 365.0)
              * CASE WHEN extract(isodow FROM d) >= 6 THEN 1.35 ELSE 1 END
              * CASE WHEN extract(month FROM d) = 12 THEN 1.5 WHEN extract(month FROM d) = 11 THEN 1.2 ELSE 1 END
              * (0.9 + random() * 0.2))::numeric, 2) AS net,
       r.tax_rate
FROM generate_series(date '2019-01-01', date '2026-10-13', interval '1 day') d
CROSS JOIN (VALUES ('North America', 0.45, 0.07), ('Europe', 0.25, 0.20), ('Asia Pacific', 0.18, 0.10),
                   ('Latin America', 0.08, 0.16), ('Middle East & Africa', 0.04, 0.10)) r(region, share, tax_rate);
INSERT INTO seed.je
SELECT day, 'Store sales (POS) - ' || region, 'sales',
       jsonb_build_array(jsonb_build_object('account', '1010', 'debit', net + round(net * tax_rate, 2)),
                         jsonb_build_object('account', '4100', 'credit', net),
                         jsonb_build_object('account', '2200', 'credit', round(net * tax_rate, 2)))
FROM seed.pos;

-- Cost of the goods sold (orders and stores), once a week.
CREATE TABLE seed.weekly_cogs AS
SELECT day, round(sum(cogs), 2) AS cogs FROM (
    SELECT (date_trunc('week', o.ordered_at) + interval '6 days')::date AS day, i.quantity * p.unit_cost AS cogs
    FROM sales.orders o JOIN sales.order_items i ON i.order_id = o.id JOIN catalog.products p ON p.id = i.product_id
    WHERE o.status NOT IN ('cancelled', 'pending')
    UNION ALL
    SELECT (date_trunc('week', day) + interval '6 days')::date, net * 0.58 FROM seed.pos
) c
WHERE day <= date '2026-10-14'
GROUP BY day;
INSERT INTO seed.je
SELECT day, 'Weekly COGS recognition', 'sales',
       jsonb_build_array(jsonb_build_object('account', '5100', 'debit', cogs),
                         jsonb_build_object('account', '1300', 'credit', cogs))
FROM seed.weekly_cogs;

-- Customers paying their orders, weekly.
INSERT INTO seed.je
SELECT day, 'Customer receipts', 'sales',
       jsonb_build_array(jsonb_build_object('account', '1010', 'debit', amount),
                         jsonb_build_object('account', '1200', 'credit', amount))
FROM (
    SELECT (date_trunc('week', p.paid_at) + interval '6 days')::date AS day, round(sum(p.amount / fx.usd_rate), 2) AS amount
    FROM sales.payments p JOIN sales.orders o ON o.id = p.order_id JOIN seed.fx fx ON fx.code = o.currency_code
    WHERE p.status = 'captured' AND p.method <> 'invoice'
    GROUP BY 1
    UNION ALL
    SELECT (date_trunc('week', i.paid_on) + interval '6 days')::date, round(sum(i.amount / fx.usd_rate), 2)
    FROM finance.invoices i JOIN seed.fx fx ON fx.code = i.currency_code
    WHERE i.paid_on IS NOT NULL
    GROUP BY 1
) r
WHERE day <= date '2026-10-14';

-- Monthly payroll per department, from the salaries in force.
INSERT INTO seed.je
SELECT (m + interval '1 month - 1 day')::date, 'Payroll ' || to_char(m, 'Mon YYYY') || ' - ' || d.name, 'payroll',
       jsonb_build_array(jsonb_build_object('account', '6100', 'debit', p.wages, 'cost_center', d.cost_center),
                         jsonb_build_object('account', '6150', 'debit', round(p.wages * 0.22, 2), 'cost_center', d.cost_center),
                         jsonb_build_object('account', '1010', 'credit', p.wages + round(p.wages * 0.22, 2)))
FROM generate_series(timestamp '2019-01-01', timestamp '2026-09-01', interval '1 month') m
CROSS JOIN hr.departments d
CROSS JOIN LATERAL (
    SELECT round(sum(s.amount / fx.usd_rate) / 12, 2) AS wages
    FROM hr.employees e
    JOIN hr.salaries s ON s.employee_id = e.id AND s.effective_from <= m AND (s.effective_to IS NULL OR s.effective_to >= m)
    JOIN seed.fx fx ON fx.code = s.currency_code
    WHERE e.department_id = d.id AND (e.termination_date IS NULL OR e.termination_date >= m)
) p
WHERE p.wages > 0;

-- Restocking: about 150 supplier bills a week, adding up to a little more than what was sold.
CREATE TABLE seed.bills AS
SELECT w.day - seed.rint(0, 6) AS day, 1 + (w.day - date '2019-01-01' + n * 7919) % 900 AS supplier_id,
       round(w.cogs * 1.03 / 150 * (0.2 + 1.6 * random())::numeric, 2) AS amount
FROM seed.weekly_cogs w, generate_series(1, 150) n;
INSERT INTO seed.je
SELECT b.day, 'Supplier invoice SI-' || seed.digits(7) || ' - ' || s.name, 'purchasing',
       jsonb_build_array(jsonb_build_object('account', '1300', 'debit', b.amount),
                         jsonb_build_object('account', '2100', 'credit', b.amount))
FROM seed.bills b JOIN catalog.suppliers s ON s.id = b.supplier_id;
-- Paid in a monthly run, net 30.
INSERT INTO seed.je
SELECT (date_trunc('month', day) + interval '1 month 14 days')::date, 'Supplier payment run', 'purchasing',
       jsonb_build_array(jsonb_build_object('account', '2100', 'debit', sum(amount)),
                         jsonb_build_object('account', '1010', 'credit', sum(amount)))
FROM seed.bills
GROUP BY date_trunc('month', day)
HAVING date_trunc('month', day) + interval '1 month 14 days' <= date '2026-10-14';

-- Day-to-day expenses.

INSERT INTO seed.je
SELECT x.day, e.description, 'manual',
       jsonb_build_array(jsonb_build_object('account', e.account, 'debit', x.amount, 'cost_center', cc.code),
                         jsonb_build_object('account', '1010', 'credit', x.amount))
FROM generate_series(1, 60000) g
CROSS JOIN LATERAL (SELECT (seed.between('2019-01-01', '2026-10-14', 1.3) + g * interval '0 s')::date AS day,
                           round((200 + power(random(), 4) * 200000)::numeric, 2) AS amount) x
CROSS JOIN LATERAL (SELECT * FROM (VALUES
        ('6200', 'Office rent'), ('6200', 'Facilities maintenance'), ('6300', 'Electricity and water'),
        ('6300', 'Internet and phone'), ('6400', 'Business travel'), ('6400', 'Client dinner'),
        ('6500', 'SaaS subscriptions'), ('6500', 'Cloud hosting'), ('6600', 'Online advertising'),
        ('6600', 'Trade show booth'), ('6700', 'External audit fees'), ('6700', 'Legal counsel'),
        ('6700', 'Consulting services')) v(account, description)
    ORDER BY random() + g * 0 LIMIT 1) e
CROSS JOIN LATERAL (SELECT code FROM finance.cost_centers ORDER BY random() + g * 0 LIMIT 1) cc;

-- The payables from before 2019 are settled in the first payment run.
INSERT INTO seed.je VALUES (date '2019-01-15', 'Supplier payment run', 'purchasing', jsonb_build_array(
    jsonb_build_object('account', '2100', 'debit', 430000000),
    jsonb_build_object('account', '1010', 'credit', 430000000)));

-- Sales tax collected in a quarter is remitted a month after it ends.
INSERT INTO seed.je
SELECT (q + interval '4 months')::date, 'Sales tax remittance ' || to_char(q, 'YYYY') || ' Q' || to_char(q, 'Q'), 'adjustment',
       jsonb_build_array(jsonb_build_object('account', '2200', 'debit', amount),
                         jsonb_build_object('account', '1010', 'credit', amount))
FROM (SELECT date_trunc('quarter', j.entry_date) AS q, sum((l ->> 'credit')::numeric) AS amount
      FROM seed.je j, jsonb_array_elements(j.lines) l
      WHERE l ->> 'account' = '2200'
      GROUP BY 1) t
WHERE q + interval '4 months' <= date '2026-10-14';

-- Yearly dividend to shareholders.
INSERT INTO seed.je
SELECT make_date(y, 3, 15), 'Dividend paid FY' || (y - 1), 'adjustment',
       jsonb_build_array(jsonb_build_object('account', '3100', 'debit', 650000000 * power(1.1, y - 2019)),
                         jsonb_build_object('account', '1010', 'credit', 650000000 * power(1.1, y - 2019)))
FROM generate_series(2019, 2026) y;

-- Month-end depreciation.
INSERT INTO seed.je
SELECT (m + interval '1 month - 1 day')::date, 'Monthly depreciation', 'adjustment',
       jsonb_build_array(jsonb_build_object('account', '6800', 'debit', 12500000 + extract(year FROM m)::int * 0),
                         jsonb_build_object('account', '1500', 'credit', 12500000))
FROM generate_series(timestamp '2019-01-01', timestamp '2026-09-01', interval '1 month') m;

WITH numbered AS (
    SELECT row_number() OVER (ORDER BY entry_date, source, description) AS id, entry_date, description, source, lines
    FROM seed.je
), entries AS (
    INSERT INTO finance.journal_entries (id, entry_date, description, source, posted_by, posted_at)
    OVERRIDING SYSTEM VALUE
    SELECT n.id, n.entry_date, n.description, n.source,
           CASE WHEN n.source IN ('manual', 'adjustment', 'purchasing') THEN seed.pick(f.a) END,
           n.entry_date + interval '1 day' + seed.rint(0, 600) * interval '1 minute'
    FROM numbered n CROSS JOIN seed.finance_staff f
    RETURNING id
)
INSERT INTO finance.journal_lines (entry_id, line_number, account_code, cost_center, debit, credit)
SELECT n.id, l.n, l.line ->> 'account', l.line ->> 'cost_center',
       coalesce((l.line ->> 'debit')::numeric, 0), coalesce((l.line ->> 'credit')::numeric, 0)
FROM numbered n
CROSS JOIN LATERAL jsonb_array_elements(n.lines) WITH ORDINALITY l(line, n)
WHERE coalesce((l.line ->> 'debit')::numeric, 0) + coalesce((l.line ->> 'credit')::numeric, 0) > 0;
SELECT setval(pg_get_serial_sequence('finance.journal_entries', 'id'), (SELECT max(id) FROM finance.journal_entries));

-- Quarterly budgets per department and expense account, growing ~8% a year.
INSERT INTO finance.budgets (fiscal_year, quarter, cost_center, account_code, amount_usd)
SELECT y, q, cc.code, a.code,
       round((CASE a.code WHEN '6100' THEN 900000 WHEN '6200' THEN 120000 WHEN '6400' THEN 40000
                          WHEN '6500' THEN 60000 WHEN '6600' THEN 75000 ELSE 30000 END
              * power(1.08, y - 2019) * (0.6 + random() * 0.8))::numeric, -3)
FROM generate_series(2019, 2026) y, generate_series(1, 4) q, finance.cost_centers cc,
     finance.accounts a
WHERE a.code IN ('6100', '6200', '6400', '6500', '6600', '6700');

-- ---------------------------------------------------------------- support

INSERT INTO support.sla_policies (priority, first_response_hours, resolution_hours) VALUES
    ('low', 48, 240), ('normal', 24, 120), ('high', 8, 48), ('urgent', 1, 12);

CREATE TABLE seed.agents AS
SELECT array_agg(e.id) AS a FROM hr.employees e JOIN hr.departments d ON d.id = e.department_id
WHERE d.code IN ('SUP-T1', 'SUP-T2', 'SUP-CS');

CREATE TABLE seed.subjects (topic text, subject text);
INSERT INTO seed.subjects VALUES
    ('order', 'Where is my order?'), ('order', 'Package marked delivered but not received'),
    ('order', 'Order arrived damaged'), ('order', 'Wrong item in my package'), ('order', 'Missing item from order'),
    ('order', 'Can I change my delivery address?'), ('order', 'Request to cancel order'),
    ('order', 'Refund not received yet'), ('order', 'Return label not working'), ('order', 'Invoice copy needed'),
    ('order', 'Charged twice for the same order'), ('order', 'Promo code did not apply'),
    ('account', 'Cannot log in to my account'), ('account', 'Update billing information'),
    ('account', 'Loyalty points missing'), ('account', 'Unsubscribe from marketing e-mails'),
    ('account', 'Delete my account and data'), ('product', 'Question about product warranty'),
    ('product', 'Product stopped working'), ('product', 'Is this item compatible with my device?'),
    ('product', 'When will this be back in stock?'), ('b2b', 'Request for volume pricing'),
    ('b2b', 'Need to raise our credit limit'), ('b2b', 'EDI order rejected'), ('b2b', 'Tax exemption certificate');

-- 220,000 tickets; most are about an order.
INSERT INTO support.tickets (customer_id, order_id, assignee_id, subject, priority, status, channel, satisfaction,
                             opened_at, first_response_at, resolved_at)
SELECT x.customer_id, x.order_id, CASE WHEN st.status <> 'open' OR random() < 0.5 THEN seed.pick(ag.a) END,
       x.subject, pr.priority, st.status,
       seed.pick(array['email','email','chat','chat','chat','phone','web_form','social'])::support.contact_channel,
       CASE WHEN st.status IN ('resolved', 'closed') AND random() < 0.55
            THEN (array[1, 2, 3, 4, 4, 5, 5, 5, 5])[seed.rint(1, 9)] END,
       x.opened_at,
       CASE WHEN st.status <> 'open' THEN x.opened_at + sla.first_response_hours * random() * 1.1 * interval '1 hour' END,
       CASE WHEN st.status IN ('resolved', 'closed')
            THEN x.opened_at + sla.resolution_hours * power(random(), 1.5) * 1.15 * interval '1 hour' END
FROM generate_series(1, 220000) g
CROSS JOIN seed.agents ag
CROSS JOIN LATERAL (
    SELECT o.customer_id, CASE WHEN g % 10 < 7 THEN o.id END AS order_id,
           least(o.ordered_at + seed.rint(1, 30 * 24) * interval '1 hour', timestamptz '2026-10-14 17:00') AS opened_at,
           (SELECT s.subject FROM seed.subjects s
            WHERE s.topic = CASE WHEN g % 10 < 7 THEN 'order' WHEN g % 10 < 8 THEN 'account' WHEN g % 10 < 9 THEN 'product'
                                 WHEN c.segment <> 'consumer' THEN 'b2b' ELSE 'product' END
            ORDER BY random() + g * 0 LIMIT 1) AS subject
    FROM (SELECT 1 + floor(power(random(), 0.6) * (SELECT max(id) FROM sales.orders))::bigint + g * 0 AS id) r
    JOIN sales.orders o ON o.id = r.id
    JOIN sales.customers c ON c.id = o.customer_id
) x
CROSS JOIN LATERAL (SELECT (array['low','normal','normal','normal','normal','high','high','urgent'])[seed.rint(1, 8) + g * 0]::support.priority AS priority) pr
JOIN support.sla_policies sla ON sla.priority = pr.priority
CROSS JOIN LATERAL (
    SELECT CASE WHEN x.opened_at > timestamptz '2026-10-14' - interval '2 days'
                THEN (array['open','open','in_progress'])[seed.rint(1, 3)]
                WHEN x.opened_at > timestamptz '2026-10-14' - interval '10 days'
                THEN (array['open','in_progress','waiting_on_customer','resolved','resolved'])[seed.rint(1, 5)]
                ELSE (array['closed','closed','closed','closed','resolved'])[seed.rint(1, 5)] END::support.ticket_status AS status
    WHERE g > 0
) st;

INSERT INTO support.ticket_comments (ticket_id, author_employee_id, is_internal, body, created_at)
SELECT t.id, a.employee, a.internal,
       CASE WHEN a.employee IS NULL
            THEN seed.pick(array['Hi, any update on this?','I have attached a photo of the damage.',
                                 'Thanks, that solved it!','This is the second time this has happened.',
                                 'Can you call me instead?','The tracking page has not changed in 5 days.',
                                 'Please cancel it, I no longer need it.','I would like a replacement, not a refund.'])
            WHEN a.internal
            THEN seed.pick(array['Escalating to Tier 2.','Checked with the warehouse: package left on time.',
                                 'Carrier claim opened.','Customer is a VIP account, prioritize.',
                                 'Duplicate of an older ticket.','Refund approved by team lead.'])
            ELSE seed.pick(array['Thanks for reaching out! I''m looking into this now.',
                                 'I''ve issued a full refund; it should appear in 3-5 business days.',
                                 'A replacement is on its way, here is the new tracking number.',
                                 'Could you share your order number and a photo of the item?',
                                 'I''m sorry for the inconvenience. I''ve added a voucher to your account.',
                                 'Your return label has been e-mailed to you.',
                                 'We''re closing this ticket; reply anytime if you need anything else.'])
       END,
       t.opened_at + (coalesce(t.resolved_at, timestamptz '2026-10-14 17:00') - t.opened_at) * (c.n::float8 / (c.total + 1))
FROM support.tickets t
CROSS JOIN LATERAL (SELECT n, count(*) OVER () AS total FROM generate_series(1, seed.rint(1, 6) + (t.id * 0)::int) n) c
CROSS JOIN LATERAL (
    SELECT CASE WHEN c.n % 2 = 1 AND c.n > 1 THEN NULL ELSE t.assignee_id END AS employee,
           c.n % 2 = 0 AND random() < 0.3 AS internal
    WHERE c.n > 0
) a
WHERE t.status <> 'open' OR c.n = 1;

-- ---------------------------------------------------------------- marketing

CREATE TABLE seed.marketers AS
SELECT array_agg(e.id) AS a FROM hr.employees e JOIN hr.departments d ON d.id = e.department_id
WHERE d.code LIKE 'MKT-%';

INSERT INTO marketing.campaigns (name, channel, region, budget_usd, starts_on, ends_on, owner_id)
SELECT extract(year FROM s.starts_on) || ' Q' || extract(quarter FROM s.starts_on) || ' '
           || seed.pick(array['Spring Refresh','Back to School','Holiday Gift Guide','Black Friday','Summer Sale',
                              'New Arrivals','Home Office Upgrade','Fitness Kickoff','Loyalty Rewards','Brand Awareness',
                              'B2B Procurement Week','Clearance Event','App Download Push','Northstar Basics Launch'])
           || ' - ' || CASE ch.channel WHEN 'tv' THEN 'TV' ELSE initcap(ch.channel) END || coalesce(' - ' || r.region::text, ''),
       ch.channel::marketing.campaign_channel, r.region,
       round((5000 + power(random(), 2) * 1500000)::numeric, -2),
       s.starts_on, s.starts_on + seed.rint(7, 90), seed.pick(m.a)
FROM generate_series(1, 800) g
CROSS JOIN seed.marketers m
CROSS JOIN LATERAL (SELECT seed.pick(array['email','search','search','social','social','display','tv','events','affiliate'])
                           || CASE WHEN g > 0 THEN '' END AS channel) ch
CROSS JOIN LATERAL (SELECT CASE WHEN random() < 0.25 THEN NULL
                                ELSE (enum_range(NULL::core.region))[seed.rint(1, 5) + g * 0] END AS region) r
CROSS JOIN LATERAL (SELECT (seed.between('2019-01-01', '2026-10-01', 1.3) + g * interval '0 s')::date AS starts_on) s;

INSERT INTO marketing.campaign_daily_stats (campaign_id, day, impressions, clicks, conversions, spend_usd)
SELECT c.id, d::date, s.impressions, s.clicks, floor(s.clicks * (0.01 + random() * 0.05))::int, s.spend
FROM marketing.campaigns c
CROSS JOIN LATERAL generate_series(c.starts_on, least(c.ends_on, date '2026-10-14'), interval '1 day') d
CROSS JOIN LATERAL (SELECT round(c.budget_usd / (c.ends_on - c.starts_on + 1) * (0.7 + random()::numeric * 0.6), 2) AS spend
                    WHERE d IS NOT NULL) sp
-- Roughly an $8-25 CPM, a 0.3-2.5% click-through rate and 1-6% of clicks converting.
CROSS JOIN LATERAL (SELECT sp.spend, imp AS impressions, floor(imp * (0.003 + random() * 0.022))::int AS clicks
                    FROM (SELECT floor(sp.spend::float8 * (40 + random() * 85))::int AS imp) x) s;

INSERT INTO marketing.leads (campaign_id, email, company, job_title, country_code, score, status, source, converted_customer_id, created_at)
SELECT c.id,
       lower(regexp_replace(seed.pick((SELECT a FROM seed.first_names)), '[^A-Za-z]', '', 'g')) || '.'
           || lower(regexp_replace(seed.pick((SELECT a FROM seed.last_names)), '[^A-Za-z]', '', 'g'))
           || '@' || seed.pick(array['acme','globex','initech','umbrella','hooli','vandelay','wonka','stark','wayne','tyrell'])
           || seed.rint(1, 500) || '.example',
       CASE WHEN random() < 0.7 THEN seed.pick(array['Acme','Globex','Initech','Umbrella','Hooli','Vandelay','Wonka',
                                                     'Stark','Wayne','Tyrell']) || ' ' || seed.pick(array['Corp','Industries','Group','Holdings']) END,
       seed.pick(array['Procurement Manager','Office Manager','IT Director','Operations Lead','Purchasing Agent',
                       'CFO','Facilities Coordinator','Student','Small Business Owner', NULL]),
       seed.pick((SELECT a FROM seed.country_pool)),
       sc.score, st.status::marketing.lead_status,
       -- Unknown for about a fifth of the leads.
       CASE WHEN random() < 0.8 THEN (enum_range(NULL::marketing.lead_source))[seed.rint(1, 5)] END,
       CASE WHEN st.status = 'converted' THEN seed.rint(1, 300000) END,
       c.starts_on + random() * (least(c.ends_on, date '2026-10-14') - c.starts_on) * interval '1 day'
FROM generate_series(1, 350000) g
CROSS JOIN LATERAL (SELECT * FROM marketing.campaigns WHERE id = 1 + (g * 7) % 800) c
CROSS JOIN LATERAL (SELECT floor(power(random(), 1.3) * 100)::int + g * 0 AS score) sc
CROSS JOIN LATERAL (SELECT CASE WHEN sc.score > 80 AND random() < 0.5 THEN 'converted'
                                WHEN sc.score > 60 THEN (array['qualified','contacted','lost'])[seed.rint(1, 3)]
                                ELSE (array['new','new','contacted','lost'])[seed.rint(1, 4)] END AS status
                    WHERE g > 0) st;

-- ---------------------------------------------------------------- audit trail

INSERT INTO audit.change_log (table_name, row_id, action, changed_by, old_values, new_values, changed_at)
SELECT ch.table_name, ch.row_id, ch.action, ch.who, ch.old_values, ch.new_values,
       seed.between('2023-01-01', '2026-10-14 17:00', 1.4)
FROM generate_series(1, 300000) g
CROSS JOIN LATERAL (
    SELECT * FROM (VALUES
        ('sales.orders', seed.rint(1, 1300000)::text, 'UPDATE', 'svc_fulfillment',
         '{"status": "confirmed"}'::jsonb, '{"status": "shipped"}'::jsonb),
        ('sales.orders', seed.rint(1, 1300000)::text, 'UPDATE', 'svc_fulfillment',
         '{"status": "shipped"}'::jsonb, '{"status": "delivered"}'::jsonb),
        ('sales.orders', seed.rint(1, 1300000)::text, 'UPDATE', 'svc_checkout',
         '{"status": "pending"}'::jsonb, '{"status": "confirmed"}'::jsonb),
        ('sales.customers', seed.rint(1, 300000)::text, 'UPDATE', 'svc_account',
         jsonb_build_object('marketing_opt_in', true), jsonb_build_object('marketing_opt_in', false)),
        ('catalog.products', seed.rint(1, 30000)::text, 'UPDATE', 'svc_pricing',
         jsonb_build_object('list_price', seed.rint(10, 900) + 0.99), jsonb_build_object('list_price', seed.rint(10, 900) + 0.99)),
        ('inventory.stock_levels', seed.rint(1, 21) || ',' || seed.rint(1, 30000), 'UPDATE', 'etl_nightly',
         jsonb_build_object('on_hand', seed.rint(0, 900)), jsonb_build_object('on_hand', seed.rint(0, 900))),
        ('hr.employees', seed.rint(1, 31000)::text, 'UPDATE', 'hr_admin',
         jsonb_build_object('job_title_id', seed.rint(1, 150)), jsonb_build_object('job_title_id', seed.rint(1, 150))),
        ('sales.customers', seed.rint(1, 300000)::text, 'INSERT', 'svc_account', NULL, '{"segment": "consumer"}'::jsonb),
        ('sales.customer_addresses', seed.rint(1, 500000)::text, 'DELETE', 'svc_account',
         '{"kind": "shipping"}'::jsonb, NULL)
    ) v(table_name, row_id, action, who, old_values, new_values)
    ORDER BY random() + g * 0 LIMIT 1
) ch;

DROP SCHEMA seed CASCADE;

-- Foreign keys were not enforced while seeding: check every one of them now.
SET session_replication_role = origin;
DO $$
DECLARE
    fk record;
    bad bigint;
BEGIN
    FOR fk IN
        SELECT c.conname, c.conrelid::regclass AS child, c.confrelid::regclass AS parent,
               (SELECT string_agg(format('ch.%I', a.attname), ', ' ORDER BY k.n)
                FROM unnest(c.conkey) WITH ORDINALITY k(attnum, n)
                JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum) AS child_cols,
               (SELECT string_agg(format('p.%I', a.attname), ', ' ORDER BY k.n)
                FROM unnest(c.confkey) WITH ORDINALITY k(attnum, n)
                JOIN pg_attribute a ON a.attrelid = c.confrelid AND a.attnum = k.attnum) AS parent_cols,
               (SELECT string_agg(format('ch.%I IS NOT NULL', a.attname), ' AND ')
                FROM unnest(c.conkey) k(attnum)
                JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum) AS not_null
        FROM pg_constraint c
        WHERE c.contype = 'f' AND c.connamespace::regnamespace::text NOT IN ('pg_catalog', 'information_schema')
    LOOP
        EXECUTE format('SELECT count(*) FROM %s ch WHERE %s AND NOT EXISTS (SELECT FROM %s p WHERE (%s) = (%s))',
                       fk.child, fk.not_null, fk.parent, fk.parent_cols, fk.child_cols)
        INTO bad;
        IF bad > 0 THEN
            RAISE EXCEPTION 'seed broke foreign key % on %: % rows', fk.conname, fk.child, bad;
        END IF;
    END LOOP;
END
$$;
