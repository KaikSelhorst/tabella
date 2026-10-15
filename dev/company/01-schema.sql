-- Northstar Global: a fictional multinational (retail + B2B distribution) with the systems a
-- large company runs: HR, catalog, inventory, sales, finance, support and marketing.
-- Seeded by 02-seed.sql; views, functions and triggers in 03-objects.sql.

CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
CREATE EXTENSION IF NOT EXISTS citext;

CREATE SCHEMA core;
CREATE SCHEMA hr;
CREATE SCHEMA catalog;
CREATE SCHEMA inventory;
CREATE SCHEMA sales;
CREATE SCHEMA finance;
CREATE SCHEMA support;
CREATE SCHEMA marketing;
CREATE SCHEMA audit;

COMMENT ON SCHEMA core IS 'Reference data shared by every business unit';
COMMENT ON SCHEMA hr IS 'People, org structure and compensation';
COMMENT ON SCHEMA catalog IS 'Products, brands and suppliers';
COMMENT ON SCHEMA inventory IS 'Warehouses and stock';
COMMENT ON SCHEMA sales IS 'Customers, orders, payments and fulfillment';
COMMENT ON SCHEMA finance IS 'General ledger, invoicing and budgets';
COMMENT ON SCHEMA support IS 'Customer support tickets';
COMMENT ON SCHEMA marketing IS 'Campaigns and leads';
COMMENT ON SCHEMA audit IS 'Change history';

-- ---------------------------------------------------------------- core

CREATE TYPE core.region AS ENUM ('North America', 'Latin America', 'Europe', 'Asia Pacific', 'Middle East & Africa');

CREATE TABLE core.currencies (
    code        char(3) PRIMARY KEY,
    name        text NOT NULL,
    symbol      text NOT NULL,
    decimals    smallint NOT NULL DEFAULT 2 CHECK (decimals BETWEEN 0 AND 4)
);

CREATE TABLE core.countries (
    code          char(2) PRIMARY KEY,
    name          text NOT NULL UNIQUE,
    region        core.region NOT NULL,
    currency_code char(3) NOT NULL REFERENCES core.currencies,
    vat_rate      numeric(5, 2) NOT NULL DEFAULT 0 CHECK (vat_rate >= 0),
    phone_prefix  text NOT NULL
);

CREATE TABLE core.cities (
    id           integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    country_code char(2) NOT NULL REFERENCES core.countries,
    name         text NOT NULL,
    state        text,
    timezone     text NOT NULL,
    latitude     numeric(8, 5),
    longitude    numeric(8, 5),
    UNIQUE (country_code, name)
);

CREATE TABLE core.offices (
    id          integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    code        text NOT NULL UNIQUE,
    name        text NOT NULL,
    city_id     integer NOT NULL REFERENCES core.cities,
    address     text NOT NULL,
    is_hq       boolean NOT NULL DEFAULT false,
    opened_on   date NOT NULL
);
CREATE UNIQUE INDEX offices_one_hq ON core.offices (is_hq) WHERE is_hq;

CREATE TABLE core.exchange_rates (
    currency_code char(3) NOT NULL REFERENCES core.currencies,
    rate_date     date NOT NULL,
    usd_rate      numeric(14, 6) NOT NULL CHECK (usd_rate > 0),
    PRIMARY KEY (currency_code, rate_date)
);
COMMENT ON COLUMN core.exchange_rates.usd_rate IS 'Units of this currency per 1 USD';

-- ---------------------------------------------------------------- hr

CREATE TYPE hr.employment_type AS ENUM ('full_time', 'part_time', 'contractor', 'intern');
CREATE TYPE hr.time_off_status AS ENUM ('pending', 'approved', 'rejected', 'cancelled');

CREATE TABLE hr.departments (
    id          integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name        text NOT NULL,
    code        text NOT NULL UNIQUE,
    parent_id   integer REFERENCES hr.departments,
    cost_center text NOT NULL,
    head_id     integer -- FK added after hr.employees exists
);

CREATE TABLE hr.job_titles (
    id          integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    title       text NOT NULL UNIQUE,
    family      text NOT NULL,
    level       smallint NOT NULL CHECK (level BETWEEN 1 AND 10),
    min_salary  numeric(12, 2) NOT NULL,
    max_salary  numeric(12, 2) NOT NULL,
    CHECK (max_salary >= min_salary)
);
COMMENT ON COLUMN hr.job_titles.level IS '1 = entry level ... 10 = C-suite';

CREATE TABLE hr.employees (
    id              integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    employee_number text NOT NULL UNIQUE,
    first_name      text NOT NULL,
    last_name       text NOT NULL,
    email           citext NOT NULL UNIQUE,
    phone           text,
    birth_date      date NOT NULL,
    hire_date       date NOT NULL,
    termination_date date,
    employment_type hr.employment_type NOT NULL DEFAULT 'full_time',
    department_id   integer NOT NULL REFERENCES hr.departments,
    job_title_id    integer NOT NULL REFERENCES hr.job_titles,
    manager_id      integer REFERENCES hr.employees,
    office_id       integer NOT NULL REFERENCES core.offices,
    is_remote       boolean NOT NULL DEFAULT false,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (termination_date IS NULL OR termination_date >= hire_date)
);
CREATE INDEX employees_department ON hr.employees (department_id);
CREATE INDEX employees_manager ON hr.employees (manager_id);
CREATE INDEX employees_name ON hr.employees (last_name, first_name);
CREATE INDEX employees_active ON hr.employees (department_id) WHERE termination_date IS NULL;

ALTER TABLE hr.departments ADD CONSTRAINT departments_head_fk FOREIGN KEY (head_id) REFERENCES hr.employees;

CREATE TABLE hr.salaries (
    employee_id   integer NOT NULL REFERENCES hr.employees ON DELETE CASCADE,
    effective_from date NOT NULL,
    effective_to   date,
    amount        numeric(12, 2) NOT NULL CHECK (amount > 0),
    currency_code char(3) NOT NULL REFERENCES core.currencies,
    bonus_target_pct numeric(5, 2) NOT NULL DEFAULT 0,
    PRIMARY KEY (employee_id, effective_from)
);

CREATE TABLE hr.time_off_requests (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    employee_id integer NOT NULL REFERENCES hr.employees ON DELETE CASCADE,
    kind        text NOT NULL CHECK (kind IN ('vacation', 'sick', 'parental', 'bereavement', 'unpaid')),
    starts_on   date NOT NULL,
    ends_on     date NOT NULL,
    status      hr.time_off_status NOT NULL DEFAULT 'pending',
    approver_id integer REFERENCES hr.employees,
    note        text,
    CHECK (ends_on >= starts_on)
);
CREATE INDEX time_off_employee ON hr.time_off_requests (employee_id);

CREATE TABLE hr.performance_reviews (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    employee_id  integer NOT NULL REFERENCES hr.employees ON DELETE CASCADE,
    reviewer_id  integer REFERENCES hr.employees,
    period       text NOT NULL,
    rating       smallint NOT NULL CHECK (rating BETWEEN 1 AND 5),
    summary      text,
    UNIQUE (employee_id, period)
);

-- ---------------------------------------------------------------- catalog

CREATE TABLE catalog.categories (
    id        integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name      text NOT NULL,
    slug      text NOT NULL UNIQUE,
    parent_id integer REFERENCES catalog.categories
);

CREATE TABLE catalog.brands (
    id      integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name    text NOT NULL UNIQUE,
    country_code char(2) REFERENCES core.countries,
    is_private_label boolean NOT NULL DEFAULT false
);

CREATE TABLE catalog.suppliers (
    id            integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name          text NOT NULL,
    tax_id        text NOT NULL UNIQUE,
    country_code  char(2) NOT NULL REFERENCES core.countries,
    contact_email citext,
    payment_terms_days smallint NOT NULL DEFAULT 30,
    rating        numeric(2, 1) CHECK (rating BETWEEN 0 AND 5),
    is_active     boolean NOT NULL DEFAULT true
);

CREATE TABLE catalog.products (
    id          integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    sku         text NOT NULL UNIQUE,
    name        text NOT NULL,
    description text,
    category_id integer NOT NULL REFERENCES catalog.categories,
    brand_id    integer NOT NULL REFERENCES catalog.brands,
    unit_cost   numeric(12, 2) NOT NULL CHECK (unit_cost >= 0),
    list_price  numeric(12, 2) NOT NULL CHECK (list_price >= 0),
    weight_kg   numeric(8, 3),
    attributes  jsonb NOT NULL DEFAULT '{}',
    is_active   boolean NOT NULL DEFAULT true,
    launched_on date NOT NULL,
    discontinued_on date,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX products_category ON catalog.products (category_id);
CREATE INDEX products_brand ON catalog.products (brand_id);
CREATE INDEX products_attributes ON catalog.products USING gin (attributes);
CREATE INDEX products_name_lower ON catalog.products (lower(name));

CREATE TABLE catalog.product_suppliers (
    product_id   integer NOT NULL REFERENCES catalog.products ON DELETE CASCADE,
    supplier_id  integer NOT NULL REFERENCES catalog.suppliers,
    supplier_sku text NOT NULL,
    lead_time_days smallint NOT NULL,
    cost         numeric(12, 2) NOT NULL,
    is_preferred boolean NOT NULL DEFAULT false,
    PRIMARY KEY (product_id, supplier_id)
);

CREATE TABLE catalog.price_history (
    product_id   integer NOT NULL REFERENCES catalog.products ON DELETE CASCADE,
    valid_from   date NOT NULL,
    list_price   numeric(12, 2) NOT NULL,
    PRIMARY KEY (product_id, valid_from)
);

-- ---------------------------------------------------------------- inventory

CREATE TABLE inventory.warehouses (
    id        integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    code      text NOT NULL UNIQUE,
    name      text NOT NULL,
    city_id   integer NOT NULL REFERENCES core.cities,
    capacity_m3 integer NOT NULL,
    manager_id integer REFERENCES hr.employees,
    is_active boolean NOT NULL DEFAULT true
);

CREATE TABLE inventory.stock_levels (
    warehouse_id integer NOT NULL REFERENCES inventory.warehouses,
    product_id   integer NOT NULL REFERENCES catalog.products,
    on_hand      integer NOT NULL DEFAULT 0 CHECK (on_hand >= 0),
    reserved     integer NOT NULL DEFAULT 0 CHECK (reserved >= 0),
    reorder_point integer NOT NULL DEFAULT 0,
    bin_location text,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (warehouse_id, product_id)
);
CREATE INDEX stock_levels_product ON inventory.stock_levels (product_id);

CREATE TYPE inventory.movement_type AS ENUM ('receipt', 'shipment', 'transfer_in', 'transfer_out', 'adjustment', 'return');

CREATE TABLE inventory.stock_movements (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    warehouse_id integer NOT NULL REFERENCES inventory.warehouses,
    product_id   integer NOT NULL REFERENCES catalog.products,
    movement     inventory.movement_type NOT NULL,
    quantity     integer NOT NULL CHECK (quantity <> 0),
    reference    text,
    moved_at     timestamptz NOT NULL
);
CREATE INDEX stock_movements_product ON inventory.stock_movements (product_id, moved_at);
CREATE INDEX stock_movements_moved_at ON inventory.stock_movements USING brin (moved_at);

-- ---------------------------------------------------------------- sales

CREATE TYPE sales.customer_segment AS ENUM ('consumer', 'small_business', 'mid_market', 'enterprise', 'government');
CREATE TYPE sales.order_status AS ENUM ('pending', 'confirmed', 'shipped', 'delivered', 'cancelled', 'returned');
CREATE TYPE sales.payment_method AS ENUM ('credit_card', 'debit_card', 'paypal', 'bank_transfer', 'invoice', 'gift_card');

CREATE TABLE sales.channels (
    id   smallint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL UNIQUE,
    is_online boolean NOT NULL
);

CREATE TABLE sales.customers (
    id            integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    customer_number text NOT NULL UNIQUE,
    segment       sales.customer_segment NOT NULL,
    company_name  text,
    first_name    text NOT NULL,
    last_name     text NOT NULL,
    email         citext NOT NULL,
    phone         text,
    country_code  char(2) NOT NULL REFERENCES core.countries,
    city_id       integer REFERENCES core.cities,
    account_manager_id integer REFERENCES hr.employees,
    credit_limit  numeric(14, 2),
    loyalty_points integer NOT NULL DEFAULT 0,
    marketing_opt_in boolean NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CHECK (segment = 'consumer' OR company_name IS NOT NULL)
);
CREATE INDEX customers_email ON sales.customers (email);
CREATE INDEX customers_country ON sales.customers (country_code);
CREATE INDEX customers_manager ON sales.customers (account_manager_id);
COMMENT ON TABLE sales.customers IS 'Consumers and business accounts; business ones have a company and usually an account manager';

CREATE TABLE sales.customer_addresses (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES sales.customers ON DELETE CASCADE,
    kind        text NOT NULL CHECK (kind IN ('billing', 'shipping')),
    line1       text NOT NULL,
    line2       text,
    city_id     integer NOT NULL REFERENCES core.cities,
    postal_code text NOT NULL,
    is_default  boolean NOT NULL DEFAULT false
);
CREATE INDEX customer_addresses_customer ON sales.customer_addresses (customer_id);

CREATE TABLE sales.promotions (
    id          integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    code        text NOT NULL UNIQUE,
    description text NOT NULL,
    discount_pct numeric(5, 2) NOT NULL CHECK (discount_pct > 0 AND discount_pct <= 80),
    starts_on   date NOT NULL,
    ends_on     date NOT NULL
);

CREATE TABLE sales.orders (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    order_number  text NOT NULL UNIQUE,
    customer_id   integer NOT NULL REFERENCES sales.customers,
    channel_id    smallint NOT NULL REFERENCES sales.channels,
    sales_rep_id  integer REFERENCES hr.employees,
    promotion_id  integer REFERENCES sales.promotions,
    status        sales.order_status NOT NULL,
    currency_code char(3) NOT NULL REFERENCES core.currencies,
    subtotal      numeric(14, 2) NOT NULL DEFAULT 0,
    discount      numeric(14, 2) NOT NULL DEFAULT 0,
    tax           numeric(14, 2) NOT NULL DEFAULT 0,
    shipping_fee  numeric(14, 2) NOT NULL DEFAULT 0,
    total         numeric(14, 2) GENERATED ALWAYS AS (subtotal - discount + tax + shipping_fee) STORED,
    ordered_at    timestamptz NOT NULL,
    notes         text
);
CREATE INDEX orders_customer ON sales.orders (customer_id, ordered_at DESC);
CREATE INDEX orders_ordered_at ON sales.orders (ordered_at);
CREATE INDEX orders_status ON sales.orders (status) WHERE status IN ('pending', 'confirmed');

CREATE TABLE sales.order_items (
    order_id    bigint NOT NULL REFERENCES sales.orders ON DELETE CASCADE,
    line_number smallint NOT NULL,
    product_id  integer NOT NULL REFERENCES catalog.products,
    quantity    integer NOT NULL CHECK (quantity > 0),
    unit_price  numeric(12, 2) NOT NULL,
    discount_pct numeric(5, 2) NOT NULL DEFAULT 0,
    PRIMARY KEY (order_id, line_number)
);
CREATE INDEX order_items_product ON sales.order_items (product_id);

CREATE TABLE sales.payments (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    order_id    bigint NOT NULL REFERENCES sales.orders ON DELETE CASCADE,
    method      sales.payment_method NOT NULL,
    amount      numeric(14, 2) NOT NULL,
    status      text NOT NULL CHECK (status IN ('authorized', 'captured', 'refunded', 'failed')),
    provider_ref text,
    paid_at     timestamptz NOT NULL
);
CREATE INDEX payments_order ON sales.payments (order_id);

CREATE TABLE sales.shipments (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    order_id     bigint NOT NULL REFERENCES sales.orders ON DELETE CASCADE,
    warehouse_id integer NOT NULL REFERENCES inventory.warehouses,
    carrier      text NOT NULL,
    tracking_number text NOT NULL,
    shipped_at   timestamptz NOT NULL,
    delivered_at timestamptz,
    CHECK (delivered_at IS NULL OR delivered_at >= shipped_at)
);
CREATE INDEX shipments_order ON sales.shipments (order_id);

CREATE TABLE sales.returns (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    order_id    bigint NOT NULL REFERENCES sales.orders ON DELETE CASCADE,
    reason      text NOT NULL,
    refund_amount numeric(14, 2) NOT NULL,
    requested_at timestamptz NOT NULL,
    approved    boolean
);

-- ---------------------------------------------------------------- finance

CREATE TYPE finance.account_type AS ENUM ('asset', 'liability', 'equity', 'revenue', 'expense');

CREATE TABLE finance.accounts (
    code       text PRIMARY KEY,
    name       text NOT NULL,
    type       finance.account_type NOT NULL,
    parent_code text REFERENCES finance.accounts
);

CREATE TABLE finance.cost_centers (
    code          text PRIMARY KEY,
    name          text NOT NULL,
    department_id integer REFERENCES hr.departments
);

CREATE TABLE finance.journal_entries (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    entry_date  date NOT NULL,
    description text NOT NULL,
    source      text NOT NULL CHECK (source IN ('sales', 'payroll', 'purchasing', 'manual', 'adjustment')),
    posted_by   integer REFERENCES hr.employees,
    posted_at   timestamptz NOT NULL
);
CREATE INDEX journal_entries_date ON finance.journal_entries (entry_date);

CREATE TABLE finance.journal_lines (
    entry_id     bigint NOT NULL REFERENCES finance.journal_entries ON DELETE CASCADE,
    line_number  smallint NOT NULL,
    account_code text NOT NULL REFERENCES finance.accounts,
    cost_center  text REFERENCES finance.cost_centers,
    debit        numeric(16, 2) NOT NULL DEFAULT 0 CHECK (debit >= 0),
    credit       numeric(16, 2) NOT NULL DEFAULT 0 CHECK (credit >= 0),
    PRIMARY KEY (entry_id, line_number),
    CHECK ((debit = 0) <> (credit = 0))
);
CREATE INDEX journal_lines_account ON finance.journal_lines (account_code);

CREATE TABLE finance.invoices (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    invoice_number text NOT NULL UNIQUE,
    order_id     bigint REFERENCES sales.orders,
    customer_id  integer NOT NULL REFERENCES sales.customers,
    issued_on    date NOT NULL,
    due_on       date NOT NULL,
    amount       numeric(14, 2) NOT NULL,
    currency_code char(3) NOT NULL REFERENCES core.currencies,
    paid_on      date,
    CHECK (due_on >= issued_on)
);
CREATE INDEX invoices_customer ON finance.invoices (customer_id);
CREATE INDEX invoices_unpaid ON finance.invoices (due_on) WHERE paid_on IS NULL;

CREATE TABLE finance.budgets (
    fiscal_year  smallint NOT NULL,
    quarter      smallint NOT NULL CHECK (quarter BETWEEN 1 AND 4),
    cost_center  text NOT NULL REFERENCES finance.cost_centers,
    account_code text NOT NULL REFERENCES finance.accounts,
    amount_usd   numeric(16, 2) NOT NULL,
    PRIMARY KEY (fiscal_year, quarter, cost_center, account_code)
);

-- ---------------------------------------------------------------- support

CREATE TYPE support.priority AS ENUM ('low', 'normal', 'high', 'urgent');
CREATE TYPE support.ticket_status AS ENUM ('open', 'in_progress', 'waiting_on_customer', 'resolved', 'closed');

CREATE TABLE support.sla_policies (
    priority          support.priority PRIMARY KEY,
    first_response_hours smallint NOT NULL,
    resolution_hours  smallint NOT NULL
);

CREATE TABLE support.tickets (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    customer_id  integer NOT NULL REFERENCES sales.customers,
    order_id     bigint REFERENCES sales.orders,
    assignee_id  integer REFERENCES hr.employees,
    subject      text NOT NULL,
    priority     support.priority NOT NULL DEFAULT 'normal',
    status       support.ticket_status NOT NULL DEFAULT 'open',
    channel      text NOT NULL CHECK (channel IN ('email', 'phone', 'chat', 'web_form', 'social')),
    satisfaction smallint CHECK (satisfaction BETWEEN 1 AND 5),
    opened_at    timestamptz NOT NULL,
    first_response_at timestamptz,
    resolved_at  timestamptz
);
CREATE INDEX tickets_customer ON support.tickets (customer_id);
CREATE INDEX tickets_assignee_open ON support.tickets (assignee_id) WHERE status NOT IN ('resolved', 'closed');

CREATE TABLE support.ticket_comments (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ticket_id  bigint NOT NULL REFERENCES support.tickets ON DELETE CASCADE,
    author_employee_id integer REFERENCES hr.employees,
    is_internal boolean NOT NULL DEFAULT false,
    body       text NOT NULL,
    created_at timestamptz NOT NULL
);
CREATE INDEX ticket_comments_ticket ON support.ticket_comments (ticket_id);

-- ---------------------------------------------------------------- marketing

CREATE TABLE marketing.campaigns (
    id          integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name        text NOT NULL,
    channel     text NOT NULL CHECK (channel IN ('email', 'search', 'social', 'display', 'tv', 'events', 'affiliate')),
    region      core.region,
    budget_usd  numeric(14, 2) NOT NULL,
    starts_on   date NOT NULL,
    ends_on     date NOT NULL,
    owner_id    integer REFERENCES hr.employees
);

CREATE TABLE marketing.campaign_daily_stats (
    campaign_id integer NOT NULL REFERENCES marketing.campaigns ON DELETE CASCADE,
    day         date NOT NULL,
    impressions integer NOT NULL,
    clicks      integer NOT NULL,
    conversions integer NOT NULL,
    spend_usd   numeric(12, 2) NOT NULL,
    PRIMARY KEY (campaign_id, day)
);

CREATE TABLE marketing.leads (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    campaign_id integer REFERENCES marketing.campaigns,
    email       citext NOT NULL,
    company     text,
    job_title   text,
    country_code char(2) REFERENCES core.countries,
    score       smallint NOT NULL CHECK (score BETWEEN 0 AND 100),
    status      text NOT NULL CHECK (status IN ('new', 'contacted', 'qualified', 'converted', 'lost')),
    converted_customer_id integer REFERENCES sales.customers,
    created_at  timestamptz NOT NULL
);

-- ---------------------------------------------------------------- audit

CREATE TABLE audit.change_log (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    table_name  text NOT NULL,
    row_id      text NOT NULL,
    action      text NOT NULL CHECK (action IN ('INSERT', 'UPDATE', 'DELETE')),
    changed_by  text NOT NULL DEFAULT current_user,
    old_values  jsonb,
    new_values  jsonb,
    changed_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX change_log_table_row ON audit.change_log (table_name, row_id);
