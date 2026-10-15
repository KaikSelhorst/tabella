-- Functions, procedures, triggers, views and sequences for the "dev" connection, on top of
-- dev/postgres.sql. Rerunnable.
--
--   docker exec -i dblite-dev-pg-1 psql -U postgres < dev/postgres-objects.sql

SET client_min_messages = warning;

-- Functions --------------------------------------------------------------------------------

-- Sum of a customer's orders.
CREATE OR REPLACE FUNCTION customer_order_total(cid integer) RETURNS numeric
LANGUAGE sql STABLE AS $$
  SELECT coalesce(sum(total), 0) FROM orders WHERE customer_id = cid;
$$;

-- A label from a balance.
CREATE OR REPLACE FUNCTION customer_tier(balance numeric) RETURNS text
LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN
  IF balance IS NULL THEN
    RETURN 'no balance';
  ELSIF balance >= 40000 THEN
    RETURN 'gold';
  ELSIF balance >= 15000 THEN
    RETURN 'silver';
  END IF;
  RETURN 'bronze';
END;
$$;

-- Two versions with the same name (overloads): only digits, or formatted.
CREATE OR REPLACE FUNCTION format_document(doc text) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
  SELECT regexp_replace(doc, '\D', '', 'g');
$$;

CREATE OR REPLACE FUNCTION format_document(doc text, with_dashes boolean) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE
    WHEN NOT with_dashes THEN regexp_replace(doc, '\D', '', 'g')
    ELSE regexp_replace(regexp_replace(doc, '\D', '', 'g'), '(\d{3})(\d{2})(\d{4})', '\1-\2-\3')
  END;
$$;

-- Procedures -------------------------------------------------------------------------------

-- Marks old orders as archived (defined only: running it touches millions of rows).
CREATE OR REPLACE PROCEDURE archive_old_orders(days integer)
LANGUAGE plpgsql AS $$
BEGIN
  UPDATE orders SET status = 'archived'
  WHERE created_at < now() - make_interval(days => days) AND status IN ('delivered', 'cancelled');
  RAISE NOTICE 'orders archived';
END;
$$;

CREATE OR REPLACE PROCEDURE deactivate_customer(cid integer)
LANGUAGE sql AS $$
  UPDATE customers SET active = false WHERE id = cid;
$$;

-- Triggers ---------------------------------------------------------------------------------

-- events.updated_at follows every update.
CREATE OR REPLACE FUNCTION touch_updated_at() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  NEW.updated_at := now();
  RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS events_updated_at ON events;
CREATE TRIGGER events_updated_at
  BEFORE UPDATE ON events
  FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

-- Status changes of an order are logged.
CREATE TABLE IF NOT EXISTS orders_audit (
  id bigserial PRIMARY KEY,
  order_id bigint NOT NULL,
  old_status text,
  new_status text,
  changed_at timestamptz NOT NULL DEFAULT now()
);

CREATE OR REPLACE FUNCTION audit_order_status() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO orders_audit (order_id, old_status, new_status)
  VALUES (NEW.id, OLD.status, NEW.status);
  RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS orders_status_audit ON orders;
CREATE TRIGGER orders_status_audit
  AFTER UPDATE OF status ON orders
  FOR EACH ROW WHEN (OLD.status IS DISTINCT FROM NEW.status)
  EXECUTE FUNCTION audit_order_status();

-- Views ------------------------------------------------------------------------------------

CREATE OR REPLACE VIEW active_customers AS
  SELECT id, name, email, city, state, balance
  FROM customers
  WHERE active;

CREATE OR REPLACE VIEW sales_by_state AS
  SELECT c.state, count(*) AS orders, round(sum(o.total), 2) AS total
  FROM orders o JOIN customers c ON c.id = o.customer_id
  GROUP BY c.state;

CREATE OR REPLACE VIEW orders_this_month AS
  SELECT id, customer_id, status, total, created_at
  FROM orders
  WHERE created_at >= date_trunc('month', now());

-- Sequences --------------------------------------------------------------------------------

CREATE SEQUENCE IF NOT EXISTS invoice_number_seq START 1000;
CREATE SEQUENCE IF NOT EXISTS support_ticket_seq START 500000 INCREMENT 10;
-- One already in use, so "current value" shows something.
SELECT nextval('invoice_number_seq') FROM generate_series(1, 42);
