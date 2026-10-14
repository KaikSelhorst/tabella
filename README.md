# dblite

Cliente leve de banco de dados para **PostgreSQL** e **MySQL**, feito em Rust com
[egui](https://github.com/emilk/egui). Um único executável, sem runtime.

## Recursos

- Navegação de tabelas com paginação, filtro `WHERE`, ordenação e total de linhas
- Edição direta na grade e num painel de detalhes da linha (tabelas com chave primária)
- Editor SQL com realce de sintaxe, autocomplete, histórico e execução da seleção
- Paleta de comandos (`Ctrl+P`) para abrir tabelas, conexões e consultas recentes
- Cópia de linhas como JSON ou `INSERT`
- Cancelamento de consultas em andamento

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
docker compose --profile dev up -d --wait     # dados de exemplo: Postgres :5432, MySQL :3307
docker compose --profile test up -d --wait    # bancos dos testes de integração
cargo test                                    # testes de unidade e de interface
cargo test -- --ignored db::                  # testes contra os bancos reais
```

As capturas de tela da interface (dados falsos, sem banco) são geradas com
`DBLITE_SHOTS=<pasta> cargo test screenshots -- --ignored` e precisam de um driver Vulkan
(no WSL: `mesa-vulkan-drivers`).

## Configuração

Arquivos em `%APPDATA%\dblite` (Windows) ou `~/.config/dblite` (Linux); `DBLITE_CONFIG_DIR`
muda o local. `connections.json` não contém senhas.
