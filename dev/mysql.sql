-- Sample data for the "dev-mysql" connection (MySQL on localhost:3307, database `loja`). Heavy on
-- purpose, like dev/postgres.sql. Rerunnable: drops and recreates what it owns.
--
--   docker exec -i dblite-mysql mysql -uroot -pmysql loja < dev/mysql.sql      (~2-4 min)

SET SESSION sql_log_bin = 0;
DROP TABLE IF EXISTS vendas, logs, produtos, digitos, seq;
DROP DATABASE IF EXISTS arquivo;

-- 0..9, crossed with itself to make number sequences quickly (no generate_series in MySQL).
CREATE TABLE digitos (d TINYINT PRIMARY KEY);
INSERT INTO digitos VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9);
CREATE TABLE seq (n INT PRIMARY KEY);
INSERT INTO seq
SELECT 1 + a.d + 10 * b.d + 100 * c.d + 1000 * d.d + 10000 * e.d + 100000 * f.d
FROM digitos a, digitos b, digitos c, digitos d, digitos e, digitos f;

-- 1 000 000 products.
CREATE TABLE produtos (
  id INT AUTO_INCREMENT PRIMARY KEY,
  sku VARCHAR(20) NOT NULL,
  nome VARCHAR(100) NOT NULL,
  categoria VARCHAR(40),
  preco DECIMAL(10, 2) NOT NULL,
  estoque INT NOT NULL DEFAULT 0,
  em_estoque BOOLEAN NOT NULL DEFAULT TRUE,
  destaque BOOLEAN NULL,
  criado_em DATETIME DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO produtos (sku, nome, categoria, preco, estoque, em_estoque, destaque, criado_em)
SELECT CONCAT('SKU-', LPAD(n, 7, '0')),
       CONCAT('Produto ', n),
       ELT(1 + n % 8, 'Eletrônicos', 'Livros', 'Casa', 'Esporte', 'Moda', 'Brinquedos', 'Beleza', 'Mercado'),
       ROUND(5 + RAND() * 4995, 2),
       n % 500,
       n % 4 <> 0,
       IF(n % 5 = 0, NULL, n % 2 = 0),
       NOW() - INTERVAL FLOOR(RAND() * 1500) DAY
FROM seq;

-- 3 000 000 sales: three passes over the 1M sequence.
CREATE TABLE vendas (
  id BIGINT AUTO_INCREMENT PRIMARY KEY,
  produto_id INT NOT NULL,
  quantidade SMALLINT NOT NULL,
  valor DECIMAL(12, 2) NOT NULL,
  canal VARCHAR(10) NOT NULL,
  vendido_em DATETIME NOT NULL,
  KEY vendas_produto_idx (produto_id),
  KEY vendas_data_idx (vendido_em),
  CONSTRAINT vendas_produto_fk FOREIGN KEY (produto_id) REFERENCES produtos (id)
);
INSERT INTO vendas (produto_id, quantidade, valor, canal, vendido_em)
SELECT 1 + (s.n * 7919 + p.d) % 1000000,
       1 + (s.n + p.d) % 5,
       ROUND(10 + RAND() * 990, 2),
       ELT(1 + (s.n + p.d) % 3, 'loja', 'site', 'app'),
       NOW() - INTERVAL FLOOR(RAND() * 1000) DAY
FROM seq s
JOIN digitos p ON p.d < 3;

-- 200 000 wide rows with JSON, long text and NULLs.
CREATE TABLE logs (
  id INT AUTO_INCREMENT PRIMARY KEY,
  nivel ENUM('debug', 'info', 'aviso', 'erro') NOT NULL,
  origem VARCHAR(40),
  mensagem TEXT,
  contexto JSON,
  usuario_id INT NULL,
  duracao_ms INT NULL,
  criado_em DATETIME(3) NOT NULL
);
INSERT INTO logs (nivel, origem, mensagem, contexto, usuario_id, duracao_ms, criado_em)
SELECT ELT(1 + n % 4, 'debug', 'info', 'aviso', 'erro'),
       ELT(1 + n % 5, 'api', 'worker', 'checkout', 'auth', 'busca'),
       IF(n % 3 = 0, REPEAT(CONCAT('Mensagem de log ', n, '. '), 1 + n % 6), CONCAT('ok ', n)),
       JSON_OBJECT('req', CONCAT('r-', n), 'status', ELT(1 + n % 4, 200, 201, 404, 500), 'tentativas', n % 4),
       IF(n % 7 = 0, NULL, 1 + n % 50000),
       IF(n % 9 = 0, NULL, FLOOR(RAND() * 3000)),
       NOW(3) - INTERVAL FLOOR(RAND() * 90 * 86400) SECOND
FROM seq
WHERE n <= 200000;

-- 150 small tables in a separate database, for the sidebar and Ctrl+P.
CREATE DATABASE arquivo;
DROP PROCEDURE IF EXISTS cria_arquivo;
DELIMITER //
CREATE PROCEDURE cria_arquivo()
BEGIN
  DECLARE i INT DEFAULT 1;
  WHILE i <= 150 DO
    SET @t = CONCAT('arquivo.registro_', LPAD(i, 3, '0'));
    SET @ddl = CONCAT('CREATE TABLE ', @t, ' (id INT AUTO_INCREMENT PRIMARY KEY, ref VARCHAR(20), valor DECIMAL(10, 2), criado_em DATE)');
    PREPARE s FROM @ddl; EXECUTE s; DEALLOCATE PREPARE s;
    SET @ins = CONCAT('INSERT INTO ', @t, ' (ref, valor, criado_em) SELECT CONCAT(''R'', n), n * 1.5, CURDATE() - INTERVAL n DAY FROM loja.seq WHERE n <= 100');
    PREPARE s FROM @ins; EXECUTE s; DEALLOCATE PREPARE s;
    SET i = i + 1;
  END WHILE;
END //
DELIMITER ;
CALL cria_arquivo();
DROP PROCEDURE cria_arquivo;

DROP TABLE digitos, seq;
ANALYZE TABLE produtos, vendas, logs;
SELECT 'produtos' AS tabela, COUNT(*) AS linhas FROM produtos
UNION ALL SELECT 'vendas', COUNT(*) FROM vendas
UNION ALL SELECT 'logs', COUNT(*) FROM logs;
