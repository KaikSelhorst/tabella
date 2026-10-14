-- Sample data for the "dev" connection (Postgres on localhost:5432). Heavy on purpose: big
-- tables for paging, counts and exports, a wide table with many types, and lots of small
-- tables for the sidebar. Rerunnable: it drops and recreates everything it owns.
--
--   docker exec -i dblite-pg psql -U postgres < dev/postgres.sql      (~1-2 min)

\timing on
SET client_min_messages = warning;

DROP TABLE IF EXISTS pedidos, eventos, clientes CASCADE;
DROP SCHEMA IF EXISTS arquivo CASCADE;

-- 1 000 000 customers.
CREATE TABLE clientes (
  id serial PRIMARY KEY,
  nome text NOT NULL,
  email text,
  cidade text,
  uf char(2),
  ativo boolean DEFAULT true,
  saldo numeric(12, 2),
  criado_em timestamp DEFAULT now()
);
INSERT INTO clientes (nome, email, cidade, uf, ativo, saldo, criado_em)
SELECT 'Cliente ' || g,
       'cliente' || g || '@exemplo.com',
       (ARRAY['São Paulo', 'Rio de Janeiro', 'Belo Horizonte', 'Curitiba', 'Porto Alegre', 'Recife', 'Salvador', 'Fortaleza', 'Manaus', 'Goiânia'])[1 + g % 10],
       (ARRAY['SP', 'RJ', 'MG', 'PR', 'RS', 'PE', 'BA', 'CE', 'AM', 'GO'])[1 + g % 10],
       CASE WHEN g % 7 = 0 THEN NULL ELSE g % 3 <> 0 END,
       CASE WHEN g % 5 = 0 THEN NULL ELSE round((random() * 50000)::numeric, 2) END,
       now() - (random() * interval '5 years')
FROM generate_series(1, 1000000) g;

-- 5 000 000 orders (more than the app's exact-count limit: the pager shows an estimate).
CREATE TABLE pedidos (
  id bigserial PRIMARY KEY,
  cliente_id int NOT NULL REFERENCES clientes (id),
  status text NOT NULL,
  total numeric(12, 2) NOT NULL,
  pago boolean NOT NULL DEFAULT false,
  enviado boolean,
  criado_em timestamptz DEFAULT now()
);
INSERT INTO pedidos (cliente_id, status, total, pago, enviado, criado_em)
SELECT 1 + ((g::bigint * 7919) % 1000000)::int,
       (ARRAY['novo', 'pago', 'separando', 'enviado', 'entregue', 'cancelado'])[1 + g % 6],
       round((random() * 2000 + 5)::numeric, 2),
       g % 4 <> 0,
       CASE WHEN g % 4 = 0 THEN NULL ELSE g % 2 = 0 END,
       now() - (random() * interval '3 years')
FROM generate_series(1, 5000000) g;
CREATE INDEX pedidos_cliente_idx ON pedidos (cliente_id);
CREATE INDEX pedidos_criado_idx ON pedidos (criado_em);

-- 300 000 wide rows with many types and lots of NULLs.
CREATE TABLE eventos (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tipo text NOT NULL,
  cliente_id int REFERENCES clientes (id),
  payload jsonb,
  tags text[],
  ip inet,
  duracao_ms integer,
  valor double precision,
  descricao text,
  processado boolean,
  criado_em timestamptz NOT NULL DEFAULT now(),
  atualizado_em timestamptz
);
INSERT INTO eventos (tipo, cliente_id, payload, tags, ip, duracao_ms, valor, descricao, processado, criado_em, atualizado_em)
SELECT (ARRAY['login', 'logout', 'compra', 'erro', 'busca', 'checkout'])[1 + g % 6],
       CASE WHEN g % 9 = 0 THEN NULL ELSE 1 + (g * 31) % 1000000 END,
       jsonb_build_object('pagina', '/produto/' || g % 5000, 'navegador', (ARRAY['firefox', 'chrome', 'safari'])[1 + g % 3],
                          'itens', g % 12, 'cupom', CASE WHEN g % 10 = 0 THEN 'PROMO10' END),
       ARRAY['web', (ARRAY['mobile', 'desktop', 'tablet'])[1 + g % 3]],
       ('10.' || g % 256 || '.' || (g / 256) % 256 || '.' || g % 199)::inet,
       CASE WHEN g % 11 = 0 THEN NULL ELSE (random() * 5000)::int END,
       CASE WHEN g % 13 = 0 THEN NULL ELSE random() * 1000 END,
       CASE WHEN g % 4 = 0 THEN repeat('Descrição longa do evento ' || g || '. ', 1 + g % 8) END,
       CASE WHEN g % 6 = 0 THEN NULL ELSE g % 2 = 0 END,
       now() - (random() * interval '1 year'),
       CASE WHEN g % 3 = 0 THEN now() - (random() * interval '30 days') END
FROM generate_series(1, 300000) g;
CREATE INDEX eventos_tipo_idx ON eventos (tipo);

-- 200 small tables in their own schema, for the sidebar and Ctrl+P.
CREATE SCHEMA arquivo;
DO $$
BEGIN
  FOR i IN 1..200 LOOP
    EXECUTE format('CREATE TABLE arquivo.registro_%s (id serial PRIMARY KEY, ref text, valor numeric(10, 2), criado_em date)', lpad(i::text, 3, '0'));
    EXECUTE format('INSERT INTO arquivo.registro_%s (ref, valor, criado_em) SELECT ''R'' || g, g * 1.5, current_date - g FROM generate_series(1, 100) g', lpad(i::text, 3, '0'));
  END LOOP;
END $$;

ANALYZE;
SELECT relname AS tabela, n_live_tup AS linhas FROM pg_stat_user_tables WHERE schemaname = 'public' ORDER BY 1;
SELECT pg_size_pretty(pg_database_size(current_database())) AS tamanho;
