-- Functions, procedures, triggers and views for the "dev-mysql" connection (database `shop`),
-- on top of dev/mysql.sql. Rerunnable. MySQL has no sequences.
--
--   docker compose exec -T dev-mysql mysql -uroot -pmysql shop < dev/mysql-objects.sql

-- Functions need this when binary logging is on (MySQL 8's default) unless they are
-- declared DETERMINISTIC / READS SQL DATA, which these are; kept off for the session anyway,
-- like the seed.
SET SESSION sql_log_bin = 0;

DROP FUNCTION IF EXISTS discounted_price;
DROP FUNCTION IF EXISTS price_band;
DROP PROCEDURE IF EXISTS restock;
DROP PROCEDURE IF EXISTS monthly_sales;
DROP TRIGGER IF EXISTS sales_reduce_stock;
DROP TRIGGER IF EXISTS products_check_price;

DELIMITER //

-- Functions --------------------------------------------------------------------------------

CREATE FUNCTION discounted_price(price DECIMAL(10, 2), pct INT) RETURNS DECIMAL(10, 2)
DETERMINISTIC
BEGIN
  IF pct < 0 OR pct > 90 THEN
    SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'discount must be between 0 and 90%';
  END IF;
  RETURN ROUND(price * (100 - pct) / 100, 2);
END //

CREATE FUNCTION price_band(price DECIMAL(10, 2)) RETURNS VARCHAR(10)
DETERMINISTIC
RETURN CASE
  WHEN price < 100 THEN 'cheap'
  WHEN price < 1000 THEN 'mid'
  ELSE 'expensive'
END //

-- Procedures -------------------------------------------------------------------------------

CREATE PROCEDURE restock(IN pid INT, IN qty INT)
BEGIN
  UPDATE products SET stock = stock + qty, in_stock = TRUE WHERE id = pid;
  SELECT id, name, stock FROM products WHERE id = pid;
END //

CREATE PROCEDURE monthly_sales(IN yr INT, IN mon INT)
READS SQL DATA
BEGIN
  SELECT channel, COUNT(*) AS sales, ROUND(SUM(amount), 2) AS total
  FROM sales
  WHERE YEAR(sold_at) = yr AND MONTH(sold_at) = mon
  GROUP BY channel;
END //

-- Triggers ---------------------------------------------------------------------------------

-- A sale takes its quantity out of stock.
CREATE TRIGGER sales_reduce_stock AFTER INSERT ON sales
FOR EACH ROW
BEGIN
  UPDATE products SET stock = GREATEST(stock - NEW.quantity, 0) WHERE id = NEW.product_id;
END //

-- No negative prices.
CREATE TRIGGER products_check_price BEFORE UPDATE ON products
FOR EACH ROW
BEGIN
  IF NEW.price < 0 THEN
    SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'price can''t be negative';
  END IF;
END //

DELIMITER ;

-- Views ------------------------------------------------------------------------------------

CREATE OR REPLACE VIEW products_out_of_stock AS
  SELECT id, sku, name, category, stock
  FROM products
  WHERE NOT in_stock OR stock = 0;

CREATE OR REPLACE VIEW sales_by_channel AS
  SELECT channel, COUNT(*) AS sales, ROUND(SUM(amount), 2) AS total
  FROM sales
  GROUP BY channel;
