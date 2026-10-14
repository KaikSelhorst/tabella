-- Functions, procedures, triggers and views for the "dev-mysql" connection (database `loja`),
-- on top of dev/mysql.sql. Rerunnable. MySQL has no sequences.
--
--   docker exec -i dblite-dev-mysql-1 mysql -uroot -pmysql loja < dev/mysql-objects.sql

-- Functions need this when binary logging is on (MySQL 8's default) unless they are
-- declared DETERMINISTIC / READS SQL DATA, which these are; kept off for the session anyway,
-- like the seed.
SET SESSION sql_log_bin = 0;

DROP FUNCTION IF EXISTS preco_com_desconto;
DROP FUNCTION IF EXISTS faixa_de_preco;
DROP PROCEDURE IF EXISTS repor_estoque;
DROP PROCEDURE IF EXISTS vendas_do_mes;
DROP TRIGGER IF EXISTS vendas_baixa_estoque;
DROP TRIGGER IF EXISTS produtos_valida_preco;

DELIMITER //

-- Functions --------------------------------------------------------------------------------

CREATE FUNCTION preco_com_desconto(preco DECIMAL(10, 2), pct INT) RETURNS DECIMAL(10, 2)
DETERMINISTIC
BEGIN
  IF pct < 0 OR pct > 90 THEN
    SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'desconto entre 0 e 90%';
  END IF;
  RETURN ROUND(preco * (100 - pct) / 100, 2);
END //

CREATE FUNCTION faixa_de_preco(preco DECIMAL(10, 2)) RETURNS VARCHAR(10)
DETERMINISTIC
RETURN CASE
  WHEN preco < 100 THEN 'barato'
  WHEN preco < 1000 THEN 'médio'
  ELSE 'caro'
END //

-- Procedures -------------------------------------------------------------------------------

CREATE PROCEDURE repor_estoque(IN pid INT, IN qtd INT)
BEGIN
  UPDATE produtos SET estoque = estoque + qtd, em_estoque = TRUE WHERE id = pid;
  SELECT id, nome, estoque FROM produtos WHERE id = pid;
END //

CREATE PROCEDURE vendas_do_mes(IN ano INT, IN mes INT)
READS SQL DATA
BEGIN
  SELECT canal, COUNT(*) AS vendas, ROUND(SUM(valor), 2) AS total
  FROM vendas
  WHERE YEAR(vendido_em) = ano AND MONTH(vendido_em) = mes
  GROUP BY canal;
END //

-- Triggers ---------------------------------------------------------------------------------

-- A sale takes its quantity out of stock.
CREATE TRIGGER vendas_baixa_estoque AFTER INSERT ON vendas
FOR EACH ROW
BEGIN
  UPDATE produtos SET estoque = GREATEST(estoque - NEW.quantidade, 0) WHERE id = NEW.produto_id;
END //

-- No negative prices.
CREATE TRIGGER produtos_valida_preco BEFORE UPDATE ON produtos
FOR EACH ROW
BEGIN
  IF NEW.preco < 0 THEN
    SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'preço não pode ser negativo';
  END IF;
END //

DELIMITER ;

-- Views ------------------------------------------------------------------------------------

CREATE OR REPLACE VIEW produtos_em_falta AS
  SELECT id, sku, nome, categoria, estoque
  FROM produtos
  WHERE NOT em_estoque OR estoque = 0;

CREATE OR REPLACE VIEW vendas_por_canal AS
  SELECT canal, COUNT(*) AS vendas, ROUND(SUM(valor), 2) AS total
  FROM vendas
  GROUP BY canal;
