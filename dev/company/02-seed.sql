-- Northstar Global sample data: ~10 million rows from 2019 to mid-October 2026.
-- Deterministic (fixed random seed), so every fresh container gets the same data.

\set ON_ERROR_STOP on
SET client_min_messages = warning;
-- Foreign keys aren't checked row by row while seeding (it would take most of the time); the
-- data is consistent by construction and 03-seed-backoffice.sql verifies every key at the end.
SET session_replication_role = replica;
SET work_mem = '256MB';
-- Docker's small default /dev/shm can't hold big parallel hash joins.
SET max_parallel_workers_per_gather = 0;
\o /dev/null
SELECT setseed(0.4242);

CREATE SCHEMA seed;

CREATE FUNCTION seed.pick(anyarray) RETURNS anyelement
    LANGUAGE sql VOLATILE AS 'SELECT $1[1 + floor(random() * cardinality($1))::int]';
CREATE FUNCTION seed.rint(int, int) RETURNS int
    LANGUAGE sql VOLATILE AS 'SELECT $1 + floor(random() * ($2 - $1 + 1))::int';
-- A random moment between two timestamps; skew > 1 favours the later end (a growing business).
CREATE FUNCTION seed.between(timestamptz, timestamptz, float8 DEFAULT 1) RETURNS timestamptz
    LANGUAGE sql VOLATILE AS 'SELECT $1 + ($2 - $1) * power(random(), 1 / $3)';
CREATE FUNCTION seed.digits(int) RETURNS text
    LANGUAGE sql VOLATILE AS 'SELECT lpad(floor(random() * 10 ^ $1)::bigint::text, $1, ''0'')';

-- "Today" for the data set: recent orders are still pending, tickets still open, etc.
CREATE TABLE seed.clock AS SELECT timestamptz '2026-10-14 18:00:00+00' AS now, timestamptz '2019-01-01 00:00:00+00' AS start;

CREATE TABLE seed.first_names (a text[]);
INSERT INTO seed.first_names VALUES (ARRAY[
    'James','Mary','Robert','Patricia','John','Jennifer','Michael','Linda','David','Elizabeth',
    'William','Barbara','Richard','Susan','Joseph','Jessica','Thomas','Sarah','Charles','Karen',
    'Daniel','Emily','Matthew','Olivia','Anthony','Sophia','Mark','Emma','Steven','Ava',
    'Andrew','Isabella','Joshua','Mia','Kevin','Charlotte','Brian','Amelia','Ethan','Harper',
    'Lucas','Evelyn','Liam','Abigail','Noah','Chloe','Mateus','Ana','Gabriel','Juliana',
    'Rafael','Camila','Diego','Valentina','Carlos','Lucia','Pedro','Sofia','Luis','Elena',
    'Hans','Anna','Lukas','Lena','Pierre','Camille','Louis','Marco','Giulia','Pablo',
    'Carmen','Jan','Eva','Erik','Astrid','Piotr','Zofia','Hiroshi','Yuki','Kenji',
    'Sakura','Wei','Jun','Mei','Arjun','Priya','Rahul','Ananya','Vikram','Aisha',
    'Omar','Fatima','Ahmed','Layla','Min-jun','Seo-yeon','Thabo','Naledi','Siobhan','Oliver',
    'Grace','Jack','Chen','Nina','Ivan','Maya','Leo','Zara','Samuel','Hannah']);

CREATE TABLE seed.last_names (a text[]);
INSERT INTO seed.last_names VALUES (ARRAY[
    'Smith','Johnson','Williams','Brown','Jones','Garcia','Miller','Davis','Rodriguez','Martinez',
    'Hernandez','Lopez','Gonzalez','Wilson','Anderson','Thomas','Taylor','Moore','Jackson','Martin',
    'Lee','Perez','Thompson','White','Harris','Sanchez','Clark','Ramirez','Lewis','Robinson',
    'Walker','Young','Allen','King','Wright','Scott','Torres','Nguyen','Hill','Flores',
    'Green','Adams','Nelson','Baker','Hall','Rivera','Campbell','Mitchell','Carter','Roberts',
    'Silva','Santos','Oliveira','Souza','Costa','Pereira','Muller','Schmidt','Schneider','Fischer',
    'Weber','Dubois','Moreau','Laurent','Rossi','Russo','Bianchi','Fernandez','Ruiz','Jansen',
    'de Vries','Andersson','Johansson','Kowalski','Nowak','Tanaka','Suzuki','Sato','Watanabe','Wang',
    'Zhang','Liu','Chen','Patel','Sharma','Singh','Kumar','Gupta','Khan','Ali',
    'Hassan','Kim','Park','Choi','Nkosi','Dlamini','O''Brien','Murphy','Kelly','Novak']);

-- ---------------------------------------------------------------- core

INSERT INTO core.currencies (code, name, symbol, decimals) VALUES
    ('USD', 'US Dollar', '$', 2), ('EUR', 'Euro', '€', 2), ('GBP', 'Pound Sterling', '£', 2),
    ('CAD', 'Canadian Dollar', 'C$', 2), ('MXN', 'Mexican Peso', 'MX$', 2), ('BRL', 'Brazilian Real', 'R$', 2),
    ('JPY', 'Japanese Yen', '¥', 0), ('CNY', 'Chinese Yuan', 'CN¥', 2), ('INR', 'Indian Rupee', '₹', 2),
    ('AUD', 'Australian Dollar', 'A$', 2), ('SGD', 'Singapore Dollar', 'S$', 2), ('ZAR', 'South African Rand', 'R', 2),
    ('AED', 'UAE Dirham', 'AED', 2), ('CHF', 'Swiss Franc', 'CHF', 2), ('KRW', 'South Korean Won', '₩', 0),
    ('SEK', 'Swedish Krona', 'kr', 2), ('PLN', 'Polish Zloty', 'zł', 2);

-- Approximate units per USD, the centre of the generated daily rates.
CREATE TABLE seed.fx (code char(3) PRIMARY KEY, usd_rate numeric);
INSERT INTO seed.fx VALUES
    ('USD', 1), ('EUR', 0.92), ('GBP', 0.79), ('CAD', 1.36), ('MXN', 17.8), ('BRL', 5.2), ('JPY', 150),
    ('CNY', 7.2), ('INR', 83.5), ('AUD', 1.52), ('SGD', 1.35), ('ZAR', 18.6), ('AED', 3.6725),
    ('CHF', 0.88), ('KRW', 1340), ('SEK', 10.6), ('PLN', 4.0);

INSERT INTO core.countries (code, name, region, currency_code, vat_rate, phone_prefix) VALUES
    ('US', 'United States', 'North America', 'USD', 0, '+1'),
    ('CA', 'Canada', 'North America', 'CAD', 5, '+1'),
    ('MX', 'Mexico', 'Latin America', 'MXN', 16, '+52'),
    ('BR', 'Brazil', 'Latin America', 'BRL', 17, '+55'),
    ('GB', 'United Kingdom', 'Europe', 'GBP', 20, '+44'),
    ('DE', 'Germany', 'Europe', 'EUR', 19, '+49'),
    ('FR', 'France', 'Europe', 'EUR', 20, '+33'),
    ('ES', 'Spain', 'Europe', 'EUR', 21, '+34'),
    ('IT', 'Italy', 'Europe', 'EUR', 22, '+39'),
    ('NL', 'Netherlands', 'Europe', 'EUR', 21, '+31'),
    ('IE', 'Ireland', 'Europe', 'EUR', 23, '+353'),
    ('SE', 'Sweden', 'Europe', 'SEK', 25, '+46'),
    ('PL', 'Poland', 'Europe', 'PLN', 23, '+48'),
    ('CH', 'Switzerland', 'Europe', 'CHF', 8.1, '+41'),
    ('JP', 'Japan', 'Asia Pacific', 'JPY', 10, '+81'),
    ('CN', 'China', 'Asia Pacific', 'CNY', 13, '+86'),
    ('IN', 'India', 'Asia Pacific', 'INR', 18, '+91'),
    ('AU', 'Australia', 'Asia Pacific', 'AUD', 10, '+61'),
    ('SG', 'Singapore', 'Asia Pacific', 'SGD', 9, '+65'),
    ('KR', 'South Korea', 'Asia Pacific', 'KRW', 10, '+82'),
    ('ZA', 'South Africa', 'Middle East & Africa', 'ZAR', 15, '+27'),
    ('AE', 'United Arab Emirates', 'Middle East & Africa', 'AED', 5, '+971');

-- Share of customers per country, and pay level relative to the US.
CREATE TABLE seed.country_mix (code char(2) PRIMARY KEY, customers int, pay numeric);
INSERT INTO seed.country_mix VALUES
    ('US', 30, 1), ('CA', 5, 0.8), ('MX', 4, 0.3), ('BR', 6, 0.3), ('GB', 7, 0.8), ('DE', 7, 0.8),
    ('FR', 5, 0.75), ('ES', 3, 0.6), ('IT', 3, 0.6), ('NL', 2, 0.8), ('IE', 1, 0.8), ('SE', 1, 0.75),
    ('PL', 2, 0.4), ('CH', 1, 1.1), ('JP', 5, 0.65), ('CN', 4, 0.4), ('IN', 5, 0.22), ('AU', 3, 0.85),
    ('SG', 1, 0.8), ('KR', 2, 0.6), ('ZA', 1, 0.3), ('AE', 1, 0.8);

INSERT INTO core.cities (country_code, name, state, timezone, latitude, longitude) VALUES
    ('US', 'Seattle', 'WA', 'America/Los_Angeles', 47.60620, -122.33210),
    ('US', 'New York', 'NY', 'America/New_York', 40.71280, -74.00600),
    ('US', 'Los Angeles', 'CA', 'America/Los_Angeles', 34.05220, -118.24370),
    ('US', 'Chicago', 'IL', 'America/Chicago', 41.87810, -87.62980),
    ('US', 'Houston', 'TX', 'America/Chicago', 29.76040, -95.36980),
    ('US', 'Phoenix', 'AZ', 'America/Phoenix', 33.44840, -112.07400),
    ('US', 'Austin', 'TX', 'America/Chicago', 30.26720, -97.74310),
    ('US', 'Atlanta', 'GA', 'America/New_York', 33.74900, -84.38800),
    ('US', 'Denver', 'CO', 'America/Denver', 39.73920, -104.99030),
    ('US', 'Boston', 'MA', 'America/New_York', 42.36010, -71.05890),
    ('US', 'Miami', 'FL', 'America/New_York', 25.76170, -80.19180),
    ('US', 'San Francisco', 'CA', 'America/Los_Angeles', 37.77490, -122.41940),
    ('US', 'Dallas', 'TX', 'America/Chicago', 32.77670, -96.79700),
    ('US', 'Memphis', 'TN', 'America/Chicago', 35.14950, -90.04900),
    ('US', 'Columbus', 'OH', 'America/New_York', 39.96120, -82.99880),
    ('CA', 'Toronto', 'ON', 'America/Toronto', 43.65320, -79.38320),
    ('CA', 'Vancouver', 'BC', 'America/Vancouver', 49.28270, -123.12070),
    ('CA', 'Montreal', 'QC', 'America/Toronto', 45.50170, -73.56730),
    ('MX', 'Mexico City', 'CDMX', 'America/Mexico_City', 19.43260, -99.13320),
    ('MX', 'Guadalajara', 'Jalisco', 'America/Mexico_City', 20.65970, -103.34960),
    ('MX', 'Monterrey', 'Nuevo León', 'America/Monterrey', 25.68660, -100.31610),
    ('BR', 'São Paulo', 'SP', 'America/Sao_Paulo', -23.55050, -46.63330),
    ('BR', 'Rio de Janeiro', 'RJ', 'America/Sao_Paulo', -22.90680, -43.17290),
    ('BR', 'Curitiba', 'PR', 'America/Sao_Paulo', -25.42840, -49.27330),
    ('BR', 'Belo Horizonte', 'MG', 'America/Sao_Paulo', -19.91670, -43.93450),
    ('GB', 'London', 'England', 'Europe/London', 51.50740, -0.12780),
    ('GB', 'Manchester', 'England', 'Europe/London', 53.48080, -2.24260),
    ('GB', 'Birmingham', 'England', 'Europe/London', 52.48620, -1.89040),
    ('GB', 'Edinburgh', 'Scotland', 'Europe/London', 55.95330, -3.18830),
    ('DE', 'Berlin', 'Berlin', 'Europe/Berlin', 52.52000, 13.40500),
    ('DE', 'Munich', 'Bavaria', 'Europe/Berlin', 48.13510, 11.58200),
    ('DE', 'Hamburg', 'Hamburg', 'Europe/Berlin', 53.55110, 9.99370),
    ('DE', 'Frankfurt', 'Hesse', 'Europe/Berlin', 50.11090, 8.68210),
    ('FR', 'Paris', 'Île-de-France', 'Europe/Paris', 48.85660, 2.35220),
    ('FR', 'Lyon', 'Auvergne-Rhône-Alpes', 'Europe/Paris', 45.76400, 4.83570),
    ('FR', 'Marseille', 'Provence-Alpes-Côte d''Azur', 'Europe/Paris', 43.29650, 5.36980),
    ('ES', 'Madrid', 'Community of Madrid', 'Europe/Madrid', 40.41680, -3.70380),
    ('ES', 'Barcelona', 'Catalonia', 'Europe/Madrid', 41.38510, 2.17340),
    ('IT', 'Milan', 'Lombardy', 'Europe/Rome', 45.46420, 9.19000),
    ('IT', 'Rome', 'Lazio', 'Europe/Rome', 41.90280, 12.49640),
    ('NL', 'Amsterdam', 'North Holland', 'Europe/Amsterdam', 52.36760, 4.90410),
    ('NL', 'Rotterdam', 'South Holland', 'Europe/Amsterdam', 51.92440, 4.47770),
    ('IE', 'Dublin', 'Leinster', 'Europe/Dublin', 53.34980, -6.26030),
    ('SE', 'Stockholm', 'Stockholm County', 'Europe/Stockholm', 59.32930, 18.06860),
    ('PL', 'Warsaw', 'Masovia', 'Europe/Warsaw', 52.22970, 21.01220),
    ('PL', 'Kraków', 'Lesser Poland', 'Europe/Warsaw', 50.06470, 19.94500),
    ('CH', 'Zurich', 'Zurich', 'Europe/Zurich', 47.37690, 8.54170),
    ('JP', 'Tokyo', 'Tokyo', 'Asia/Tokyo', 35.67620, 139.65030),
    ('JP', 'Osaka', 'Osaka', 'Asia/Tokyo', 34.69370, 135.50230),
    ('CN', 'Shanghai', 'Shanghai', 'Asia/Shanghai', 31.23040, 121.47370),
    ('CN', 'Shenzhen', 'Guangdong', 'Asia/Shanghai', 22.54310, 114.05790),
    ('CN', 'Beijing', 'Beijing', 'Asia/Shanghai', 39.90420, 116.40740),
    ('IN', 'Bengaluru', 'Karnataka', 'Asia/Kolkata', 12.97160, 77.59460),
    ('IN', 'Mumbai', 'Maharashtra', 'Asia/Kolkata', 19.07600, 72.87770),
    ('IN', 'Hyderabad', 'Telangana', 'Asia/Kolkata', 17.38500, 78.48670),
    ('IN', 'New Delhi', 'Delhi', 'Asia/Kolkata', 28.61390, 77.20900),
    ('AU', 'Sydney', 'NSW', 'Australia/Sydney', -33.86880, 151.20930),
    ('AU', 'Melbourne', 'VIC', 'Australia/Melbourne', -37.81360, 144.96310),
    ('SG', 'Singapore', NULL, 'Asia/Singapore', 1.35210, 103.81980),
    ('KR', 'Seoul', NULL, 'Asia/Seoul', 37.56650, 126.97800),
    ('ZA', 'Johannesburg', 'Gauteng', 'Africa/Johannesburg', -26.20410, 28.04730),
    ('ZA', 'Cape Town', 'Western Cape', 'Africa/Johannesburg', -33.92490, 18.42410),
    ('AE', 'Dubai', 'Dubai', 'Asia/Dubai', 25.20480, 55.27080);

CREATE FUNCTION seed.city(text) RETURNS int LANGUAGE sql STABLE AS 'SELECT id FROM core.cities WHERE name = $1';

INSERT INTO core.offices (code, name, city_id, address, is_hq, opened_on) VALUES
    ('SEA', 'Seattle Headquarters', seed.city('Seattle'), '1200 Northstar Way', true, '2004-03-01'),
    ('NYC', 'New York', seed.city('New York'), '350 Hudson Street, Floor 18', false, '2008-09-15'),
    ('AUS', 'Austin Tech Hub', seed.city('Austin'), '500 Congress Avenue', false, '2014-06-02'),
    ('CHI', 'Chicago', seed.city('Chicago'), '233 S Wacker Drive, Suite 4100', false, '2010-01-11'),
    ('ATL', 'Atlanta Customer Center', seed.city('Atlanta'), '75 Peachtree Place', false, '2012-04-23'),
    ('TOR', 'Toronto', seed.city('Toronto'), '100 King Street West', false, '2011-08-08'),
    ('MEX', 'Mexico City', seed.city('Mexico City'), 'Paseo de la Reforma 250', false, '2015-02-16'),
    ('SAO', 'São Paulo', seed.city('São Paulo'), 'Avenida Paulista 1500', false, '2013-05-06'),
    ('LON', 'London', seed.city('London'), '20 Fenchurch Street', false, '2009-11-02'),
    ('BER', 'Berlin', seed.city('Berlin'), 'Friedrichstraße 68', false, '2012-10-01'),
    ('PAR', 'Paris', seed.city('Paris'), '15 Rue de la Paix', false, '2013-03-18'),
    ('AMS', 'Amsterdam EMEA Hub', seed.city('Amsterdam'), 'Herengracht 420', false, '2011-01-24'),
    ('DUB', 'Dublin', seed.city('Dublin'), '1 Grand Canal Square', false, '2016-07-04'),
    ('WAW', 'Warsaw Shared Services', seed.city('Warsaw'), 'ul. Prosta 70', false, '2017-09-11'),
    ('BLR', 'Bengaluru Engineering', seed.city('Bengaluru'), 'Outer Ring Road, Bellandur', false, '2015-11-30'),
    ('HYD', 'Hyderabad', seed.city('Hyderabad'), 'HITEC City, Madhapur', false, '2019-02-04'),
    ('SIN', 'Singapore APAC Hub', seed.city('Singapore'), '8 Marina View', false, '2012-06-18'),
    ('TYO', 'Tokyo', seed.city('Tokyo'), '2-1-1 Marunouchi, Chiyoda-ku', false, '2014-04-01'),
    ('SHA', 'Shanghai', seed.city('Shanghai'), '1000 Lujiazui Ring Road', false, '2016-03-21'),
    ('SYD', 'Sydney', seed.city('Sydney'), '1 Martin Place', false, '2014-10-13'),
    ('JNB', 'Johannesburg', seed.city('Johannesburg'), '144 Katherine Street, Sandton', false, '2018-05-14'),
    ('DXB', 'Dubai', seed.city('Dubai'), 'Dubai Internet City, Building 5', false, '2018-01-08');

-- Daily rates: each currency drifts around its centre with a yearly cycle and some noise.
INSERT INTO core.exchange_rates (currency_code, rate_date, usd_rate)
SELECT f.code, d::date,
       CASE WHEN f.code = 'USD' THEN 1
            ELSE round(f.usd_rate * (1 + 0.05 * sin(2 * pi() * (d::date - date '2019-01-01') / 365.0 + ascii(f.code))
                                       + 0.02 * sin((d::date - date '2019-01-01') / 41.0 + ascii(substr(f.code, 2)))
                                       + (random() - 0.5) * 0.01)::numeric, 6) END
FROM seed.fx f, seed.clock c, generate_series(c.start, c.now, interval '1 day') d;

-- ---------------------------------------------------------------- hr: org structure

CREATE TABLE seed.divisions (code text PRIMARY KEY, name text, family text, reports_to text, weight int);
INSERT INTO seed.divisions VALUES
    ('EXEC', 'Executive Office', 'Executive', NULL, 1),
    ('SALES', 'Sales', 'Sales', 'Chief Revenue Officer', 14),
    ('MKT', 'Marketing', 'Marketing', 'Chief Marketing Officer', 5),
    ('ENG', 'Engineering', 'Engineering', 'Chief Technology Officer', 12),
    ('PROD', 'Product', 'Product', 'Chief Technology Officer', 3),
    ('OPS', 'Retail Operations', 'Operations', 'Chief Operating Officer', 20),
    ('SCM', 'Supply Chain', 'Supply Chain', 'Chief Operating Officer', 12),
    ('FIN', 'Finance', 'Finance', 'Chief Financial Officer', 5),
    ('HR', 'Human Resources', 'People', 'Chief People Officer', 3),
    ('LEGAL', 'Legal & Compliance', 'Legal', 'General Counsel', 2),
    ('SUP', 'Customer Experience', 'Customer Support', 'Chief Operating Officer', 15),
    ('IT', 'Information Technology', 'IT', 'Chief Information Officer', 5),
    ('DATA', 'Data & Analytics', 'Data', 'Chief Information Officer', 3);

CREATE TABLE seed.subs (division text, code text PRIMARY KEY, name text, weight int);
INSERT INTO seed.subs VALUES
    ('EXEC', 'EXEC-OFF', 'Office of the CEO', 1), ('EXEC', 'EXEC-STR', 'Corporate Strategy', 1),
    ('SALES', 'SALES-ENT', 'Enterprise Sales', 4), ('SALES', 'SALES-MID', 'Mid-Market Sales', 4),
    ('SALES', 'SALES-INS', 'Inside Sales', 5), ('SALES', 'SALES-OPS', 'Sales Operations', 1),
    ('MKT', 'MKT-BRAND', 'Brand Marketing', 1), ('MKT', 'MKT-DIG', 'Digital Marketing', 2),
    ('MKT', 'MKT-PMM', 'Product Marketing', 1), ('MKT', 'MKT-EVT', 'Events & Partnerships', 1),
    ('ENG', 'ENG-PLAT', 'Platform Engineering', 3), ('ENG', 'ENG-ECOM', 'E-commerce Engineering', 4),
    ('ENG', 'ENG-MOB', 'Mobile Engineering', 2), ('ENG', 'ENG-QA', 'Quality Engineering', 2),
    ('ENG', 'ENG-SRE', 'Site Reliability Engineering', 1),
    ('PROD', 'PROD-PM', 'Product Management', 2), ('PROD', 'PROD-UX', 'UX Design & Research', 1),
    ('OPS', 'OPS-STORE', 'Store Operations', 12), ('OPS', 'OPS-MERCH', 'Merchandising', 3),
    ('OPS', 'OPS-LP', 'Loss Prevention', 1),
    ('SCM', 'SCM-PROC', 'Procurement', 2), ('SCM', 'SCM-LOG', 'Logistics', 4),
    ('SCM', 'SCM-WH', 'Warehouse Operations', 8), ('SCM', 'SCM-PLAN', 'Demand Planning', 1),
    ('FIN', 'FIN-ACC', 'Accounting', 2), ('FIN', 'FIN-FPA', 'FP&A', 1), ('FIN', 'FIN-TRS', 'Treasury', 1),
    ('FIN', 'FIN-TAX', 'Tax', 1), ('FIN', 'FIN-AR', 'Billing & Collections', 2),
    ('HR', 'HR-TA', 'Talent Acquisition', 2), ('HR', 'HR-PO', 'People Operations', 2),
    ('HR', 'HR-CB', 'Compensation & Benefits', 1), ('HR', 'HR-LD', 'Learning & Development', 1),
    ('LEGAL', 'LEGAL-CORP', 'Corporate Legal', 1), ('LEGAL', 'LEGAL-CMP', 'Compliance & Privacy', 1),
    ('SUP', 'SUP-T1', 'Tier 1 Support', 8), ('SUP', 'SUP-T2', 'Tier 2 Support', 3),
    ('SUP', 'SUP-CS', 'Customer Success', 2),
    ('IT', 'IT-INFRA', 'IT Infrastructure', 2), ('IT', 'IT-SEC', 'Information Security', 1),
    ('IT', 'IT-DESK', 'Service Desk', 2), ('IT', 'IT-ERP', 'Business Applications', 1),
    ('DATA', 'DATA-ENG', 'Data Engineering', 2), ('DATA', 'DATA-BI', 'Business Intelligence', 1),
    ('DATA', 'DATA-SCI', 'Data Science', 1);

INSERT INTO hr.departments (name, code, cost_center)
SELECT name, code, 'CC-' || code FROM seed.divisions ORDER BY code;
INSERT INTO hr.departments (name, code, parent_id, cost_center)
SELECT s.name, s.code, d.id, 'CC-' || s.code
FROM seed.subs s JOIN hr.departments d ON d.code = s.division ORDER BY s.code;

INSERT INTO finance.cost_centers (code, name, department_id)
SELECT cost_center, name, id FROM hr.departments;

-- Job titles: the C-suite, a VP per division, a director and a manager per department, and four
-- individual-contributor levels per job family.
INSERT INTO hr.job_titles (title, family, level, min_salary, max_salary) VALUES
    ('Chief Executive Officer', 'Executive', 10, 700000, 1200000);
INSERT INTO hr.job_titles (title, family, level, min_salary, max_salary)
SELECT DISTINCT reports_to, 'Executive', 9, 380000, 650000 FROM seed.divisions WHERE reports_to IS NOT NULL;
INSERT INTO hr.job_titles (title, family, level, min_salary, max_salary)
SELECT 'VP, ' || name, family, 8, 240000, 380000 FROM seed.divisions WHERE code <> 'EXEC';
INSERT INTO hr.job_titles (title, family, level, min_salary, max_salary)
SELECT 'Director, ' || s.name, d.family, 7, 170000, 250000 FROM seed.subs s JOIN seed.divisions d ON d.code = s.division;
INSERT INTO hr.job_titles (title, family, level, min_salary, max_salary)
SELECT 'Manager, ' || s.name, d.family, 5, 110000, 165000 FROM seed.subs s JOIN seed.divisions d ON d.code = s.division;

CREATE TABLE seed.ic_titles (family text, level int, title text);
INSERT INTO seed.ic_titles VALUES
    ('Executive', 1, 'Executive Assistant'), ('Executive', 2, 'Senior Executive Assistant'),
    ('Executive', 3, 'Strategy Analyst'), ('Executive', 4, 'Chief of Staff'),
    ('Sales', 1, 'Sales Development Representative'), ('Sales', 2, 'Account Executive'),
    ('Sales', 3, 'Senior Account Executive'), ('Sales', 4, 'Strategic Account Director'),
    ('Marketing', 1, 'Marketing Coordinator'), ('Marketing', 2, 'Marketing Specialist'),
    ('Marketing', 3, 'Senior Marketing Manager'), ('Marketing', 4, 'Principal Marketing Strategist'),
    ('Engineering', 1, 'Software Engineer I'), ('Engineering', 2, 'Software Engineer II'),
    ('Engineering', 3, 'Senior Software Engineer'), ('Engineering', 4, 'Staff Software Engineer'),
    ('Product', 1, 'Associate Product Manager'), ('Product', 2, 'Product Manager'),
    ('Product', 3, 'Senior Product Manager'), ('Product', 4, 'Principal Product Manager'),
    ('Operations', 1, 'Sales Associate'), ('Operations', 2, 'Shift Supervisor'),
    ('Operations', 3, 'Assistant Store Manager'), ('Operations', 4, 'Store Manager'),
    ('Supply Chain', 1, 'Warehouse Associate'), ('Supply Chain', 2, 'Logistics Coordinator'),
    ('Supply Chain', 3, 'Supply Chain Analyst'), ('Supply Chain', 4, 'Senior Supply Chain Planner'),
    ('Finance', 1, 'Accounting Clerk'), ('Finance', 2, 'Financial Analyst'),
    ('Finance', 3, 'Senior Financial Analyst'), ('Finance', 4, 'Finance Manager'),
    ('People', 1, 'HR Coordinator'), ('People', 2, 'HR Generalist'),
    ('People', 3, 'Senior HR Business Partner'), ('People', 4, 'Principal People Partner'),
    ('Legal', 1, 'Paralegal'), ('Legal', 2, 'Compliance Analyst'),
    ('Legal', 3, 'Corporate Counsel'), ('Legal', 4, 'Senior Corporate Counsel'),
    ('Customer Support', 1, 'Customer Support Agent'), ('Customer Support', 2, 'Senior Support Agent'),
    ('Customer Support', 3, 'Support Specialist'), ('Customer Support', 4, 'Customer Success Manager'),
    ('IT', 1, 'IT Support Technician'), ('IT', 2, 'Systems Administrator'),
    ('IT', 3, 'Senior Systems Engineer'), ('IT', 4, 'IT Architect'),
    ('Data', 1, 'Data Analyst'), ('Data', 2, 'Data Engineer'),
    ('Data', 3, 'Senior Data Scientist'), ('Data', 4, 'Principal Data Scientist');
INSERT INTO hr.job_titles (title, family, level, min_salary, max_salary)
SELECT title, family, level, (array[38000, 55000, 80000, 110000])[level], (array[60000, 85000, 125000, 175000])[level]
FROM seed.ic_titles;

-- ---------------------------------------------------------------- hr: people

-- Offices weighted by size (HQ and the big hubs hire the most).
CREATE TABLE seed.office_pool AS
SELECT array_agg(o.id) AS a
FROM core.offices o, generate_series(1, CASE o.code WHEN 'SEA' THEN 8 WHEN 'AUS' THEN 4 WHEN 'BLR' THEN 4
                                       WHEN 'NYC' THEN 3 WHEN 'LON' THEN 3 WHEN 'AMS' THEN 3 WHEN 'ATL' THEN 3
                                       WHEN 'WAW' THEN 2 WHEN 'SIN' THEN 2 ELSE 1 END);

-- Staging: every employee with an explicit id, so managers can be referenced before insert.
CREATE TABLE seed.emp (
    id int PRIMARY KEY, dept_id int, title_id int, manager_id int, office_id int, level int, family text
);

-- 1: CEO. 2..: one per C-suite title.
INSERT INTO seed.emp
SELECT 1, d.id, t.id, NULL, (SELECT id FROM core.offices WHERE is_hq), 10, 'Executive'
FROM hr.departments d, hr.job_titles t WHERE d.code = 'EXEC-OFF' AND t.title = 'Chief Executive Officer';
INSERT INTO seed.emp
SELECT 1 + row_number() OVER (ORDER BY t.title), d.id, t.id, 1, (SELECT id FROM core.offices WHERE is_hq), 9, 'Executive'
FROM hr.job_titles t JOIN hr.departments d ON d.code = 'EXEC-OFF'
WHERE t.level = 9;

-- VPs report to their division's C-suite officer.
INSERT INTO seed.emp
SELECT 100 + row_number() OVER (ORDER BY dv.code), d.id, t.id, cs.id,
       CASE WHEN random() < 0.7 THEN (SELECT id FROM core.offices WHERE is_hq) ELSE seed.pick(p.a) END, 8, dv.family
FROM seed.divisions dv
JOIN hr.departments d ON d.code = dv.code
JOIN hr.job_titles t ON t.title = 'VP, ' || dv.name
JOIN hr.job_titles ct ON ct.title = dv.reports_to
JOIN seed.emp cs ON cs.title_id = ct.id
CROSS JOIN seed.office_pool p;

-- Directors report to the VP of their division.
INSERT INTO seed.emp
SELECT 200 + row_number() OVER (ORDER BY s.code), d.id, t.id,
       coalesce(vp.id, 1),
       CASE WHEN random() < 0.5 THEN (SELECT id FROM core.offices WHERE is_hq) ELSE seed.pick(p.a) END, 7, dv.family
FROM seed.subs s
JOIN seed.divisions dv ON dv.code = s.division
JOIN hr.departments d ON d.code = s.code
JOIN hr.job_titles t ON t.title = 'Director, ' || s.name
LEFT JOIN hr.departments vd ON vd.code = dv.code
LEFT JOIN seed.emp vp ON vp.dept_id = vd.id AND vp.level = 8
CROSS JOIN seed.office_pool p;

-- 2,000 people managers spread by department size, each reporting to their department's director.
CREATE TABLE seed.sub_pool AS
SELECT array_agg(s.code) AS a FROM seed.subs s, generate_series(1, s.weight);
INSERT INTO seed.emp
SELECT 1000 + g, d.id, t.id, dir.id, seed.pick(p.a), 5, dv.family
FROM generate_series(1, 2000) g
CROSS JOIN seed.sub_pool sp
CROSS JOIN seed.office_pool p
CROSS JOIN LATERAL (SELECT seed.pick(sp.a) AS code WHERE g > 0) pick
JOIN seed.subs s ON s.code = pick.code
JOIN seed.divisions dv ON dv.code = s.division
JOIN hr.departments d ON d.code = s.code
JOIN hr.job_titles t ON t.title = 'Manager, ' || s.name
JOIN seed.emp dir ON dir.dept_id = d.id AND dir.level = 7;

-- 26,000 individual contributors under a random manager, mostly in the manager's office.
INSERT INTO seed.emp
SELECT 5000 + g, m.dept_id, t.id, m.id,
       CASE WHEN random() < 0.75 THEN m.office_id ELSE seed.pick(p.a) END, lvl.level, m.family
FROM generate_series(1, 26000) g
CROSS JOIN seed.office_pool p
CROSS JOIN LATERAL (SELECT 1000 + seed.rint(1, 2000) + g * 0 AS id) mid
JOIN seed.emp m ON m.id = mid.id
CROSS JOIN LATERAL (SELECT CASE WHEN r < 0.40 THEN 1 WHEN r < 0.72 THEN 2 WHEN r < 0.92 THEN 3 ELSE 4 END AS level
                    FROM (SELECT random() + g * 0 AS r) x) lvl
JOIN seed.ic_titles it ON it.family = m.family AND it.level = lvl.level
JOIN hr.job_titles t ON t.title = it.title;

ALTER TABLE seed.emp ADD first text, ADD last text;
UPDATE seed.emp SET first = seed.pick((SELECT a FROM seed.first_names)), last = seed.pick((SELECT a FROM seed.last_names));

INSERT INTO hr.employees (id, employee_number, first_name, last_name, email, phone, birth_date, hire_date,
                          termination_date, employment_type, department_id, job_title_id, manager_id, office_id,
                          is_remote, created_at, updated_at)
OVERRIDING SYSTEM VALUE
SELECT e.id, 'E' || lpad(e.id::text, 6, '0'), e.first, e.last,
       -- Repeated names get a number: jane.smith@, jane.smith2@...
       lower(regexp_replace(e.first || '.' || e.last, '[^A-Za-z.]', '', 'g'))
           || coalesce(nullif(row_number() OVER (PARTITION BY e.first, e.last ORDER BY e.id), 1)::text, '')
           || '@northstar-global.example',
       c.phone_prefix || ' ' || seed.digits(3) || ' ' || seed.digits(3) || ' ' || seed.digits(4),
       h.hire_date - make_interval(years => seed.rint(21, 48), days => seed.rint(0, 364)),
       h.hire_date,
       CASE WHEN e.level < 7 AND random() < 0.13
            THEN least(h.hire_date + seed.rint(90, 2500), date '2026-10-10') END,
       CASE WHEN e.level >= 5 THEN 'full_time'::hr.employment_type
            ELSE (array['full_time','full_time','full_time','full_time','full_time','full_time','full_time',
                        'part_time','contractor','intern'])[seed.rint(1, 10)]::hr.employment_type END,
       e.dept_id, e.title_id, e.manager_id, e.office_id,
       e.family IN ('Engineering', 'Data', 'Product', 'Marketing', 'IT') AND random() < 0.3,
       h.hire_date::timestamptz, h.hire_date::timestamptz
FROM seed.emp e
JOIN core.offices o ON o.id = e.office_id
JOIN core.cities ci ON ci.id = o.city_id
JOIN core.countries c ON c.code = ci.country_code
CROSS JOIN LATERAL (
    SELECT CASE WHEN e.level >= 7 THEN date '2006-01-01' + seed.rint(0, 6000) + e.id * 0
                ELSE (seed.between('2010-01-01', '2026-09-30', 1.8) + e.id * interval '0 s')::date END AS hire_date
) h
;
SELECT setval(pg_get_serial_sequence('hr.employees', 'id'), (SELECT max(id) FROM hr.employees));

-- Leaders head their departments.
UPDATE hr.departments d SET head_id = e.id
FROM seed.emp e WHERE e.dept_id = d.id AND e.level >= 7 AND e.level <> 9;
UPDATE hr.departments d SET head_id = 1 WHERE code = 'EXEC';

-- Salary history: a starting salary and a raise most years, in the office's currency.
INSERT INTO hr.salaries (employee_id, effective_from, effective_to, amount, currency_code, bonus_target_pct)
SELECT e.id, y.from_date,
       lead(y.from_date) OVER (PARTITION BY e.id ORDER BY y.from_date) - 1,
       round(base.usd * power(1.035, y.n) * cm.pay * fx.usd_rate, -2),
       c.currency_code,
       CASE WHEN t.level >= 9 THEN 50 WHEN t.level >= 7 THEN 25 WHEN t.level >= 5 THEN 15 WHEN t.level >= 3 THEN 10 ELSE 0 END
FROM hr.employees e
JOIN hr.job_titles t ON t.id = e.job_title_id
JOIN core.offices o ON o.id = e.office_id
JOIN core.cities ci ON ci.id = o.city_id
JOIN core.countries c ON c.code = ci.country_code
JOIN seed.country_mix cm ON cm.code = c.code
JOIN seed.fx fx ON fx.code = c.currency_code
CROSS JOIN LATERAL (SELECT t.min_salary + (t.max_salary - t.min_salary) * random()::numeric * 0.6 + e.id * 0 AS usd) base
CROSS JOIN LATERAL (
    SELECT n, (e.hire_date + make_interval(years => n))::date AS from_date
    FROM generate_series(0, 20) n
    WHERE e.hire_date + make_interval(years => n) <= coalesce(e.termination_date, date '2026-10-14')
      AND (n = 0 OR random() < 0.85)
) y;

INSERT INTO hr.time_off_requests (employee_id, kind, starts_on, ends_on, status, approver_id, note)
SELECT e.id, k.kind, s.starts_on, s.starts_on + k.days,
       CASE WHEN s.starts_on > date '2026-10-14' THEN (array['pending','approved','approved'])[seed.rint(1, 3)]
            ELSE (array['approved','approved','approved','approved','approved','approved','rejected','cancelled'])[seed.rint(1, 8)]
       END::hr.time_off_status,
       e.manager_id,
       CASE WHEN random() < 0.2 THEN seed.pick(array['Family trip','Wedding','Moving house','Medical appointment',
                                                     'School holidays','Conference','Personal matters']) END
FROM hr.employees e
CROSS JOIN LATERAL generate_series(1, seed.rint(1, 8) + e.id * 0) g
CROSS JOIN LATERAL (SELECT (greatest(e.hire_date, date '2019-01-01')
                            + floor(random() * (least(coalesce(e.termination_date, date '2026-12-20'), date '2026-12-20')
                                                - greatest(e.hire_date, date '2019-01-01') + 1))::int + g * 0) AS starts_on) s
CROSS JOIN LATERAL (SELECT kind, days FROM (VALUES
    ('vacation', seed.rint(1, 14)), ('vacation', seed.rint(2, 10)), ('vacation', seed.rint(1, 5)),
    ('sick', seed.rint(0, 3)), ('sick', seed.rint(0, 2)), ('parental', seed.rint(30, 120)),
    ('bereavement', seed.rint(1, 4)), ('unpaid', seed.rint(1, 20))) v(kind, days)
    ORDER BY random() + g * 0 LIMIT 1) k
WHERE s.starts_on >= greatest(e.hire_date, date '2019-01-01');

INSERT INTO hr.performance_reviews (employee_id, reviewer_id, period, rating, summary)
SELECT e.id, e.manager_id, 'FY' || y,
       (array[1,2,2,3,3,3,3,3,4,4,4,4,5,5])[seed.rint(1, 14)],
       seed.pick(array['Consistently exceeds expectations and mentors peers.',
                       'Solid performer; delivered all committed goals.',
                       'Strong collaborator, should take on more ownership.',
                       'Met most goals; needs to improve prioritization.',
                       'Outstanding year, ready for promotion.',
                       'Below expectations; performance plan agreed.',
                       'Great customer focus and reliable execution.'])
FROM hr.employees e
CROSS JOIN generate_series(2019, 2025) y
WHERE e.manager_id IS NOT NULL
  AND extract(year FROM e.hire_date) < y
  AND (e.termination_date IS NULL OR extract(year FROM e.termination_date) > y);

-- ---------------------------------------------------------------- catalog

CREATE TABLE seed.cat (parent text, name text, weight int);
INSERT INTO seed.cat VALUES
    ('Electronics', 'Laptops', 6),
    ('Electronics', 'Smartphones', 7),
    ('Electronics', 'Tablets', 4),
    ('Electronics', 'Audio', 8),
    ('Electronics', 'Cameras', 3),
    ('Electronics', 'TV & Home Theater', 4),
    ('Electronics', 'Accessories', 12),
    ('Home & Kitchen', 'Cookware', 7),
    ('Home & Kitchen', 'Small Appliances', 9),
    ('Home & Kitchen', 'Furniture', 5),
    ('Home & Kitchen', 'Bedding & Bath', 6),
    ('Home & Kitchen', 'Lighting', 4),
    ('Office', 'Office Supplies', 8),
    ('Office', 'Office Furniture', 3),
    ('Office', 'Printers & Ink', 4),
    ('Sports & Outdoors', 'Fitness', 5),
    ('Sports & Outdoors', 'Camping & Hiking', 4),
    ('Sports & Outdoors', 'Cycling', 3),
    ('Apparel', 'Men', 8),
    ('Apparel', 'Women', 9),
    ('Apparel', 'Kids', 5),
    ('Apparel', 'Footwear', 6),
    ('Health & Beauty', 'Skincare', 6),
    ('Health & Beauty', 'Personal Care', 6),
    ('Toys & Games', 'Toys', 5),
    ('Toys & Games', 'Video Games', 5),
    ('Grocery', 'Beverages', 6),
    ('Grocery', 'Snacks', 6),
    ('Automotive', 'Car Care', 3),
    ('Automotive', 'Tools', 3);

-- What each category sells, with a realistic price range (USD) and shipping weight.
CREATE TABLE seed.item (cat text, noun text, min_price numeric, max_price numeric, kg numeric);
INSERT INTO seed.item VALUES
    ('Laptops', 'Laptop', 499, 1899, 1.8),
    ('Laptops', 'Ultrabook', 899, 2499, 1.3),
    ('Laptops', 'Gaming Laptop', 1099, 3499, 2.6),
    ('Laptops', '2-in-1 Laptop', 599, 1699, 1.4),
    ('Laptops', 'Chromebook', 229, 599, 1.3),
    ('Smartphones', 'Smartphone', 199, 999, 0.19),
    ('Smartphones', '5G Phone', 349, 1299, 0.2),
    ('Smartphones', 'Foldable Phone', 999, 1899, 0.26),
    ('Tablets', 'Tablet', 179, 799, 0.5),
    ('Tablets', 'Mini Tablet', 129, 499, 0.3),
    ('Tablets', 'Pro Tablet', 799, 1499, 0.65),
    ('Audio', 'Wireless Earbuds', 29, 299, 0.06),
    ('Audio', 'Over-Ear Headphones', 59, 449, 0.3),
    ('Audio', 'Bluetooth Speaker', 29, 349, 0.7),
    ('Audio', 'Soundbar', 149, 899, 3.5),
    ('Audio', 'Turntable', 129, 699, 5),
    ('Cameras', 'Mirrorless Camera', 699, 2999, 0.7),
    ('Cameras', 'Action Camera', 149, 499, 0.15),
    ('Cameras', 'Instant Camera', 69, 199, 0.4),
    ('Cameras', 'Camera Lens', 199, 1899, 0.6),
    ('TV & Home Theater', '4K Smart TV', 299, 1999, 14),
    ('TV & Home Theater', 'OLED TV', 999, 3999, 18),
    ('TV & Home Theater', 'Projector', 249, 1999, 3),
    ('TV & Home Theater', 'Streaming Stick', 29, 69, 0.1),
    ('Accessories', 'USB-C Charger', 15, 69, 0.15),
    ('Accessories', 'Power Bank', 19, 99, 0.3),
    ('Accessories', 'Laptop Sleeve', 15, 59, 0.3),
    ('Accessories', 'Wireless Mouse', 12, 99, 0.1),
    ('Accessories', 'Mechanical Keyboard', 49, 229, 0.9),
    ('Accessories', 'HDMI Cable', 7, 29, 0.1),
    ('Accessories', '27-inch Monitor', 179, 699, 5.5),
    ('Cookware', 'Nonstick Skillet', 19, 89, 1.2),
    ('Cookware', 'Cast Iron Dutch Oven', 39, 349, 5.5),
    ('Cookware', 'Stainless Saucepan', 25, 149, 1.4),
    ('Cookware', 'Knife Set', 49, 399, 2.5),
    ('Cookware', 'Baking Sheet', 9, 39, 0.8),
    ('Small Appliances', 'Espresso Machine', 149, 899, 7),
    ('Small Appliances', 'Air Fryer', 59, 229, 5),
    ('Small Appliances', 'Blender', 39, 449, 3.5),
    ('Small Appliances', 'Stand Mixer', 199, 649, 10),
    ('Small Appliances', 'Electric Kettle', 25, 119, 1.2),
    ('Small Appliances', 'Toaster', 25, 149, 2),
    ('Small Appliances', 'Robot Vacuum', 199, 1099, 3.5),
    ('Furniture', 'Sofa', 599, 2499, 60),
    ('Furniture', 'Dining Table', 299, 1499, 40),
    ('Furniture', 'Bookshelf', 69, 399, 22),
    ('Furniture', 'Office Chair', 129, 699, 16),
    ('Furniture', 'Bed Frame', 199, 1299, 45),
    ('Furniture', 'Coffee Table', 89, 599, 15),
    ('Bedding & Bath', 'Duvet Cover', 39, 199, 1.2),
    ('Bedding & Bath', 'Sheet Set', 35, 249, 1.4),
    ('Bedding & Bath', 'Pillow', 15, 89, 0.9),
    ('Bedding & Bath', 'Bath Towel Set', 25, 129, 2),
    ('Bedding & Bath', 'Weighted Blanket', 59, 199, 7),
    ('Lighting', 'Floor Lamp', 49, 349, 6),
    ('Lighting', 'Desk Lamp', 19, 149, 1.5),
    ('Lighting', 'Smart Bulb 4pk', 29, 89, 0.4),
    ('Lighting', 'Pendant Light', 59, 449, 2.5),
    ('Lighting', 'LED Strip', 15, 59, 0.3),
    ('Office Supplies', 'Notebook', 3, 19, 0.3),
    ('Office Supplies', 'Gel Pen Set', 4, 24, 0.1),
    ('Office Supplies', 'Stapler', 6, 35, 0.4),
    ('Office Supplies', 'Sticky Notes 12pk', 5, 19, 0.3),
    ('Office Supplies', 'Copy Paper Ream', 6, 15, 2.5),
    ('Office Supplies', 'Label Maker', 25, 89, 0.5),
    ('Office Furniture', 'Standing Desk', 249, 999, 30),
    ('Office Furniture', 'Ergonomic Chair', 179, 1499, 18),
    ('Office Furniture', 'Filing Cabinet', 99, 449, 25),
    ('Office Furniture', 'Monitor Arm', 39, 229, 3),
    ('Printers & Ink', 'Laser Printer', 129, 799, 11),
    ('Printers & Ink', 'Inkjet Printer', 69, 299, 6),
    ('Printers & Ink', 'Toner Cartridge', 39, 189, 1),
    ('Printers & Ink', 'Ink Cartridge', 19, 69, 0.2),
    ('Fitness', 'Yoga Mat', 19, 99, 1.2),
    ('Fitness', 'Adjustable Dumbbells', 99, 449, 22),
    ('Fitness', 'Resistance Bands', 12, 39, 0.4),
    ('Fitness', 'Treadmill', 599, 2299, 80),
    ('Fitness', 'Exercise Bike', 299, 1799, 40),
    ('Camping & Hiking', '2-Person Tent', 89, 499, 2.5),
    ('Camping & Hiking', 'Sleeping Bag', 39, 329, 1.4),
    ('Camping & Hiking', 'Hiking Backpack 45L', 69, 299, 1.6),
    ('Camping & Hiking', 'Camp Stove', 29, 149, 1),
    ('Camping & Hiking', 'Headlamp', 15, 79, 0.1),
    ('Cycling', 'Road Bike', 699, 3999, 9),
    ('Cycling', 'Mountain Bike', 499, 2999, 13),
    ('Cycling', 'Bike Helmet', 39, 199, 0.3),
    ('Cycling', 'Bike Lights Set', 19, 89, 0.2),
    ('Cycling', 'E-Bike', 1199, 3999, 24),
    ('Men', 'T-Shirt', 12, 39, 0.2),
    ('Men', 'Chinos', 35, 89, 0.5),
    ('Men', 'Hoodie', 35, 99, 0.7),
    ('Men', 'Rain Jacket', 59, 249, 0.6),
    ('Men', 'Oxford Shirt', 29, 89, 0.3),
    ('Men', 'Slim Jeans', 39, 129, 0.7),
    ('Women', 'Wrap Dress', 39, 179, 0.4),
    ('Women', 'Leggings', 19, 89, 0.3),
    ('Women', 'Silk Blouse', 39, 149, 0.2),
    ('Women', 'Puffer Jacket', 79, 279, 0.9),
    ('Women', 'Cardigan', 35, 119, 0.5),
    ('Women', 'High-Rise Jeans', 39, 139, 0.6),
    ('Kids', 'Kids T-Shirt', 8, 24, 0.15),
    ('Kids', 'Kids Rain Jacket', 29, 79, 0.4),
    ('Kids', 'Pajama Set', 15, 39, 0.3),
    ('Kids', 'School Backpack', 25, 69, 0.6),
    ('Footwear', 'Running Shoes', 59, 199, 0.7),
    ('Footwear', 'Sneakers', 49, 149, 0.8),
    ('Footwear', 'Hiking Boots', 89, 249, 1.3),
    ('Footwear', 'Sandals', 19, 79, 0.5),
    ('Footwear', 'Leather Dress Shoes', 79, 299, 1.1),
    ('Skincare', 'Daily Moisturizer', 9, 59, 0.2),
    ('Skincare', 'Sunscreen SPF 50', 8, 35, 0.15),
    ('Skincare', 'Vitamin C Serum', 15, 89, 0.1),
    ('Skincare', 'Gentle Cleanser', 7, 35, 0.2),
    ('Skincare', 'Eye Cream', 15, 79, 0.05),
    ('Personal Care', 'Electric Toothbrush', 29, 229, 0.5),
    ('Personal Care', 'Hair Dryer', 29, 399, 0.8),
    ('Personal Care', 'Beard Trimmer', 19, 129, 0.3),
    ('Personal Care', 'Shampoo', 5, 29, 0.4),
    ('Personal Care', 'Body Wash', 4, 19, 0.5),
    ('Toys', 'Building Set', 19, 249, 1.2),
    ('Toys', 'Plush Toy', 9, 39, 0.3),
    ('Toys', 'RC Car', 29, 149, 1),
    ('Toys', '1000-Piece Puzzle', 12, 35, 0.7),
    ('Toys', 'Doll House', 49, 199, 6),
    ('Video Games', 'Game Console', 299, 549, 3.5),
    ('Video Games', 'Wireless Controller', 39, 79, 0.3),
    ('Video Games', 'Video Game', 19, 69, 0.1),
    ('Video Games', 'Gaming Headset', 39, 249, 0.35),
    ('Beverages', 'Coffee Beans 1kg', 14, 39, 1),
    ('Beverages', 'Green Tea 100ct', 6, 19, 0.3),
    ('Beverages', 'Sparkling Water 12pk', 5, 12, 4.5),
    ('Beverages', 'Cold Brew 6pk', 9, 19, 2),
    ('Snacks', 'Protein Bars 12pk', 15, 32, 0.7),
    ('Snacks', 'Trail Mix 500g', 6, 14, 0.5),
    ('Snacks', 'Dark Chocolate 6pk', 8, 24, 0.6),
    ('Snacks', 'Granola 750g', 5, 12, 0.75),
    ('Car Care', 'Car Wash Kit', 19, 69, 3),
    ('Car Care', 'Dash Cam', 49, 299, 0.3),
    ('Car Care', 'Tire Inflator', 29, 99, 1.5),
    ('Car Care', 'Jump Starter', 59, 199, 1.2),
    ('Car Care', 'All-Weather Floor Mats', 39, 149, 4),
    ('Tools', 'Cordless Drill', 59, 299, 1.8),
    ('Tools', 'Socket Set 120pc', 39, 199, 5),
    ('Tools', 'Rolling Tool Box', 79, 399, 15),
    ('Tools', 'LED Work Light', 19, 89, 1.2);

INSERT INTO catalog.categories (name, slug)
SELECT DISTINCT parent, lower(regexp_replace(parent, '[^A-Za-z]+', '-', 'g')) FROM seed.cat ORDER BY 1;
INSERT INTO catalog.categories (name, slug, parent_id)
SELECT c.name, lower(regexp_replace(c.parent || '-' || c.name, '[^A-Za-z]+', '-', 'g')), p.id
FROM seed.cat c JOIN catalog.categories p ON p.name = c.parent ORDER BY c.parent, c.name;

-- Fictional brands, each selling in one top-level category; Northstar's own labels sell everywhere.
CREATE TABLE seed.brand (name text, parent text, country char(2), private bool);
INSERT INTO seed.brand VALUES
    ('Voltix', 'Electronics', 'US', false), ('Lumina', 'Electronics', 'KR', false), ('Pixelcraft', 'Electronics', 'JP', false),
    ('Nimbus', 'Electronics', 'US', false), ('Kestrel Audio', 'Electronics', 'GB', false), ('Zenith Labs', 'Electronics', 'CN', false),
    ('Orbit Mobile', 'Electronics', 'CN', false), ('Halcyon', 'Electronics', 'DE', false),
    ('Cascade Kitchen', 'Home & Kitchen', 'US', false), ('Solace Home', 'Home & Kitchen', 'SE', false),
    ('Luxe Linen', 'Home & Kitchen', 'IT', false), ('Halo Lighting', 'Home & Kitchen', 'NL', false),
    ('Brewhaus', 'Home & Kitchen', 'DE', false), ('Cozy Nest', 'Home & Kitchen', 'CA', false),
    ('Quillmark', 'Office', 'US', false), ('Deskwise', 'Office', 'DE', false), ('Printwave', 'Office', 'JP', false),
    ('Trailmark', 'Sports & Outdoors', 'US', false), ('Peak Outfitters', 'Sports & Outdoors', 'CH', false),
    ('Velo Works', 'Sports & Outdoors', 'NL', false), ('Fitcore', 'Sports & Outdoors', 'US', false),
    ('Urban Thread', 'Apparel', 'GB', false), ('Coastline', 'Apparel', 'AU', false), ('Strideline', 'Apparel', 'US', false),
    ('Maison Verre', 'Apparel', 'FR', false), ('Little Explorers', 'Apparel', 'ES', false),
    ('Bloom Beauty', 'Health & Beauty', 'KR', false), ('Pure Botanics', 'Health & Beauty', 'FR', false),
    ('Aqua Pure', 'Health & Beauty', 'US', false),
    ('Playforge', 'Toys & Games', 'DK', false), ('Tinkerly', 'Toys & Games', 'US', false), ('Arcadia', 'Toys & Games', 'JP', false),
    ('Harvest Lane', 'Grocery', 'US', false), ('Alto Café', 'Grocery', 'BR', false), ('Green Valley', 'Grocery', 'IN', false),
    ('Roadster', 'Automotive', 'DE', false), ('Ironclad Tools', 'Automotive', 'US', false), ('Gearline', 'Automotive', 'MX', false),
    ('Northstar Basics', NULL, 'US', true), ('Northstar Home', NULL, 'US', true), ('Polaris Essentials', NULL, 'US', true);
-- Denmark isn't a sales country: its brand has no country on file.
INSERT INTO catalog.brands (name, country_code, is_private_label)
SELECT b.name, c.code, b.private FROM seed.brand b LEFT JOIN core.countries c ON c.code = b.country ORDER BY b.name;

CREATE TABLE seed.cat_pool AS
SELECT array_agg(c.name) AS a FROM seed.cat c, generate_series(1, c.weight);

INSERT INTO catalog.suppliers (name, tax_id, country_code, contact_email, payment_terms_days, rating, is_active)
SELECT n.name || ' ' || seed.pick(array['Manufacturing Co., Ltd.','Industries','Trading GmbH','Supply LLC','Group S.A.',
                                         'Components Pvt. Ltd.','Imports Inc.','Distribution B.V.']),
       'TX-' || lpad(g::text, 6, '0') || '-' || seed.digits(3),
       c.code,
       'sales' || g || '@' || lower(regexp_replace(n.name, '[^A-Za-z]', '', 'g')) || '.example',
       seed.pick(array[15, 30, 30, 45, 60, 90]),
       round((2.5 + random() * 2.5)::numeric, 1),
       random() < 0.93
FROM generate_series(1, 900) g
CROSS JOIN LATERAL (SELECT seed.pick(array['Pacific','Golden','Eastern','Atlas','Sunrise','Harbor','Delta','Summit',
                                           'Pioneer','Lotus','Crown','Silverline','Orient','Northfield','Bayview','Sterling'])
                           || ' ' || seed.pick(array['Precision','Textile','Electronics','Plastics','Metalworks','Foods',
                                                     'Home Goods','Optics','Packaging','Apparel','Components','Ceramics'])
                           || CASE WHEN g > 0 THEN '' END AS name) n
CROSS JOIN LATERAL (SELECT seed.pick(array['CN','CN','CN','CN','IN','IN','US','US','MX','DE','IT','PL','JP','KR','BR','GB','ES'])
                           || CASE WHEN g > 0 THEN '' END AS code) c;

-- 30,000 products. Low ids are the long-running best sellers (orders favour them).
INSERT INTO catalog.products (sku, name, description, category_id, brand_id, unit_cost, list_price, weight_kg,
                              attributes, is_active, launched_on, discontinued_on, created_at, updated_at)
SELECT 'NS-' || upper(left(regexp_replace(cat.name, '[^A-Za-z]', '', 'g'), 3)) || '-' || lpad(g::text, 6, '0'),
       b.name || ' ' || x.series || ' ' || x.noun || ' ' || x.model,
       'The ' || b.name || ' ' || x.series || ' ' || x.noun || ' — '
           || seed.pick(array['built to last','designed for everyday use','our best-selling model','engineered for performance',
                              'a customer favorite','sustainably made','compact and lightweight','with a 2-year warranty'])
           || '.',
       cat.id, b.id,
       round(p.price * (0.35 + random()::numeric * 0.3), 2),
       p.price,
       round((it.kg * (0.7 + random() * 0.6))::numeric, 3),
       jsonb_build_object('color', seed.pick(array['Black','White','Silver','Navy','Graphite','Red','Green','Beige','Blue','Rose Gold']),
                          'warranty_months', seed.pick(array[0, 6, 12, 12, 24, 36]),
                          'rating', round((3 + random() * 2)::numeric, 1),
                          'reviews', floor(power(random(), 3) * 20000)::int,
                          'eco_friendly', random() < 0.2),
       l.discontinued IS NULL,
       l.launched,
       l.discontinued,
       l.launched::timestamptz, l.launched::timestamptz
FROM generate_series(1, 30000) g
CROSS JOIN seed.cat_pool cp
CROSS JOIN LATERAL (SELECT seed.pick(cp.a) AS name WHERE g > 0) pick
JOIN seed.cat sc ON sc.name = pick.name
JOIN catalog.categories cat ON cat.name = sc.name AND cat.parent_id IS NOT NULL
CROSS JOIN LATERAL (SELECT * FROM seed.item i WHERE i.cat = sc.name ORDER BY random() + g * 0 LIMIT 1) it
CROSS JOIN LATERAL (
    SELECT bb.id, bb.name FROM catalog.brands bb JOIN seed.brand s ON s.name = bb.name
    WHERE s.parent = sc.parent OR (s.private AND random() < 0.15)
    ORDER BY random() + g * 0 LIMIT 1
) b
CROSS JOIN LATERAL (SELECT it.noun,
                           seed.pick(array['Pro','Max','Air','Lite','Plus','Ultra','Classic','Essential','Prime','Edge','One','Go','X','S']) AS series,
                           upper(chr(65 + seed.rint(0, 25))) || seed.rint(10, 990) + g * 0 AS model) x
CROSS JOIN LATERAL (SELECT round((it.min_price + (it.max_price - it.min_price) * power(random(), 1.6))::numeric, 0) - 0.01 + g * 0 AS price) p
CROSS JOIN LATERAL (
    SELECT launched, CASE WHEN random() < 0.12 THEN launched + seed.rint(200, 2000) END AS discontinued
    FROM (SELECT date '2014-01-01' + (power(random(), 0.7 + g / 30000.0) * 4600)::int + g * 0 AS launched) d
) l;
UPDATE catalog.products SET discontinued_on = NULL, is_active = true WHERE discontinued_on > date '2026-10-14';

INSERT INTO catalog.product_suppliers (product_id, supplier_id, supplier_sku, lead_time_days, cost, is_preferred)
SELECT p.id, s.supplier_id, 'SUP-' || upper(substr(md5(p.id::text || s.supplier_id), 1, 10)),
       seed.rint(3, 75), round(p.unit_cost * (0.9 + random()::numeric * 0.2), 2), s.n = 1
FROM catalog.products p
CROSS JOIN LATERAL (SELECT DISTINCT ON (sid) sid AS supplier_id, n
                    FROM (SELECT seed.rint(1, 900) + p.id * 0 AS sid, n FROM generate_series(1, seed.rint(1, 3) + p.id * 0) n) z
                    ORDER BY sid, n) s;

-- Prices change a few times over a product's life; the last change is today's list price.
INSERT INTO catalog.price_history (product_id, valid_from, list_price)
SELECT p.id, greatest(p.launched_on, date '2019-01-01') + (k * 400), round(p.list_price * (0.8 + k * 0.07), 2)
FROM catalog.products p, generate_series(0, 3) k
WHERE greatest(p.launched_on, date '2019-01-01') + (k * 400) < date '2026-10-01';

-- ---------------------------------------------------------------- inventory

INSERT INTO inventory.warehouses (code, name, city_id, capacity_m3, manager_id, is_active)
SELECT w.code, w.name, seed.city(w.city), w.capacity,
       (SELECT e.id FROM hr.employees e JOIN hr.departments d ON d.id = e.department_id
        WHERE d.code = 'SCM-WH' AND e.termination_date IS NULL ORDER BY e.id OFFSET w.n * 7 LIMIT 1),
       w.code <> 'WH-PHX'
FROM (VALUES
    (1, 'WH-MEM', 'Memphis Distribution Center', 'Memphis', 420000), (2, 'WH-CMH', 'Columbus Fulfillment Center', 'Columbus', 310000),
    (3, 'WH-DFW', 'Dallas Fulfillment Center', 'Dallas', 280000), (4, 'WH-PHX', 'Phoenix Returns Center', 'Phoenix', 90000),
    (5, 'WH-ATL', 'Atlanta Fulfillment Center', 'Atlanta', 260000), (6, 'WH-LAX', 'Los Angeles Port Center', 'Los Angeles', 350000),
    (7, 'WH-YYZ', 'Toronto Distribution Center', 'Toronto', 150000), (8, 'WH-MTY', 'Monterrey Distribution Center', 'Monterrey', 120000),
    (9, 'WH-CWB', 'Curitiba Distribution Center', 'Curitiba', 110000), (10, 'WH-BHX', 'Birmingham Fulfillment Centre', 'Birmingham', 200000),
    (11, 'WH-RTM', 'Rotterdam European Hub', 'Rotterdam', 380000), (12, 'WH-HAM', 'Hamburg Distribution Center', 'Hamburg', 160000),
    (13, 'WH-LYS', 'Lyon Distribution Center', 'Lyon', 120000), (14, 'WH-KRK', 'Kraków Fulfillment Center', 'Kraków', 140000),
    (15, 'WH-SZX', 'Shenzhen Sourcing Hub', 'Shenzhen', 300000), (16, 'WH-KIX', 'Osaka Distribution Center', 'Osaka', 130000),
    (17, 'WH-MEL', 'Melbourne Distribution Center', 'Melbourne', 110000), (18, 'WH-SIN', 'Singapore Regional Hub', 'Singapore', 140000),
    (19, 'WH-BOM', 'Mumbai Distribution Center', 'Mumbai', 120000), (20, 'WH-DXB', 'Dubai Logistics Hub', 'Dubai', 100000),
    (21, 'WH-JNB', 'Johannesburg Distribution Center', 'Johannesburg', 80000)
) w(n, code, name, city, capacity);

CREATE TABLE seed.region_wh AS
SELECT c.region, array_agg(w.id ORDER BY w.id) AS a
FROM inventory.warehouses w JOIN core.cities ci ON ci.id = w.city_id JOIN core.countries c ON c.code = ci.country_code
WHERE w.is_active GROUP BY c.region;

INSERT INTO inventory.stock_levels (warehouse_id, product_id, on_hand, reserved, reorder_point, bin_location, updated_at)
SELECT w.id, p.id, s.on_hand, floor(s.on_hand * random() * 0.15)::int, seed.rint(10, 200),
       chr(65 + seed.rint(0, 11)) || '-' || lpad(seed.rint(1, 60)::text, 2, '0') || '-' || lpad(seed.rint(1, 8)::text, 2, '0'),
       timestamptz '2026-10-14 06:00+00' - seed.rint(0, 30 * 24 * 60) * interval '1 minute'
FROM catalog.products p
CROSS JOIN inventory.warehouses w
CROSS JOIN LATERAL (SELECT CASE WHEN random() < 0.08 THEN 0 ELSE floor(power(random(), 2) * 2500)::int END + (p.id * 0) AS on_hand) s
WHERE w.is_active AND random() < CASE WHEN p.is_active THEN 0.3 ELSE 0.05 END;

INSERT INTO inventory.stock_movements (warehouse_id, product_id, movement, quantity, reference, moved_at)
SELECT s.warehouse_id, s.product_id, m.kind,
       CASE WHEN m.kind IN ('shipment', 'transfer_out') THEN -seed.rint(1, 40)
            WHEN m.kind = 'adjustment' THEN (array[-5,-3,-2,-1,1,2,3])[seed.rint(1, 7)]
            ELSE seed.rint(1, 400) END,
       CASE m.kind WHEN 'receipt' THEN 'PO-' WHEN 'shipment' THEN 'SO-' WHEN 'return' THEN 'RMA-'
                   WHEN 'adjustment' THEN 'CC-' ELSE 'TR-' END || seed.digits(8),
       seed.between('2019-01-01', '2026-10-14 06:00', 1.5)
FROM inventory.stock_levels s
CROSS JOIN LATERAL generate_series(1, seed.rint(2, 13) + s.product_id * 0) g
CROSS JOIN LATERAL (SELECT (array['receipt','shipment','shipment','shipment','shipment','transfer_in','transfer_out',
                                  'adjustment','return'])[seed.rint(1, 9) + g * 0]::inventory.movement_type AS kind) m;

-- ---------------------------------------------------------------- sales: customers

INSERT INTO sales.channels (name, is_online) VALUES
    ('Website', true), ('Mobile App', true), ('Retail Stores', false), ('Third-Party Marketplaces', true),
    ('Inside Sales', false), ('B2B Portal & EDI', true);

CREATE TABLE seed.country_pool AS
SELECT array_agg(m.code) AS a FROM seed.country_mix m, generate_series(1, m.customers);
CREATE TABLE seed.country_cities AS
SELECT country_code, array_agg(id) AS a FROM core.cities GROUP BY country_code;
CREATE TABLE seed.account_managers AS
SELECT array_agg(e.id) AS a FROM hr.employees e JOIN hr.departments d ON d.id = e.department_id
WHERE d.code IN ('SALES-ENT', 'SALES-MID') AND e.termination_date IS NULL;

-- 300,000 customers: mostly consumers, plus business and public-sector accounts.
INSERT INTO sales.customers (customer_number, segment, company_name, first_name, last_name, email, phone, country_code,
                             city_id, account_manager_id, credit_limit, loyalty_points, marketing_opt_in, created_at, updated_at)
SELECT 'C' || lpad(g::text, 8, '0'), seg.segment,
       CASE seg.segment
           WHEN 'consumer' THEN NULL
           WHEN 'government' THEN seed.pick(array['City of ', 'County of ', 'Public Schools of ', 'University of ', 'Port Authority of '])
                                  || ci.name
           ELSE seed.pick(array['Apex','Blue Harbor','Summit','Silverline','Pioneer','Evergreen','Granite','Crescent','Atlas',
                                'Vertex','Cobalt','Redwood','Lakeside','Ironwood','Brightpath','Northern','Pacific','Golden Gate',
                                'Prairie','Keystone','Liberty','Meridian','Horizon','Sterling','Coastal','Union','Allied','Cedar'])
                || ' ' || seed.pick(array['Logistics','Health','Systems','Foods','Manufacturing','Retail','Construction','Energy',
                                          'Partners','Solutions','Labs','Hospitality','Media','Dynamics','Industries','Capital',
                                          'Analytics','Motors','Pharma','Electric','Telecom','Supply','Holdings','Education'])
                || ' ' || CASE cty.code WHEN 'US' THEN seed.pick(array['Inc.','LLC','Corp.','Co.'])
                                       WHEN 'GB' THEN seed.pick(array['Ltd.','PLC'])
                                       WHEN 'DE' THEN 'GmbH' WHEN 'CH' THEN 'AG' WHEN 'NL' THEN 'B.V.'
                                       WHEN 'FR' THEN 'SAS' WHEN 'BR' THEN 'Ltda.' WHEN 'MX' THEN 'S.A. de C.V.'
                                       WHEN 'ES' THEN 'S.L.' WHEN 'IT' THEN 'S.p.A.' WHEN 'IN' THEN 'Pvt. Ltd.'
                                       WHEN 'JP' THEN 'K.K.' WHEN 'AU' THEN 'Pty Ltd' ELSE 'Ltd.' END
       END,
       nm.first, nm.last,
       lower(regexp_replace(nm.first, '[^A-Za-z]', '', 'g')) || seed.pick(array['.', '_', '', '.']) ||
           lower(regexp_replace(nm.last, '[^A-Za-z]', '', 'g')) ||
           CASE WHEN random() < 0.6 THEN seed.rint(1, 999)::text ELSE '' END || '@' ||
           CASE WHEN seg.segment = 'consumer'
                THEN seed.pick(array['gmail.example','outlook.example','yahoo.example','icloud.example','proton.example','mail.example'])
                ELSE 'corp' || g || '.example' END,
       c.phone_prefix || ' ' || seed.digits(3) || ' ' || seed.digits(3) || ' ' || seed.digits(4),
       cty.code, seed.pick(cc.a),
       CASE WHEN seg.segment IN ('mid_market', 'enterprise', 'government') THEN seed.pick(am.a) END,
       CASE seg.segment WHEN 'consumer' THEN NULL WHEN 'small_business' THEN 10000 WHEN 'mid_market' THEN 50000
                        WHEN 'enterprise' THEN 250000 ELSE 150000 END,
       CASE WHEN seg.segment = 'consumer' THEN floor(power(random(), 3) * 25000)::int ELSE 0 END,
       random() < 0.45,
       t.created_at, t.created_at + (timestamptz '2026-10-14' - t.created_at) * random()
FROM generate_series(1, 300000) g
CROSS JOIN seed.country_pool cp
CROSS JOIN seed.account_managers am
CROSS JOIN LATERAL (SELECT seed.pick(cp.a) AS code WHERE g > 0) cty
JOIN core.countries c ON c.code = cty.code
JOIN seed.country_cities cc ON cc.country_code = cty.code
CROSS JOIN LATERAL (SELECT ci.name FROM core.cities ci WHERE ci.country_code = cty.code ORDER BY random() + g * 0 LIMIT 1) ci
CROSS JOIN LATERAL (SELECT CASE WHEN r < 0.82 THEN 'consumer' WHEN r < 0.92 THEN 'small_business' WHEN r < 0.97 THEN 'mid_market'
                                WHEN r < 0.995 THEN 'enterprise' ELSE 'government' END::sales.customer_segment AS segment
                    FROM (SELECT random() + g * 0 AS r) x) seg
CROSS JOIN LATERAL (SELECT seed.pick((SELECT a FROM seed.first_names)) || CASE WHEN g > 0 THEN '' END AS first,
                           seed.pick((SELECT a FROM seed.last_names)) AS last) nm
CROSS JOIN LATERAL (SELECT seed.between('2012-01-01', '2026-10-10', 1.3) + g * interval '0 s' AS created_at) t;

INSERT INTO sales.customer_addresses (customer_id, kind, line1, line2, city_id, postal_code, is_default)
SELECT c.id, k.kind,
       seed.rint(1, 9999) || ' ' || seed.pick(array['Main','Oak','Maple','Park','Cedar','Elm','Lake','Hill','Washington','Market',
                                                      'High','Church','Station','King','Queen','River','Harbor','Sunset'])
           || ' ' || seed.pick(array['Street','Avenue','Road','Boulevard','Lane','Drive','Way']),
       CASE WHEN random() < 0.3 THEN seed.pick(array['Apt ', 'Suite ', 'Unit ', 'Floor ']) || seed.rint(1, 40) END,
       coalesce(c.city_id, cc.a[1]),
       CASE c.country_code WHEN 'US' THEN seed.digits(5) WHEN 'GB' THEN 'SW1A ' || seed.rint(1, 9) || 'AA'
                           WHEN 'CA' THEN 'M5V ' || seed.rint(1, 9) || 'K' || seed.rint(1, 9)
                           WHEN 'BR' THEN seed.digits(5) || '-' || seed.digits(3) WHEN 'JP' THEN seed.digits(3) || '-' || seed.digits(4)
                           WHEN 'NL' THEN seed.digits(4) || ' AB' ELSE seed.digits(5) END,
       k.n = 1
FROM sales.customers c
JOIN seed.country_cities cc ON cc.country_code = c.country_code
CROSS JOIN LATERAL (SELECT n, CASE WHEN n = 1 THEN 'shipping' WHEN n = 2 THEN 'billing' ELSE 'shipping' END AS kind
                    FROM generate_series(1, CASE WHEN random() < 0.6 THEN 1 WHEN random() < 0.8 THEN 2 ELSE 3 END + c.id * 0) n) k;

-- A promotion every month, plus the big yearly events.
INSERT INTO sales.promotions (code, description, discount_pct, starts_on, ends_on)
SELECT upper(to_char(m, 'MON')) || to_char(m, 'YY') || '-' || d.pct,
       to_char(m, 'FMMonth YYYY') || ' savings event', d.pct, m::date, (m + interval '1 month - 1 day')::date
FROM generate_series(timestamp '2019-01-01', timestamp '2026-10-01', interval '1 month') m
CROSS JOIN LATERAL (SELECT seed.pick(array[5, 10, 10, 15, 20]) + extract(month FROM m)::int * 0 AS pct) d;
INSERT INTO sales.promotions (code, description, discount_pct, starts_on, ends_on)
SELECT e.code || y, e.description || ' ' || y, e.pct, make_date(y, e.m, e.d), make_date(y, e.m, e.d) + e.days
FROM generate_series(2019, 2026) y
CROSS JOIN (VALUES ('BLACKFRIDAY', 'Black Friday', 30, 11, 24, 4), ('SUMMER', 'Summer sale', 20, 7, 1, 14),
                   ('BACK2SCHOOL', 'Back to school', 15, 8, 15, 21), ('NEWYEAR', 'New Year clearance', 25, 1, 2, 10)) e(code, description, pct, m, d, days)
WHERE make_date(y, e.m, e.d) <= date '2026-10-14';

-- ---------------------------------------------------------------- sales: orders

-- When orders happen: steady growth plus holiday peaks. Consumers order more often than
-- they're numerous; low customer ids are the most loyal.
CREATE TABLE seed.o (ordered_at timestamptz, customer_id int);
INSERT INTO seed.o
SELECT seed.between('2019-01-01', '2026-10-14 17:00', 1.25), 1 + floor(power(random(), 1.4) * 300000)::int
FROM generate_series(1, 1250000);
INSERT INTO seed.o
SELECT make_date(y, 11, 20) + random() * interval '38 days', 1 + floor(power(random(), 1.4) * 300000)::int
FROM generate_series(2019, 2025) y, generate_series(1, 9000 + (y - 2019) * 3500);
-- Nobody orders before signing up.
UPDATE seed.o o SET ordered_at = c.created_at + (timestamptz '2026-10-14 17:00' - c.created_at) * random()
FROM sales.customers c WHERE c.id = o.customer_id AND o.ordered_at < c.created_at;

CREATE TABLE seed.inside_sales AS
SELECT array_agg(e.id) AS a FROM hr.employees e JOIN hr.departments d ON d.id = e.department_id
WHERE d.code IN ('SALES-INS', 'SALES-MID', 'SALES-ENT');

-- Orders are staged with their ids (in date order) so the lines and totals can be computed
-- before anything is inserted.
CREATE TABLE seed.ord AS
SELECT row_number() OVER (ORDER BY o.ordered_at) AS id, o.customer_id, o.ordered_at, ch.id AS channel_id,
       CASE WHEN ch.id = 5 THEN coalesce(c.account_manager_id, seed.pick(isl.a)) WHEN c.segment <> 'consumer' THEN c.account_manager_id END AS sales_rep_id,
       CASE WHEN random() < 0.18 THEN (SELECT p.id FROM sales.promotions p
                                      WHERE o.ordered_at::date BETWEEN p.starts_on AND p.ends_on
                                      ORDER BY p.discount_pct DESC LIMIT 1) END AS promotion_id,
       CASE WHEN age < interval '2 days' THEN (array['pending','confirmed'])[seed.rint(1, 2)]
            WHEN age < interval '6 days' THEN (array['confirmed','shipped','shipped'])[seed.rint(1, 3)]
            WHEN age < interval '12 days' THEN (array['shipped','delivered','delivered'])[seed.rint(1, 3)]
            WHEN random() < 0.04 THEN 'cancelled'
            WHEN random() < 0.035 THEN 'returned'
            ELSE 'delivered'
       END::sales.order_status AS status,
       co.currency_code, co.vat_rate, c.segment,
       CASE WHEN random() < 0.04 THEN seed.pick(array['Leave at the front door','Gift — please include a receipt without prices',
                                                      'Call before delivery','Deliver to reception','Urgent: needed by Friday',
                                                      'PO number must appear on the invoice']) END AS notes
FROM seed.o o
JOIN sales.customers c ON c.id = o.customer_id
JOIN core.countries co ON co.code = c.country_code
CROSS JOIN seed.inside_sales isl
CROSS JOIN LATERAL (SELECT timestamptz '2026-10-14 18:00' - o.ordered_at AS age) a
CROSS JOIN LATERAL (
    SELECT CASE WHEN c.segment = 'consumer'
                THEN (array[1,1,1,1,1,1,1,1,1,2,2,2,2,2,2,3,3,3,4,4])[seed.rint(1, 20)]
                ELSE (array[5,5,5,5,6,6,6,6,6,1,1])[seed.rint(1, 11)] END + (o.customer_id * 0) AS id
) ch;

-- 1 to 8 lines per order (most have one or two); popular products sell the most.
CREATE TABLE seed.items AS
SELECT o.id AS order_id, l.n::smallint AS line_number, p.id AS product_id,
       CASE o.segment WHEN 'consumer' THEN (array[1,1,1,1,1,1,2,2,3])[seed.rint(1, 9)]
                      WHEN 'small_business' THEN seed.rint(1, 12) WHEN 'mid_market' THEN seed.rint(2, 30)
                      ELSE seed.rint(5, 120) END AS quantity,
       CASE WHEN cur.decimals = 0 THEN round(p.list_price * fx.usd_rate, 0) ELSE round(p.list_price * fx.usd_rate, 2) END AS unit_price,
       CASE o.segment WHEN 'enterprise' THEN seed.pick(array[5, 8, 10, 12, 15]) WHEN 'government' THEN 7
                      WHEN 'mid_market' THEN seed.pick(array[0, 3, 5]) ELSE 0 END::numeric AS discount_pct
FROM seed.ord o
JOIN core.currencies cur ON cur.code = o.currency_code
JOIN seed.fx fx ON fx.code = o.currency_code
CROSS JOIN LATERAL generate_series(1, (array[1,1,1,1,1,2,2,2,3,3,4,5,6,8])[seed.rint(1, 14) + (o.id * 0)::int]) l(n)
-- Cheap things sell far more often than expensive ones: the cheapest of a few popular candidates.
CROSS JOIN LATERAL (SELECT p.id, p.list_price
                    FROM (SELECT 1 + floor(power(random(), 2.2) * 30000)::int + l.n * 0 AS id FROM generate_series(1, 4) k) c
                    JOIN catalog.products p ON p.id = c.id
                    ORDER BY p.list_price LIMIT 1) p;

-- Totals: items, promotion, VAT of the customer's country, free shipping over a threshold.
INSERT INTO sales.orders (id, order_number, customer_id, channel_id, sales_rep_id, promotion_id, status, currency_code,
                          subtotal, discount, tax, shipping_fee, ordered_at, notes)
OVERRIDING SYSTEM VALUE
SELECT o.id, 'SO-' || to_char(o.ordered_at, 'YYYY') || '-' || lpad(o.id::text, 8, '0'),
       o.customer_id, o.channel_id, o.sales_rep_id, o.promotion_id, o.status, o.currency_code,
       s.subtotal,
       round(s.subtotal * coalesce(p.discount_pct, 0) / 100, cur.decimals),
       round(s.subtotal * (1 - coalesce(p.discount_pct, 0) / 100) * o.vat_rate / 100, cur.decimals),
       CASE WHEN s.subtotal / fx.usd_rate >= 50 OR o.channel_id = 3 THEN 0 ELSE round(7.99 * fx.usd_rate, cur.decimals) END,
       o.ordered_at, o.notes
FROM seed.ord o
JOIN (SELECT order_id, round(sum(quantity * unit_price * (1 - discount_pct / 100)), 2) AS subtotal
      FROM seed.items GROUP BY order_id) s ON s.order_id = o.id
JOIN core.currencies cur ON cur.code = o.currency_code
JOIN seed.fx fx ON fx.code = o.currency_code
LEFT JOIN sales.promotions p ON p.id = o.promotion_id
ORDER BY o.id;
SELECT setval(pg_get_serial_sequence('sales.orders', 'id'), (SELECT max(id) FROM sales.orders));

INSERT INTO sales.order_items (order_id, line_number, product_id, quantity, unit_price, discount_pct)
SELECT order_id, line_number, product_id, quantity, unit_price, discount_pct FROM seed.items ORDER BY order_id, line_number;

-- ---------------------------------------------------------------- sales: payments, shipments, returns

INSERT INTO sales.payments (order_id, method, amount, status, provider_ref, paid_at)
SELECT o.id, m.method, o.total,
       CASE o.status WHEN 'pending' THEN 'authorized' WHEN 'confirmed' THEN 'authorized'
                     WHEN 'cancelled' THEN 'refunded' ELSE 'captured' END,
       CASE m.method WHEN 'paypal' THEN 'PAYID-' WHEN 'bank_transfer' THEN 'WIRE-' WHEN 'invoice' THEN 'INV-' ELSE 'ch_' END
           || substr(md5(o.id::text || 'pay'), 1, 20),
       o.ordered_at + seed.rint(0, 300) * interval '1 second'
FROM sales.orders o
JOIN sales.customers c ON c.id = o.customer_id
CROSS JOIN LATERAL (
    SELECT CASE WHEN c.segment IN ('enterprise', 'government', 'mid_market') AND random() < 0.8
                THEN (array['invoice','invoice','bank_transfer'])[seed.rint(1, 3)]
                ELSE (array['credit_card','credit_card','credit_card','credit_card','debit_card','debit_card','paypal','paypal',
                            'gift_card'])[seed.rint(1, 9)] END::sales.payment_method AS method
    WHERE o.id > 0
) m
WHERE NOT (o.status = 'cancelled' AND random() < 0.5);
-- Some cards are declined first.
INSERT INTO sales.payments (order_id, method, amount, status, provider_ref, paid_at)
SELECT p.order_id, p.method, p.amount, 'failed', 'ch_' || substr(md5(p.id::text || 'fail'), 1, 20), p.paid_at - interval '2 minutes'
FROM sales.payments p
WHERE p.method IN ('credit_card', 'debit_card') AND random() < 0.03;

INSERT INTO sales.shipments (order_id, warehouse_id, carrier, tracking_number, shipped_at, delivered_at)
SELECT o.id, seed.pick(rw.a),
       CASE co.region WHEN 'North America' THEN seed.pick(array['UPS','FedEx','USPS','Canada Post'])
                      WHEN 'Latin America' THEN seed.pick(array['DHL','Correios','Estafeta','FedEx'])
                      WHEN 'Europe' THEN seed.pick(array['DHL','DPD','GLS','Royal Mail','PostNL'])
                      WHEN 'Asia Pacific' THEN seed.pick(array['DHL','Yamato','SF Express','Australia Post','Blue Dart'])
                      ELSE seed.pick(array['Aramex','DHL','The Courier Guy']) END,
       upper(substr(md5(o.id::text || 'ship'), 1, 18)),
       s.shipped_at,
       CASE WHEN o.status IN ('delivered', 'returned') THEN s.shipped_at + seed.rint(18, 170) * interval '1 hour' END
FROM sales.orders o
JOIN sales.customers c ON c.id = o.customer_id
JOIN core.countries co ON co.code = c.country_code
JOIN seed.region_wh rw ON rw.region = co.region
CROSS JOIN LATERAL (SELECT o.ordered_at + seed.rint(4, 60) * interval '1 hour' AS shipped_at) s
WHERE o.status IN ('shipped', 'delivered', 'returned');

INSERT INTO sales.returns (order_id, reason, refund_amount, requested_at, approved)
SELECT o.id,
       seed.pick(array['Damaged in transit','Wrong item received','No longer needed','Item not as described',
                       'Arrived too late','Better price elsewhere','Defective','Wrong size']),
       round(o.total * (array[1, 1, 1, 0.5, 0.75])[seed.rint(1, 5)], 2),
       sh.delivered_at + seed.rint(1, 25) * interval '1 day',
       (array[true, true, true, true, true, true, true, true, false, NULL])[seed.rint(1, 10)]
FROM sales.orders o JOIN sales.shipments sh ON sh.order_id = o.id
WHERE o.status = 'returned';
