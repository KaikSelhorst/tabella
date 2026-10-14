# dblite

Cliente leve de banco de dados para **PostgreSQL** e **MySQL**, feito em Rust com
[egui](https://github.com/emilk/egui). Um único executável, sem runtime.

## Recursos

- Navegação de tabelas com paginação, filtro `WHERE`, ordenação e total de linhas
- Edição direta na grade e num painel de detalhes da linha (tabelas com chave primária): as
  alterações ficam pendentes e são salvas juntas numa transação (`Ctrl+S`), com desfazer (`Ctrl+Z`)
- Editor SQL com realce de sintaxe, autocomplete, formatação (`Shift+Alt+F`), histórico,
  consultas favoritas e execução da seleção
- Importar conexão colando uma URL (`postgres://…`, `mysql://…`)
- Reabre a última conexão, as abas (com o texto dos editores) e o tamanho da janela
- Paleta de comandos (`Ctrl+P`) para abrir tabelas, conexões e consultas recentes
- Inserção de linhas por formulário (cada coluna: padrão do banco, `NULL` ou valor)
- Aba "Estrutura": colunas, índices, chaves estrangeiras e DDL pronto para copiar
- Exportação para CSV, JSON ou SQL (`INSERT`): a página atual ou a tabela inteira (em blocos,
  sem carregar tudo na memória), salva na pasta Downloads
- Cópia de linhas como JSON ou `INSERT`
- Cancelamento de consultas em andamento e timeout por conexão (aplicado no servidor)
- Túnel SSH pelo cliente `ssh` do sistema (chaves ou agente)
- Cor por conexão: uma faixa colorida no topo enquanto conectado (ex.: vermelho para produção)

## Segurança

- **Senhas** nunca vão em texto para o disco: Gerenciador de Credenciais (Windows), chaveiro do
  sistema via Secret Service (Linux com GNOME Keyring/KWallet) ou, sem chaveiro, um cofre
  criptografado com senha mestra (Argon2id + ChaCha20-Poly1305).
- **Valores** nas edições geradas pelo app são parâmetros (MySQL) ou literais com escape
  independente da configuração do servidor (Postgres).
- **Filtro `WHERE`** aceita uma única condição (sem `;` nem `--`).
- **TLS**: Postgres tenta TLS por padrão (`prefer`); "SSL/TLS" exige e verifica o certificado.
- **Somente leitura** por conexão: a sessão fica read-only no próprio servidor.
- Comandos destrutivos no editor (`DROP`, `TRUNCATE`, `ALTER`, `DELETE`/`UPDATE` sem `WHERE`)
  pedem confirmação.

## Build

```sh
cargo build --release                                      # Linux
cargo build --release --target x86_64-pc-windows-gnu       # Windows (precisa de mingw-w64)
```

A feature `dev-conn` lista as conexões dos bancos de exemplo (abaixo). Ela embute as senhas de
teste no binário: não use em builds distribuídos.

## Bancos para desenvolvimento e testes

```sh
docker compose --profile dev up -d --wait     # dados de exemplo pesados: Postgres :5432, MySQL :3307 (~4 min)
docker compose --profile test up -d --wait    # bancos dos testes de integração
cargo test                                    # testes de unidade e de interface
cargo test -- --ignored db:: worker:: tunnel::   # testes contra os bancos reais (e SSH)
```

Os dados de exemplo são grandes de propósito: no Postgres, `clientes` (1 mi), `pedidos` (5 mi),
`eventos` (300 mil, tabela larga com JSON, arrays e IPs) e 200 tabelas pequenas no schema `arquivo`;
no MySQL, `produtos` (1 mi), `vendas` (3 mi), `logs` (200 mil) e 150 tabelas no banco `arquivo`.
Os scripts em `dev/` podem ser rodados de novo num banco existente (recriam o que é deles).

O perfil `test` inclui um servidor SSH descartável (porta 52222) que alcança o Postgres de teste;
a chave em `dev/ssh/` serve só para ele.

As capturas de tela da interface (dados falsos, sem banco) são geradas com
`DBLITE_SHOTS=<pasta> cargo test screenshots -- --ignored` e precisam de um driver Vulkan
(no WSL: `mesa-vulkan-drivers`).

## Configuração

Arquivos em `%APPDATA%\dblite` (Windows) ou `~/.config/dblite` (Linux); `DBLITE_CONFIG_DIR`
muda o local. `connections.json` não contém senhas.
