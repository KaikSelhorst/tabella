-- A second database for trying "trocar banco" on the "dev-mysql" connection (on top of
-- dev/mysql.sql). `produtos` exists in loja and in rh with different rows, so an unqualified
-- SELECT shows which database the editor is pointing at. Rerunnable.
--
--   docker exec -i dblite-dev-mysql-1 mysql -uroot -pmysql < dev/mysql-extra.sql

CREATE DATABASE IF NOT EXISTS rh CHARACTER SET utf8mb4;
USE rh;

DROP TABLE IF EXISTS ferias, funcionarios, departamentos, produtos;

CREATE TABLE departamentos (
  id INT PRIMARY KEY,
  nome VARCHAR(60) NOT NULL,
  centro_custo VARCHAR(10) NOT NULL
);
INSERT INTO departamentos VALUES
  (1, 'Tecnologia', 'CC-100'),
  (2, 'Comercial', 'CC-200'),
  (3, 'Financeiro', 'CC-300'),
  (4, 'Logística', 'CC-400');

CREATE TABLE funcionarios (
  id INT AUTO_INCREMENT PRIMARY KEY,
  nome VARCHAR(80) NOT NULL,
  cargo VARCHAR(40) NOT NULL,
  departamento_id INT NOT NULL,
  salario DECIMAL(10, 2) NOT NULL,
  admitido_em DATE NOT NULL,
  FOREIGN KEY (departamento_id) REFERENCES departamentos (id)
);
INSERT INTO funcionarios (nome, cargo, departamento_id, salario, admitido_em)
WITH RECURSIVE s (g) AS (SELECT 1 UNION ALL SELECT g + 1 FROM s WHERE g < 40)
SELECT CONCAT(ELT(1 + g % 8, 'Ana', 'Bruno', 'Carla', 'Diego', 'Elisa', 'Fábio', 'Gabi', 'Hugo'), ' ',
              ELT(1 + g % 5, 'Silva', 'Souza', 'Lima', 'Costa', 'Rocha')),
       ELT(1 + g % 5, 'Analista', 'Desenvolvedor', 'Vendedor', 'Coordenador', 'Assistente'),
       1 + g % 4,
       3000 + (g * 731) % 12000,
       DATE_ADD('2018-01-01', INTERVAL g * 23 DAY)
FROM s;

CREATE TABLE ferias (
  id INT AUTO_INCREMENT PRIMARY KEY,
  funcionario_id INT NOT NULL,
  inicio DATE NOT NULL,
  dias INT NOT NULL,
  FOREIGN KEY (funcionario_id) REFERENCES funcionarios (id)
);
INSERT INTO ferias (funcionario_id, inicio, dias)
WITH RECURSIVE s (g) AS (SELECT 1 UNION ALL SELECT g + 1 FROM s WHERE g < 25)
SELECT 1 + g % 40, DATE_ADD('2026-01-05', INTERVAL g * 9 DAY), ELT(1 + g % 4, 10, 15, 20, 30)
FROM s;

-- Same name as loja.produtos, only 3 rows.
CREATE TABLE produtos (id INT PRIMARY KEY, nome VARCHAR(80) NOT NULL, observacao VARCHAR(120));
INSERT INTO produtos VALUES
  (1, 'Uniforme', 'esta tabela fica no banco rh'),
  (2, 'Crachá', 'se você vê isto, a aba está no banco rh'),
  (3, 'Kit boas-vindas', NULL);
