-- Sample data for the "dev" connection (Postgres on localhost:5432). Heavy on purpose: big
-- tables for paging, counts and exports, a wide table with many types, and lots of small
-- tables for the sidebar. Rerunnable: it drops and recreates everything it owns.
--
--   docker exec -i dblite-pg psql -U postgres < dev/postgres.sql      (~1-2 min)

\timing on
SET client_min_messages = warning;

-- For the "slow queries" view (the server loads it via shared_preload_libraries).
CREATE EXTENSION IF NOT EXISTS pg_stat_statements;

DROP TABLE IF EXISTS orders, events, customers CASCADE;
DROP SCHEMA IF EXISTS archive CASCADE;

-- 1 000 000 customers.
CREATE TABLE customers (
  id serial PRIMARY KEY,
  name text NOT NULL,
  email text,
  city text,
  state char(2),
  active boolean DEFAULT true,
  balance numeric(12, 2),
  created_at timestamp DEFAULT now()
);
INSERT INTO customers (name, email, city, state, active, balance, created_at)
SELECT 'Customer ' || g,
       'customer' || g || '@example.com',
       (ARRAY['New York', 'Los Angeles', 'Chicago', 'Houston', 'Phoenix', 'Seattle', 'Miami', 'Denver', 'Boston', 'Atlanta'])[1 + g % 10],
       (ARRAY['NY', 'CA', 'IL', 'TX', 'AZ', 'WA', 'FL', 'CO', 'MA', 'GA'])[1 + g % 10],
       CASE WHEN g % 7 = 0 THEN NULL ELSE g % 3 <> 0 END,
       CASE WHEN g % 5 = 0 THEN NULL ELSE round((random() * 50000)::numeric, 2) END,
       now() - (random() * interval '5 years')
FROM generate_series(1, 1000000) g;

-- 5 000 000 orders (more than the app's exact-count limit: the pager shows an estimate).
CREATE TABLE orders (
  id bigserial PRIMARY KEY,
  customer_id int NOT NULL REFERENCES customers (id),
  status text NOT NULL,
  total numeric(12, 2) NOT NULL,
  paid boolean NOT NULL DEFAULT false,
  shipped boolean,
  created_at timestamptz DEFAULT now()
);
INSERT INTO orders (customer_id, status, total, paid, shipped, created_at)
SELECT 1 + ((g::bigint * 7919) % 1000000)::int,
       (ARRAY['new', 'paid', 'picking', 'shipped', 'delivered', 'cancelled'])[1 + g % 6],
       round((random() * 2000 + 5)::numeric, 2),
       g % 4 <> 0,
       CASE WHEN g % 4 = 0 THEN NULL ELSE g % 2 = 0 END,
       now() - (random() * interval '3 years')
FROM generate_series(1, 5000000) g;
CREATE INDEX orders_customer_idx ON orders (customer_id);
CREATE INDEX orders_created_idx ON orders (created_at);

-- 300 000 wide rows with many types and lots of NULLs.
CREATE TABLE events (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  type text NOT NULL,
  customer_id int REFERENCES customers (id),
  payload jsonb,
  tags text[],
  ip inet,
  duration_ms integer,
  value double precision,
  description text,
  processed boolean,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz
);
INSERT INTO events (type, customer_id, payload, tags, ip, duration_ms, value, description, processed, created_at, updated_at)
SELECT (ARRAY['login', 'logout', 'purchase', 'error', 'search', 'checkout'])[1 + g % 6],
       CASE WHEN g % 9 = 0 THEN NULL ELSE 1 + (g * 31) % 1000000 END,
       jsonb_build_object('page', '/product/' || g % 5000, 'browser', (ARRAY['firefox', 'chrome', 'safari'])[1 + g % 3],
                          'items', g % 12, 'coupon', CASE WHEN g % 10 = 0 THEN 'PROMO10' END),
       ARRAY['web', (ARRAY['mobile', 'desktop', 'tablet'])[1 + g % 3]],
       ('10.' || g % 256 || '.' || (g / 256) % 256 || '.' || g % 199)::inet,
       CASE WHEN g % 11 = 0 THEN NULL ELSE (random() * 5000)::int END,
       CASE WHEN g % 13 = 0 THEN NULL ELSE random() * 1000 END,
       CASE WHEN g % 4 = 0 THEN repeat('Long description of event ' || g || '. ', 1 + g % 8) END,
       CASE WHEN g % 6 = 0 THEN NULL ELSE g % 2 = 0 END,
       now() - (random() * interval '1 year'),
       CASE WHEN g % 3 = 0 THEN now() - (random() * interval '30 days') END
FROM generate_series(1, 300000) g;
CREATE INDEX events_type_idx ON events (type);

-- 200 small tables in their own schema, for the sidebar and Ctrl+P.
CREATE SCHEMA archive;
DO $$
BEGIN
  FOR i IN 1..200 LOOP
    EXECUTE format('CREATE TABLE archive.record_%s (id serial PRIMARY KEY, ref text, amount numeric(10, 2), created_at date)', lpad(i::text, 3, '0'));
    EXECUTE format('INSERT INTO archive.record_%s (ref, amount, created_at) SELECT ''R'' || g, g * 1.5, current_date - g FROM generate_series(1, 100) g', lpad(i::text, 3, '0'));
  END LOOP;
END $$;

ANALYZE;
SELECT relname AS table_name, n_live_tup AS row_count FROM pg_stat_user_tables WHERE schemaname = 'public' ORDER BY 1;
SELECT pg_size_pretty(pg_database_size(current_database())) AS size;
