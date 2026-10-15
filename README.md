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
- Exportação para CSV, Excel, JSON ou SQL (`INSERT`): a página atual ou a tabela inteira (em blocos,
  sem carregar tudo na memória), salva na pasta Downloads
- Cópia de linhas como JSON ou `INSERT`
- Cancelamento de consultas em andamento e timeout por conexão (aplicado no servidor)
- Túnel SSH pelo cliente `ssh` do sistema (chaves ou agente)
- Cor por conexão: uma faixa colorida no topo enquanto conectado (ex.: vermelho para produção)
- Busca em todas as colunas da tabela e "filtrar por este valor" (igual/diferente) no menu da célula
- Chaves estrangeiras clicáveis: `Ctrl+clique` (ou o menu da célula) abre a linha referenciada
- Tamanho em disco das tabelas na sidebar, com opção de ordenar por tamanho
- Monitor de atividade: sessões do servidor, atualização automática, cancelar consulta ou
  encerrar conexão (com confirmação)
- Editor: `Ctrl+Enter` executa o comando sob o cursor (marcado na margem), `F5` executa tudo;
  um script com várias consultas mostra um resultado para cada
- Plano de execução visual (`Ctrl+E`, ou EXPLAIN ANALYZE só para leituras): tempo/custo de cada
  etapa, varreduras completas em tabelas grandes e estimativas erradas destacadas
- Seleção de várias células (arrastar, `Shift`+clique/setas, `Ctrl+A`): copia como texto
  separado por tabs (cola no Excel) e mostra contagem, soma, média, mínimo e máximo
- Colar do Excel na grade (`Ctrl+V`): cada célula vira uma alteração pendente
- Importar CSV para uma tabela (botão ou arrastar o arquivo): separador e codificação detectados,
  colunas associadas pelo nome, números como `1.520,30` e `sim`/`não` convertidos, tudo numa
  transação
- Commit manual por conexão (ou só na sessão, pelo menu "…"): alterações ficam numa transação
  até "Confirmar" ou "Desfazer", com faixa de aviso e confirmação ao fechar a janela. Edições
  da grade e importações usam savepoint dentro dela. No MySQL, DDL confirma sozinho
- Parâmetros no SQL (`WHERE id = :id`): o app pede os valores antes de executar
- Buscar e substituir no editor (`Ctrl+F` / `Ctrl+H`)
- JSON em árvore no painel de detalhes (copia valor ou caminho, ex.: `$.itens[0].preco`)
- Estatísticas da coluna (clique direito no cabeçalho): nulos, distintos, mín/máx e valores
  mais comuns, com atalho para filtrar
- Fixar colunas à esquerda (ficam visíveis ao rolar para o lado) e esconder colunas, pelo
  clique direito no cabeçalho; lembrado por aba de tabela
- Buscar no resultado do editor (`Ctrl+F` na grade); em tabelas, `Ctrl+F` vai para a busca no banco
- Duplicar linha: formulário de inserção já preenchido (chaves geradas ficam no padrão)
- Filtros salvos por tabela (funil antes do WHERE)
- Snippets no autocomplete (`sel`, `cnt`, `grp`, `dup`, `join`, `upd`…) e `:parâmetros` destacados
- Atalhos de teclado e snippets em uma tela (`F1`)
- Criar e alterar tabelas por formulário: colunas (nome, tipo, nulo, padrão), chave primária e
  índices, com o SQL gerado à vista; abre no editor ou executa (com confirmação)
- Prévia de UPDATE/DELETE (botão "Prévia" ou `Ctrl+Shift+Enter`; automática se a conexão pedir):
  quantas linhas e quais seriam afetadas, sem alterar nada
- "Linhas que apontam para esta" no menu da célula: abre as tabelas cujas chaves estrangeiras
  referenciam a linha
- Monitor de atividade com três visões: sessões, bloqueios (quem trava quem) e consultas lentas
  (`pg_stat_statements` / `performance_schema`), com "Zerar estatísticas"
- Gráfico rápido do resultado do editor: barras ou linha, valor ao passar o mouse
- Resultado de um SELECT simples de uma tabela (com a chave primária no SELECT) é editável no
  próprio editor: duplo clique, colar, excluir, `Ctrl+S` salva e a consulta roda de novo
- Sugestão de índice no plano de execução: varreduras completas com filtro em tabelas grandes
  viram um `CREATE INDEX` pronto para abrir no editor
- Painel "Servidor" no monitor de atividade: versão, tempo no ar, tamanho, conexões, acerto do
  cache, maiores tabelas e índices sem uso
- Ícone próprio, e aviso (botão piscando na barra de tarefas e título) quando algo que levou
  10 s ou mais termina com a janela em segundo plano
- Colunas no `Ctrl+P`: digite parte do nome ("cpf", "clientes.email") para achar onde está
- Buscar um valor em todas as tabelas (`Ctrl+Shift+F`): olha só colunas onde o valor caberia,
  pula tabelas grandes por padrão, mostra o progresso, pode ser cancelada e abre cada achado
  já filtrado
- No editor, passar o mouse sobre o nome de uma tabela mostra as colunas; `Ctrl+clique` abre
- Abas: clique direito para renomear, duplicar, fechar as outras/da direita; `Ctrl+Shift+T`
  reabre a última fechada
- "Repetir": roda a última consulta de leitura a cada 2 s … 1 min
- Diagrama ER por schema (`Ctrl+P` ou clique direito no schema): tabelas e chaves
  estrangeiras organizadas sozinhas, com zoom, arrastar e duplo clique para abrir
- Autocomplete entende apelidos (`FROM pedidos p` → `p.` sugere as colunas de pedidos) e,
  depois de `JOIN`, sugere a junção pronta pelas chaves estrangeiras
  (`clientes c ON c.id = p.cliente_id`)
- Arquivos `.sql`: abrir (`Ctrl+O` ou arrastar), salvar (`Ctrl+S`) e salvar como
  (`Ctrl+Shift+S`); a aba marca alterações não salvas e avisa antes de fechar
- Histórico completo (`Ctrl+Shift+H`): busca, filtro por conexão e por erro, abrir no editor
- Tema claro ou escuro e zoom da interface pelo `Ctrl+P` (zoom também com `Ctrl +`/`Ctrl −`/
  `Ctrl 0`); os dois ficam lembrados
- Funções, procedures, triggers e sequences na sidebar (views junto com as tabelas): um clique
  abre o código num editor, pronto para alterar e rodar
- "Gerar SQL" no clique direito da tabela: SELECT, INSERT, UPDATE e DELETE pela chave, com
  `:parâmetros`
- Edição em massa: definir valor ou NULL em todas as células selecionadas, preencher para baixo
  (`Ctrl+D`) e excluir as linhas selecionadas (`Delete`)
- Exportar para Excel (.xlsx, com números e booleanos como tais) e copiar como tabela Markdown
- Trocar de banco/schema sem editar a conexão (seletores abaixo da conexão na sidebar); o
  editor mostra onde os nomes sem schema vão parar
- Executar função/procedure pelo clique direito: os argumentos viram `:parâmetros` pedidos
  num formulário
- Ações de tabela no clique direito: renomear, duplicar (só estrutura ou com dados), esvaziar
  (`TRUNCATE`) e excluir, estas duas só depois de digitar o nome da tabela
- Executar arquivo `.sql` grande (`Ctrl+P` ou menu de arquivo do editor) sem abrir no editor:
  lê comando a comando (entende `DELIMITER`, `$$` e `COPY … FROM stdin` do pg_dump), mostra o
  progresso, para no primeiro erro dizendo a linha, pode ser cancelado e roda numa transação
- Erro de SQL marcado no editor: o trecho fica sublinhado, o cursor vai até ele e a mensagem
  diz linha e coluna
- Filtro por coluna estilo Excel (clique direito no cabeçalho → "Filtrar valores…"): valores
  distintos com contagem, marcar/desmarcar e busca; vira `IN`/`NOT IN` no WHERE e a coluna ganha
  um funil
- Exportar tabela, schema ou banco como `.sql` (estrutura e/ou dados, views, funções, triggers),
  em blocos e com progresso; recarrega com "Executar arquivo .sql", psql ou mysql
- Comparar estrutura entre schemas ou bancos (inclusive de outra conexão, ex.: dev × produção)
  e gerar o SQL que iguala o destino; remoções destacadas e `DROP TABLE` só comentado
- Importar Excel (.xlsx, .xls) e ODS no mesmo diálogo do CSV

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
