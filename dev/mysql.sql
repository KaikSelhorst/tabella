-- Sample data for the "dev-mysql" connection (MySQL on localhost:3307, database `loja`).
CREATE TABLE produtos (
  id INT AUTO_INCREMENT PRIMARY KEY,
  nome VARCHAR(100) NOT NULL,
  preco DECIMAL(10, 2) NOT NULL,
  em_estoque BOOLEAN NOT NULL DEFAULT TRUE,
  destaque BOOLEAN NULL,
  criado_em DATETIME DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO produtos (nome, preco, em_estoque, destaque)
WITH RECURSIVE seq (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 300)
SELECT CONCAT('Produto ', n), ROUND(n * 3.7 + 9.9, 2), n % 4 <> 0, IF(n % 5 = 0, NULL, n % 2 = 0) FROM seq;
