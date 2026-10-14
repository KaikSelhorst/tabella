-- Sample data for the "dev" connection (Postgres on localhost:5432).
CREATE TABLE clientes (
  id serial PRIMARY KEY,
  nome text,
  email text,
  ativo boolean DEFAULT true,
  saldo numeric(12, 2),
  criado_em timestamp DEFAULT now()
);
INSERT INTO clientes (nome, email, ativo, saldo)
SELECT 'Cliente ' || g, 'c' || g || '@exemplo.com',
       CASE WHEN g % 7 = 0 THEN NULL ELSE g % 3 <> 0 END,
       CASE WHEN g % 5 = 0 THEN NULL ELSE round((g * 37.13)::numeric, 2) END
FROM generate_series(1, 500) g;

CREATE TABLE pedidos (
  id serial PRIMARY KEY,
  cliente_id int REFERENCES clientes (id),
  total numeric(12, 2) NOT NULL,
  pago boolean NOT NULL DEFAULT false,
  enviado boolean,
  criado_em timestamptz DEFAULT now()
);
INSERT INTO pedidos (cliente_id, total, pago, enviado, criado_em)
SELECT (g % 500) + 1, round((random() * 900 + 10)::numeric, 2), g % 4 <> 0,
       CASE WHEN g % 4 = 0 THEN NULL ELSE g % 2 = 0 END, now() - (g || ' hours')::interval
FROM generate_series(1, 2000) g;

ANALYZE;
