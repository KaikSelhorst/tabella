-- Northstar Global: views, functions, procedures and triggers, created after the data is loaded
-- (so the triggers don't fire for the seed).

\set ON_ERROR_STOP on
SET client_min_messages = warning;
\o /dev/null

-- ---------------------------------------------------------------- functions

CREATE FUNCTION core.to_usd(amount numeric, currency char(3), on_date date DEFAULT current_date)
RETURNS numeric LANGUAGE sql STABLE AS $$
    SELECT round(amount / r.usd_rate, 2)
    FROM core.exchange_rates r
    WHERE r.currency_code = currency AND r.rate_date <= on_date
    ORDER BY r.rate_date DESC
    LIMIT 1
$$;
COMMENT ON FUNCTION core.to_usd IS 'Converts an amount to USD at the latest rate on or before the given day';

CREATE FUNCTION sales.customer_tier(lifetime_usd numeric) RETURNS text LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE WHEN lifetime_usd >= 100000 THEN 'platinum'
                WHEN lifetime_usd >= 10000 THEN 'gold'
                WHEN lifetime_usd >= 1000 THEN 'silver'
                ELSE 'bronze' END
$$;

CREATE FUNCTION hr.reporting_chain(employee integer)
RETURNS TABLE (depth integer, employee_id integer, full_name text, title text)
LANGUAGE sql STABLE AS $$
    WITH RECURSIVE chain AS (
        SELECT 0 AS depth, e.id, e.manager_id FROM hr.employees e WHERE e.id = employee
        UNION ALL
        SELECT c.depth + 1, m.id, m.manager_id FROM chain c JOIN hr.employees m ON m.id = c.manager_id
    )
    SELECT c.depth, e.id, e.first_name || ' ' || e.last_name, t.title
    FROM chain c JOIN hr.employees e ON e.id = c.id JOIN hr.job_titles t ON t.id = e.job_title_id
    ORDER BY c.depth
$$;
COMMENT ON FUNCTION hr.reporting_chain IS 'An employee and every manager above them, up to the CEO';

-- Cancels an order that hasn't shipped: refunds its payments and releases its stock reservation.
CREATE PROCEDURE sales.cancel_order(order_id bigint, reason text DEFAULT 'Cancelled by customer')
LANGUAGE plpgsql AS $$
DECLARE
    current_status sales.order_status;
BEGIN
    SELECT status INTO current_status FROM sales.orders WHERE id = order_id FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'order % does not exist', order_id;
    END IF;
    IF current_status NOT IN ('pending', 'confirmed') THEN
        RAISE EXCEPTION 'order % is already %, it can no longer be cancelled', order_id, current_status;
    END IF;
    UPDATE sales.orders SET status = 'cancelled', notes = concat_ws(' | ', notes, reason) WHERE id = order_id;
    UPDATE sales.payments SET status = 'refunded' WHERE payments.order_id = cancel_order.order_id AND status IN ('authorized', 'captured');
END
$$;

-- ---------------------------------------------------------------- triggers

CREATE FUNCTION core.touch_updated_at() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.updated_at := now();
    RETURN NEW;
END
$$;

CREATE TRIGGER employees_touch BEFORE UPDATE ON hr.employees FOR EACH ROW EXECUTE FUNCTION core.touch_updated_at();
CREATE TRIGGER products_touch BEFORE UPDATE ON catalog.products FOR EACH ROW EXECUTE FUNCTION core.touch_updated_at();
CREATE TRIGGER customers_touch BEFORE UPDATE ON sales.customers FOR EACH ROW EXECUTE FUNCTION core.touch_updated_at();
CREATE TRIGGER stock_levels_touch BEFORE UPDATE ON inventory.stock_levels FOR EACH ROW EXECUTE FUNCTION core.touch_updated_at();

-- Records what changed, field by field.
CREATE FUNCTION audit.record_change() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    old_row jsonb := CASE WHEN TG_OP <> 'INSERT' THEN to_jsonb(OLD) END;
    new_row jsonb := CASE WHEN TG_OP <> 'DELETE' THEN to_jsonb(NEW) END;
    changed_old jsonb;
    changed_new jsonb;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        SELECT jsonb_object_agg(key, old_row -> key), jsonb_object_agg(key, value)
        INTO changed_old, changed_new
        FROM jsonb_each(new_row)
        WHERE old_row -> key IS DISTINCT FROM value AND key <> 'updated_at';
        IF changed_new IS NULL THEN
            RETURN NEW;
        END IF;
    ELSE
        changed_old := old_row;
        changed_new := new_row;
    END IF;
    INSERT INTO audit.change_log (table_name, row_id, action, old_values, new_values)
    VALUES (TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME, coalesce(new_row, old_row) ->> 'id', TG_OP, changed_old, changed_new);
    RETURN coalesce(NEW, OLD);
END
$$;

CREATE TRIGGER orders_audit AFTER INSERT OR UPDATE OR DELETE ON sales.orders
    FOR EACH ROW EXECUTE FUNCTION audit.record_change();
CREATE TRIGGER customers_audit AFTER UPDATE OR DELETE ON sales.customers
    FOR EACH ROW EXECUTE FUNCTION audit.record_change();
CREATE TRIGGER products_audit AFTER UPDATE OF list_price, is_active ON catalog.products
    FOR EACH ROW EXECUTE FUNCTION audit.record_change();

-- A journal entry must balance once the transaction ends (lines are added one by one).
CREATE FUNCTION finance.check_entry_balanced() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    imbalance numeric;
BEGIN
    SELECT sum(debit) - sum(credit) INTO imbalance FROM finance.journal_lines WHERE entry_id = NEW.entry_id;
    IF imbalance <> 0 THEN
        RAISE EXCEPTION 'journal entry % is out of balance by %', NEW.entry_id, imbalance;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER journal_lines_balanced AFTER INSERT OR UPDATE ON finance.journal_lines
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION finance.check_entry_balanced();

-- ---------------------------------------------------------------- views

CREATE VIEW sales.order_summary AS
SELECT o.id, o.order_number, o.ordered_at, o.status, ch.name AS channel,
       c.customer_number, coalesce(c.company_name, c.first_name || ' ' || c.last_name) AS customer,
       c.segment, co.name AS country, co.region,
       (SELECT count(*) FROM sales.order_items i WHERE i.order_id = o.id) AS lines,
       o.total, o.currency_code
FROM sales.orders o
JOIN sales.customers c ON c.id = o.customer_id
JOIN core.countries co ON co.code = c.country_code
JOIN sales.channels ch ON ch.id = o.channel_id;
COMMENT ON VIEW sales.order_summary IS 'One row per order with its customer, country and channel';

CREATE VIEW sales.monthly_revenue AS
SELECT date_trunc('month', o.ordered_at)::date AS month, co.region,
       count(*) AS orders,
       round(sum(o.total / r.usd_rate), 2) AS revenue_usd,
       round(avg(o.total / r.usd_rate), 2) AS avg_order_usd
FROM sales.orders o
JOIN sales.customers c ON c.id = o.customer_id
JOIN core.countries co ON co.code = c.country_code
JOIN core.exchange_rates r ON r.currency_code = o.currency_code AND r.rate_date = o.ordered_at::date
WHERE o.status NOT IN ('cancelled', 'returned')
GROUP BY 1, 2;

CREATE VIEW sales.top_products_last_90_days AS
SELECT p.id, p.sku, p.name, cat.name AS category, b.name AS brand,
       sum(i.quantity) AS units, count(DISTINCT i.order_id) AS orders
FROM sales.order_items i
JOIN sales.orders o ON o.id = i.order_id
JOIN catalog.products p ON p.id = i.product_id
JOIN catalog.categories cat ON cat.id = p.category_id
JOIN catalog.brands b ON b.id = p.brand_id
WHERE o.ordered_at >= now() - interval '90 days' AND o.status <> 'cancelled'
GROUP BY p.id, cat.name, b.name
ORDER BY units DESC
LIMIT 100;

CREATE VIEW hr.headcount AS
SELECT coalesce(parent.name, d.name) AS division, d.name AS department,
       count(*) FILTER (WHERE e.termination_date IS NULL) AS active,
       count(*) FILTER (WHERE e.termination_date IS NOT NULL) AS left_company,
       round(avg((coalesce(e.termination_date, current_date) - e.hire_date) / 365.25)::numeric, 1) AS avg_tenure_years
FROM hr.departments d
LEFT JOIN hr.departments parent ON parent.id = d.parent_id
LEFT JOIN hr.employees e ON e.department_id = d.id
GROUP BY 1, 2;

CREATE VIEW hr.current_salaries AS
SELECT e.id AS employee_id, e.first_name || ' ' || e.last_name AS employee, t.title, t.level, d.name AS department,
       s.amount, s.currency_code, core.to_usd(s.amount, s.currency_code) AS amount_usd, s.bonus_target_pct
FROM hr.employees e
JOIN hr.salaries s ON s.employee_id = e.id AND s.effective_to IS NULL
JOIN hr.job_titles t ON t.id = e.job_title_id
JOIN hr.departments d ON d.id = e.department_id
WHERE e.termination_date IS NULL;

CREATE VIEW inventory.low_stock AS
SELECT w.code AS warehouse, p.sku, p.name, s.on_hand, s.reserved, s.on_hand - s.reserved AS available, s.reorder_point
FROM inventory.stock_levels s
JOIN inventory.warehouses w ON w.id = s.warehouse_id
JOIN catalog.products p ON p.id = s.product_id
WHERE p.is_active AND s.on_hand - s.reserved < s.reorder_point;

CREATE VIEW finance.trial_balance AS
SELECT a.code, a.name, a.type, sum(l.debit) AS debit, sum(l.credit) AS credit, sum(l.debit) - sum(l.credit) AS balance
FROM finance.accounts a
LEFT JOIN finance.journal_lines l ON l.account_code = a.code
GROUP BY a.code
ORDER BY a.code;

CREATE VIEW finance.overdue_invoices AS
SELECT i.invoice_number, c.company_name, c.customer_number, i.issued_on, i.due_on,
       current_date - i.due_on AS days_overdue, i.amount, i.currency_code
FROM finance.invoices i
JOIN sales.customers c ON c.id = i.customer_id
WHERE i.paid_on IS NULL AND i.due_on < current_date;

CREATE VIEW support.sla_breaches AS
SELECT t.id, t.subject, t.priority, t.status, t.opened_at, t.first_response_at,
       t.first_response_at - t.opened_at > make_interval(hours => p.first_response_hours) AS late_first_response,
       coalesce(t.resolved_at, now()) - t.opened_at > make_interval(hours => p.resolution_hours) AS late_resolution
FROM support.tickets t
JOIN support.sla_policies p ON p.priority = t.priority
WHERE t.first_response_at - t.opened_at > make_interval(hours => p.first_response_hours)
   OR coalesce(t.resolved_at, now()) - t.opened_at > make_interval(hours => p.resolution_hours);

CREATE VIEW marketing.campaign_performance AS
SELECT c.id, c.name, c.channel, c.region, c.budget_usd,
       sum(s.spend_usd) AS spend_usd, sum(s.impressions) AS impressions, sum(s.clicks) AS clicks,
       sum(s.conversions) AS conversions,
       round(sum(s.clicks)::numeric / nullif(sum(s.impressions), 0) * 100, 2) AS ctr_pct,
       round(sum(s.spend_usd) / nullif(sum(s.conversions), 0), 2) AS cost_per_conversion
FROM marketing.campaigns c
LEFT JOIN marketing.campaign_daily_stats s ON s.campaign_id = c.id
GROUP BY c.id;

CREATE MATERIALIZED VIEW sales.customer_lifetime_value AS
SELECT c.id AS customer_id, c.customer_number, c.segment, c.country_code,
       count(o.id) AS orders,
       coalesce(round(sum(o.total / fx.usd_rate), 2), 0) AS lifetime_usd,
       sales.customer_tier(coalesce(sum(o.total / fx.usd_rate), 0)) AS tier,
       min(o.ordered_at) AS first_order_at, max(o.ordered_at) AS last_order_at
FROM sales.customers c
LEFT JOIN sales.orders o ON o.customer_id = c.id AND o.status NOT IN ('cancelled', 'returned')
LEFT JOIN (SELECT DISTINCT ON (currency_code) currency_code, usd_rate
           FROM core.exchange_rates ORDER BY currency_code, rate_date DESC) fx ON fx.currency_code = o.currency_code
GROUP BY c.id;
CREATE UNIQUE INDEX customer_lifetime_value_pk ON sales.customer_lifetime_value (customer_id);
CREATE INDEX customer_lifetime_value_tier ON sales.customer_lifetime_value (tier, lifetime_usd DESC);
COMMENT ON MATERIALIZED VIEW sales.customer_lifetime_value IS 'Refresh with REFRESH MATERIALIZED VIEW CONCURRENTLY';

-- ---------------------------------------------------------------- roles

DO $$ BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'analyst') THEN CREATE ROLE analyst NOLOGIN; END IF;
END $$;
GRANT USAGE ON SCHEMA core, hr, catalog, inventory, sales, finance, support, marketing TO analyst;
GRANT SELECT ON ALL TABLES IN SCHEMA core, catalog, inventory, sales, support, marketing TO analyst;
GRANT SELECT ON hr.headcount, finance.trial_balance TO analyst;

ANALYZE;
