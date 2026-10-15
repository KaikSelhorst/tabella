-- A second database for trying "switch database" on the "dev-mysql" connection (on top of
-- dev/mysql.sql). `products` exists in shop and in hr with different rows, so an unqualified
-- SELECT shows which database the editor is pointing at. Rerunnable.
--
--   docker exec -i dblite-dev-mysql-1 mysql -uroot -pmysql < dev/mysql-extra.sql

CREATE DATABASE IF NOT EXISTS hr CHARACTER SET utf8mb4;
USE hr;

DROP TABLE IF EXISTS vacations, employees, departments, products;

CREATE TABLE departments (
  id INT PRIMARY KEY,
  name VARCHAR(60) NOT NULL,
  cost_center VARCHAR(10) NOT NULL
);
INSERT INTO departments VALUES
  (1, 'Technology', 'CC-100'),
  (2, 'Sales', 'CC-200'),
  (3, 'Finance', 'CC-300'),
  (4, 'Logistics', 'CC-400');

CREATE TABLE employees (
  id INT AUTO_INCREMENT PRIMARY KEY,
  name VARCHAR(80) NOT NULL,
  role VARCHAR(40) NOT NULL,
  department_id INT NOT NULL,
  salary DECIMAL(10, 2) NOT NULL,
  hired_at DATE NOT NULL,
  FOREIGN KEY (department_id) REFERENCES departments (id)
);
INSERT INTO employees (name, role, department_id, salary, hired_at)
WITH RECURSIVE s (g) AS (SELECT 1 UNION ALL SELECT g + 1 FROM s WHERE g < 40)
SELECT CONCAT(ELT(1 + g % 8, 'Anna', 'Brian', 'Carol', 'David', 'Emily', 'Frank', 'Grace', 'Henry'), ' ',
              ELT(1 + g % 5, 'Smith', 'Johnson', 'Brown', 'Miller', 'Davis')),
       ELT(1 + g % 5, 'Analyst', 'Developer', 'Salesperson', 'Coordinator', 'Assistant'),
       1 + g % 4,
       3000 + (g * 731) % 12000,
       DATE_ADD('2018-01-01', INTERVAL g * 23 DAY)
FROM s;

CREATE TABLE vacations (
  id INT AUTO_INCREMENT PRIMARY KEY,
  employee_id INT NOT NULL,
  starts_on DATE NOT NULL,
  days INT NOT NULL,
  FOREIGN KEY (employee_id) REFERENCES employees (id)
);
INSERT INTO vacations (employee_id, starts_on, days)
WITH RECURSIVE s (g) AS (SELECT 1 UNION ALL SELECT g + 1 FROM s WHERE g < 25)
SELECT 1 + g % 40, DATE_ADD('2026-01-05', INTERVAL g * 9 DAY), ELT(1 + g % 4, 10, 15, 20, 30)
FROM s;

-- Same name as shop.products, only 3 rows.
CREATE TABLE products (id INT PRIMARY KEY, name VARCHAR(80) NOT NULL, note VARCHAR(120));
INSERT INTO products VALUES
  (1, 'Uniform', 'this table lives in the hr database'),
  (2, 'Badge', 'if you see this, the tab is on the hr database'),
  (3, 'Welcome kit', NULL);
