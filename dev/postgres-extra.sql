-- A second schema and a second database for trying "switch database/schema" (on top of
-- dev/postgres.sql). Names repeat on purpose: `customers` exists in public and in finance
-- with different rows, so an unqualified SELECT shows which one the editor is pointing at.
-- Rerunnable.
--
--   docker exec -i dblite-dev-pg-1 psql -U postgres < dev/postgres-extra.sql

SET client_min_messages = warning;

-- Schema finance (database postgres) -------------------------------------------------------

CREATE SCHEMA IF NOT EXISTS finance;
DROP TABLE IF EXISTS finance.entries, finance.accounts, finance.customers;

-- Same name as public.customers, other columns and only 5 rows.
CREATE TABLE finance.customers (
  id int PRIMARY KEY,
  legal_name text NOT NULL,
  tax_id text NOT NULL,
  credit_limit numeric(12, 2) NOT NULL
);
INSERT INTO finance.customers VALUES
  (1, 'Hot Bread Bakery LLC', '12-3456789', 15000),
  (2, 'Good Price Market Inc', '23-4567890', 80000),
  (3, 'Joe''s Garage', '34-5678901', 5000),
  (4, 'Bright Smile Dental LLC', '45-6789012', 30000),
  (5, 'Fast Freight Co', '56-7890123', 120000);

CREATE TABLE finance.accounts (
  id serial PRIMARY KEY,
  name text NOT NULL,
  bank text NOT NULL,
  balance numeric(12, 2) NOT NULL
);
INSERT INTO finance.accounts (name, bank, balance) VALUES
  ('Checking', 'First National Bank', 152340.55),
  ('Certificate of deposit', 'Union Trust', 500000.00),
  ('Petty cash', 'Internal', 1200.00);

CREATE TABLE finance.entries (
  id serial PRIMARY KEY,
  account_id int NOT NULL REFERENCES finance.accounts (id),
  customer_id int REFERENCES finance.customers (id),
  type text NOT NULL CHECK (type IN ('income', 'expense')),
  description text NOT NULL,
  amount numeric(12, 2) NOT NULL,
  due_date date NOT NULL,
  paid boolean NOT NULL DEFAULT false
);
INSERT INTO finance.entries (account_id, customer_id, type, description, amount, due_date, paid)
SELECT 1 + g % 3,
       CASE WHEN g % 2 = 0 THEN 1 + g % 5 END,
       CASE WHEN g % 2 = 0 THEN 'income' ELSE 'expense' END,
       CASE WHEN g % 2 = 0 THEN 'Invoice ' || g ELSE (ARRAY['Rent', 'Power', 'Payroll', 'Taxes'])[1 + g % 4] END,
       round((100 + g * 37.5 % 9000)::numeric, 2),
       date '2026-01-01' + g * 3,
       g < 70
FROM generate_series(1, 100) g;

-- Database hr (another database on the same server) ---------------------------------------

SELECT 'CREATE DATABASE hr' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'hr')\gexec
\connect hr
SET client_min_messages = warning;

DROP TABLE IF EXISTS vacations, employees, departments, customers;

CREATE TABLE departments (
  id int PRIMARY KEY,
  name text NOT NULL,
  cost_center text NOT NULL
);
INSERT INTO departments VALUES
  (1, 'Technology', 'CC-100'),
  (2, 'Sales', 'CC-200'),
  (3, 'Finance', 'CC-300'),
  (4, 'Logistics', 'CC-400');

CREATE TABLE employees (
  id serial PRIMARY KEY,
  name text NOT NULL,
  role text NOT NULL,
  department_id int NOT NULL REFERENCES departments (id),
  salary numeric(10, 2) NOT NULL,
  hired_at date NOT NULL
);
INSERT INTO employees (name, role, department_id, salary, hired_at)
SELECT (ARRAY['Anna', 'Brian', 'Carol', 'David', 'Emily', 'Frank', 'Grace', 'Henry'])[1 + g % 8] || ' ' ||
       (ARRAY['Smith', 'Johnson', 'Brown', 'Miller', 'Davis'])[1 + g % 5],
       (ARRAY['Analyst', 'Developer', 'Salesperson', 'Coordinator', 'Assistant'])[1 + g % 5],
       1 + g % 4,
       3000 + (g * 731) % 12000,
       date '2018-01-01' + g * 23
FROM generate_series(1, 40) g;

CREATE TABLE vacations (
  id serial PRIMARY KEY,
  employee_id int NOT NULL REFERENCES employees (id),
  starts_on date NOT NULL,
  days int NOT NULL CHECK (days BETWEEN 5 AND 30)
);
INSERT INTO vacations (employee_id, starts_on, days)
SELECT 1 + g % 40, date '2026-01-05' + g * 9, (ARRAY[10, 15, 20, 30])[1 + g % 4]
FROM generate_series(1, 25) g;

-- Also a `customers` here: the third different one (only 2 rows).
CREATE TABLE customers (id int PRIMARY KEY, name text NOT NULL, note text);
INSERT INTO customers VALUES
  (1, 'Customer of the hr database', 'this table lives in the hr database'),
  (2, 'Another one from hr', 'if you see this, the tab is on the hr database');
