-- Functions, procedures, triggers, views and sequences for the "dev" connection, on top of
-- dev/postgres.sql. Rerunnable.
--
--   docker exec -i dblite-dev-pg-1 psql -U postgres < dev/postgres-objects.sql

SET client_min_messages = warning;

-- Functions --------------------------------------------------------------------------------

-- Sum of a customer's orders.
CREATE OR REPLACE FUNCTION saldo_total_cliente(cid integer) RETURNS numeric
LANGUAGE sql STABLE AS $$
  SELECT coalesce(sum(total), 0) FROM pedidos WHERE cliente_id = cid;
$$;

-- A label from a balance.
CREATE OR REPLACE FUNCTION classifica_cliente(saldo numeric) RETURNS text
LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN
  IF saldo IS NULL THEN
    RETURN 'sem saldo';
  ELSIF saldo >= 40000 THEN
    RETURN 'ouro';
  ELSIF saldo >= 15000 THEN
    RETURN 'prata';
  END IF;
  RETURN 'bronze';
END;
$$;

-- Two versions with the same name (overloads): only digits, or formatted.
CREATE OR REPLACE FUNCTION formata_documento(doc text) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
  SELECT regexp_replace(doc, '\D', '', 'g');
$$;

CREATE OR REPLACE FUNCTION formata_documento(doc text, com_pontuacao boolean) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE
    WHEN NOT com_pontuacao THEN regexp_replace(doc, '\D', '', 'g')
    ELSE regexp_replace(regexp_replace(doc, '\D', '', 'g'), '(\d{3})(\d{3})(\d{3})(\d{2})', '\1.\2.\3-\4')
  END;
$$;

-- Procedures -------------------------------------------------------------------------------

-- Marks old orders as archived (defined only: running it touches millions of rows).
CREATE OR REPLACE PROCEDURE arquiva_pedidos_antigos(dias integer)
LANGUAGE plpgsql AS $$
BEGIN
  UPDATE pedidos SET status = 'arquivado'
  WHERE criado_em < now() - make_interval(days => dias) AND status IN ('entregue', 'cancelado');
  RAISE NOTICE 'pedidos arquivados';
END;
$$;

CREATE OR REPLACE PROCEDURE desativa_cliente(cid integer)
LANGUAGE sql AS $$
  UPDATE clientes SET ativo = false WHERE id = cid;
$$;

-- Triggers ---------------------------------------------------------------------------------

-- eventos.atualizado_em follows every update.
CREATE OR REPLACE FUNCTION toca_atualizado_em() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  NEW.atualizado_em := now();
  RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS eventos_atualizado_em ON eventos;
CREATE TRIGGER eventos_atualizado_em
  BEFORE UPDATE ON eventos
  FOR EACH ROW EXECUTE FUNCTION toca_atualizado_em();

-- Status changes of an order are logged.
CREATE TABLE IF NOT EXISTS pedidos_auditoria (
  id bigserial PRIMARY KEY,
  pedido_id bigint NOT NULL,
  status_antigo text,
  status_novo text,
  em timestamptz NOT NULL DEFAULT now()
);

CREATE OR REPLACE FUNCTION audita_status_pedido() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO pedidos_auditoria (pedido_id, status_antigo, status_novo)
  VALUES (NEW.id, OLD.status, NEW.status);
  RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS pedidos_status_auditoria ON pedidos;
CREATE TRIGGER pedidos_status_auditoria
  AFTER UPDATE OF status ON pedidos
  FOR EACH ROW WHEN (OLD.status IS DISTINCT FROM NEW.status)
  EXECUTE FUNCTION audita_status_pedido();

-- Views ------------------------------------------------------------------------------------

CREATE OR REPLACE VIEW clientes_ativos AS
  SELECT id, nome, email, cidade, uf, saldo
  FROM clientes
  WHERE ativo;

CREATE OR REPLACE VIEW vendas_por_uf AS
  SELECT c.uf, count(*) AS pedidos, round(sum(p.total), 2) AS total
  FROM pedidos p JOIN clientes c ON c.id = p.cliente_id
  GROUP BY c.uf;

CREATE OR REPLACE VIEW pedidos_do_mes AS
  SELECT id, cliente_id, status, total, criado_em
  FROM pedidos
  WHERE criado_em >= date_trunc('month', now());

-- Sequences --------------------------------------------------------------------------------

CREATE SEQUENCE IF NOT EXISTS numero_nota_fiscal_seq START 1000;
CREATE SEQUENCE IF NOT EXISTS protocolo_atendimento_seq START 500000 INCREMENT 10;
-- One already in use, so "current value" shows something.
SELECT nextval('numero_nota_fiscal_seq') FROM generate_series(1, 42);
