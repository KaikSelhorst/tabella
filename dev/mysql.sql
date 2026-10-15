-- Sample data for the "dev-mysql" connection (MySQL on localhost:3307, database `shop`). Heavy on
-- purpose, like dev/postgres.sql. Rerunnable: drops and recreates what it owns.
--
--   docker exec -i dblite-mysql mysql -uroot -pmysql shop < dev/mysql.sql      (~2-4 min)

SET SESSION sql_log_bin = 0;
DROP TABLE IF EXISTS sales, logs, products, digits, seq;
DROP DATABASE IF EXISTS archive;

-- 0..9, crossed with itself to make number sequences quickly (no generate_series in MySQL).
CREATE TABLE digits (d TINYINT PRIMARY KEY);
INSERT INTO digits VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9);
CREATE TABLE seq (n INT PRIMARY KEY);
INSERT INTO seq
SELECT 1 + a.d + 10 * b.d + 100 * c.d + 1000 * d.d + 10000 * e.d + 100000 * f.d
FROM digits a, digits b, digits c, digits d, digits e, digits f;

-- 1 000 000 products.
CREATE TABLE products (
  id INT AUTO_INCREMENT PRIMARY KEY,
  sku VARCHAR(20) NOT NULL,
  name VARCHAR(100) NOT NULL,
  category VARCHAR(40),
  price DECIMAL(10, 2) NOT NULL,
  stock INT NOT NULL DEFAULT 0,
  in_stock BOOLEAN NOT NULL DEFAULT TRUE,
  featured BOOLEAN NULL,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO products (sku, name, category, price, stock, in_stock, featured, created_at)
SELECT CONCAT('SKU-', LPAD(n, 7, '0')),
       CONCAT('Product ', n),
       ELT(1 + n % 8, 'Electronics', 'Books', 'Home', 'Sports', 'Fashion', 'Toys', 'Beauty', 'Grocery'),
       ROUND(5 + RAND() * 4995, 2),
       n % 500,
       n % 4 <> 0,
       IF(n % 5 = 0, NULL, n % 2 = 0),
       NOW() - INTERVAL FLOOR(RAND() * 1500) DAY
FROM seq;

-- 3 000 000 sales: three passes over the 1M sequence.
CREATE TABLE sales (
  id BIGINT AUTO_INCREMENT PRIMARY KEY,
  product_id INT NOT NULL,
  quantity SMALLINT NOT NULL,
  amount DECIMAL(12, 2) NOT NULL,
  channel VARCHAR(10) NOT NULL,
  sold_at DATETIME NOT NULL,
  KEY sales_product_idx (product_id),
  KEY sales_date_idx (sold_at),
  CONSTRAINT sales_product_fk FOREIGN KEY (product_id) REFERENCES products (id)
);
INSERT INTO sales (product_id, quantity, amount, channel, sold_at)
SELECT 1 + (s.n * 7919 + p.d) % 1000000,
       1 + (s.n + p.d) % 5,
       ROUND(10 + RAND() * 990, 2),
       ELT(1 + (s.n + p.d) % 3, 'store', 'web', 'app'),
       NOW() - INTERVAL FLOOR(RAND() * 1000) DAY
FROM seq s
JOIN digits p ON p.d < 3;

-- 200 000 wide rows with JSON, long text and NULLs.
CREATE TABLE logs (
  id INT AUTO_INCREMENT PRIMARY KEY,
  level ENUM('debug', 'info', 'warning', 'error') NOT NULL,
  origin VARCHAR(40),
  message TEXT,
  context JSON,
  user_id INT NULL,
  duration_ms INT NULL,
  created_at DATETIME(3) NOT NULL
);
INSERT INTO logs (level, origin, message, context, user_id, duration_ms, created_at)
SELECT ELT(1 + n % 4, 'debug', 'info', 'warning', 'error'),
       ELT(1 + n % 5, 'api', 'worker', 'checkout', 'auth', 'search'),
       IF(n % 3 = 0, REPEAT(CONCAT('Log message ', n, '. '), 1 + n % 6), CONCAT('ok ', n)),
       JSON_OBJECT('req', CONCAT('r-', n), 'status', ELT(1 + n % 4, 200, 201, 404, 500), 'attempts', n % 4),
       IF(n % 7 = 0, NULL, 1 + n % 50000),
       IF(n % 9 = 0, NULL, FLOOR(RAND() * 3000)),
       NOW(3) - INTERVAL FLOOR(RAND() * 90 * 86400) SECOND
FROM seq
WHERE n <= 200000;

-- 150 small tables in a separate database, for the sidebar and Ctrl+P.
CREATE DATABASE archive;
DROP PROCEDURE IF EXISTS create_archive;
DELIMITER //
CREATE PROCEDURE create_archive()
BEGIN
  DECLARE i INT DEFAULT 1;
  WHILE i <= 150 DO
    SET @t = CONCAT('archive.record_', LPAD(i, 3, '0'));
    SET @ddl = CONCAT('CREATE TABLE ', @t, ' (id INT AUTO_INCREMENT PRIMARY KEY, ref VARCHAR(20), amount DECIMAL(10, 2), created_at DATE)');
    PREPARE s FROM @ddl; EXECUTE s; DEALLOCATE PREPARE s;
    SET @ins = CONCAT('INSERT INTO ', @t, ' (ref, amount, created_at) SELECT CONCAT(''R'', n), n * 1.5, CURDATE() - INTERVAL n DAY FROM shop.seq WHERE n <= 100');
    PREPARE s FROM @ins; EXECUTE s; DEALLOCATE PREPARE s;
    SET i = i + 1;
  END WHILE;
END //
DELIMITER ;
CALL create_archive();
DROP PROCEDURE create_archive;

DROP TABLE digits, seq;
ANALYZE TABLE products, sales, logs;
SELECT 'products' AS table_name, COUNT(*) AS row_count FROM products
UNION ALL SELECT 'sales', COUNT(*) FROM sales
UNION ALL SELECT 'logs', COUNT(*) FROM logs;
