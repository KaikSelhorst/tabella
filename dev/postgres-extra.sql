-- A second schema and a second database for trying "trocar banco/schema" (on top of
-- dev/postgres.sql). Names repeat on purpose: `clientes` exists in public and in financeiro
-- with different rows, so an unqualified SELECT shows which one the editor is pointing at.
-- Rerunnable.
--
--   docker exec -i dblite-dev-pg-1 psql -U postgres < dev/postgres-extra.sql

SET client_min_messages = warning;

-- Schema financeiro (banco postgres) -------------------------------------------------------

CREATE SCHEMA IF NOT EXISTS financeiro;
DROP TABLE IF EXISTS financeiro.lancamentos, financeiro.contas, financeiro.clientes;

-- Same name as public.clientes, other columns and only 5 rows.
CREATE TABLE financeiro.clientes (
  id int PRIMARY KEY,
  razao_social text NOT NULL,
  cnpj text NOT NULL,
  limite_credito numeric(12, 2) NOT NULL
);
INSERT INTO financeiro.clientes VALUES
  (1, 'Padaria Pão Quente Ltda', '12.345.678/0001-90', 15000),
  (2, 'Mercado Bom Preço S/A', '23.456.789/0001-01', 80000),
  (3, 'Oficina do Zé ME', '34.567.890/0001-12', 5000),
  (4, 'Clínica Sorriso Ltda', '45.678.901/0001-23', 30000),
  (5, 'Transportes Rápido Ltda', '56.789.012/0001-34', 120000);

CREATE TABLE financeiro.contas (
  id serial PRIMARY KEY,
  nome text NOT NULL,
  banco text NOT NULL,
  saldo numeric(12, 2) NOT NULL
);
INSERT INTO financeiro.contas (nome, banco, saldo) VALUES
  ('Conta movimento', 'Banco do Brasil', 152340.55),
  ('Aplicação CDB', 'Itaú', 500000.00),
  ('Caixa pequeno', 'Interno', 1200.00);

CREATE TABLE financeiro.lancamentos (
  id serial PRIMARY KEY,
  conta_id int NOT NULL REFERENCES financeiro.contas (id),
  cliente_id int REFERENCES financeiro.clientes (id),
  tipo text NOT NULL CHECK (tipo IN ('receita', 'despesa')),
  descricao text NOT NULL,
  valor numeric(12, 2) NOT NULL,
  vencimento date NOT NULL,
  pago boolean NOT NULL DEFAULT false
);
INSERT INTO financeiro.lancamentos (conta_id, cliente_id, tipo, descricao, valor, vencimento, pago)
SELECT 1 + g % 3,
       CASE WHEN g % 2 = 0 THEN 1 + g % 5 END,
       CASE WHEN g % 2 = 0 THEN 'receita' ELSE 'despesa' END,
       CASE WHEN g % 2 = 0 THEN 'Fatura ' || g ELSE (ARRAY['Aluguel', 'Energia', 'Folha', 'Impostos'])[1 + g % 4] END,
       round((100 + g * 37.5 % 9000)::numeric, 2),
       date '2026-01-01' + g * 3,
       g < 70
FROM generate_series(1, 100) g;

-- Banco rh (outro banco no mesmo servidor) -------------------------------------------------

SELECT 'CREATE DATABASE rh' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'rh')\gexec
\connect rh
SET client_min_messages = warning;

DROP TABLE IF EXISTS ferias, funcionarios, departamentos, clientes;

CREATE TABLE departamentos (
  id int PRIMARY KEY,
  nome text NOT NULL,
  centro_custo text NOT NULL
);
INSERT INTO departamentos VALUES
  (1, 'Tecnologia', 'CC-100'),
  (2, 'Comercial', 'CC-200'),
  (3, 'Financeiro', 'CC-300'),
  (4, 'Logística', 'CC-400');

CREATE TABLE funcionarios (
  id serial PRIMARY KEY,
  nome text NOT NULL,
  cargo text NOT NULL,
  departamento_id int NOT NULL REFERENCES departamentos (id),
  salario numeric(10, 2) NOT NULL,
  admitido_em date NOT NULL
);
INSERT INTO funcionarios (nome, cargo, departamento_id, salario, admitido_em)
SELECT (ARRAY['Ana', 'Bruno', 'Carla', 'Diego', 'Elisa', 'Fábio', 'Gabi', 'Hugo'])[1 + g % 8] || ' ' ||
       (ARRAY['Silva', 'Souza', 'Lima', 'Costa', 'Rocha'])[1 + g % 5],
       (ARRAY['Analista', 'Desenvolvedor', 'Vendedor', 'Coordenador', 'Assistente'])[1 + g % 5],
       1 + g % 4,
       3000 + (g * 731) % 12000,
       date '2018-01-01' + g * 23
FROM generate_series(1, 40) g;

CREATE TABLE ferias (
  id serial PRIMARY KEY,
  funcionario_id int NOT NULL REFERENCES funcionarios (id),
  inicio date NOT NULL,
  dias int NOT NULL CHECK (dias BETWEEN 5 AND 30)
);
INSERT INTO ferias (funcionario_id, inicio, dias)
SELECT 1 + g % 40, date '2026-01-05' + g * 9, (ARRAY[10, 15, 20, 30])[1 + g % 4]
FROM generate_series(1, 25) g;

-- Also a `clientes` here: the third different one (only 2 rows).
CREATE TABLE clientes (id int PRIMARY KEY, nome text NOT NULL, observacao text);
INSERT INTO clientes VALUES
  (1, 'Cliente do banco rh', 'esta tabela fica no banco rh'),
  (2, 'Outro do rh', 'se você vê isto, a aba está no banco rh');
