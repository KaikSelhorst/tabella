//! F1: every keyboard shortcut and snippet on one screen.

use eframe::egui::{self, Align, Layout, RichText};

use super::complete::SNIPPETS;
use super::{icon, theme};
use crate::app::App;

/// Width of the key column, the same in every group so the descriptions line up.
const KEY_COL: f32 = 250.0;

const GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Geral",
        &[
            (
                "Ctrl+P",
                "Paleta: tabelas, colunas (\"tabela.coluna\"), conexões, consultas, ações",
            ),
            ("Ctrl+Shift+F", "Buscar um valor em todas as tabelas"),
            ("Ctrl+Shift+H", "Histórico completo de consultas"),
            ("Ctrl+T", "Nova aba SQL"),
            (
                "Ctrl+W",
                "Fechar aba (clique direito na aba: renomear, duplicar, fechar outras)",
            ),
            ("Ctrl+Shift+T", "Reabrir a última aba fechada"),
            (
                "Ctrl+O",
                "Abrir arquivo .sql (ou arraste o arquivo para a janela)",
            ),
            (
                "Ctrl+P → Executar arquivo .sql",
                "Roda um dump/migração grande comando a comando, com progresso",
            ),
            (
                "Ctrl+P → Exportar banco",
                "Estrutura e dados como .sql, com progresso",
            ),
            (
                "Ctrl+P → Comparar estrutura",
                "Diferenças com outro schema ou banco e o SQL que iguala",
            ),
            ("Ctrl+Tab / Ctrl+Shift+Tab", "Próxima / aba anterior"),
            ("F1", "Esta tela"),
            (
                "Ctrl + / Ctrl − / Ctrl 0",
                "Zoom da interface (no Ctrl+P também, junto com o tema)",
            ),
        ],
    ),
    (
        "Editor SQL",
        &[
            ("Ctrl+Enter", "Executar o comando sob o cursor (ou a seleção)"),
            ("F5", "Executar tudo"),
            ("Ctrl+E", "Plano de execução (EXPLAIN)"),
            (
                "Ctrl+S / Ctrl+Shift+S",
                "Salvar o arquivo .sql / salvar como (com edições no resultado, Ctrl+S salva elas)",
            ),
            ("Shift+Alt+F", "Formatar SQL"),
            ("Ctrl+F / Ctrl+H", "Buscar / substituir"),
            ("Tab / Enter", "Aceitar sugestão do autocomplete"),
            (":nome", "Parâmetro: o valor é pedido antes de executar"),
            (
                "Ctrl+clique numa tabela",
                "Abre a tabela (passar o mouse mostra as colunas)",
            ),
        ],
    ),
    (
        "Grade",
        &[
            ("Setas / Tab", "Mover a célula selecionada"),
            ("Shift+setas, Shift+clique, arrastar", "Selecionar várias células"),
            ("Ctrl+A", "Selecionar tudo"),
            ("Ctrl+C", "Copiar (texto com tabs, cola no Excel)"),
            ("Ctrl+V", "Colar do Excel como alterações pendentes"),
            (
                "Ctrl+D",
                "Preencher para baixo (cada coluna recebe o valor da primeira linha selecionada)",
            ),
            ("Delete", "Marcar as linhas selecionadas para exclusão"),
            ("Enter / F2 / duplo clique", "Editar a célula"),
            (
                "Shift+Enter",
                "Abrir o valor no editor grande (JSON formatado, textos longos)",
            ),
            ("Ctrl+clique", "Abrir a linha de uma chave estrangeira"),
            ("Ctrl+F", "Buscar no resultado (tabelas: busca no banco)"),
            (
                "Clique direito no cabeçalho",
                "Filtrar valores (como no Excel), estatísticas, fixar, esconder colunas",
            ),
        ],
    ),
    (
        "Tabela",
        &[
            ("Ctrl+S", "Salvar alterações pendentes"),
            ("Ctrl+Z", "Desfazer a última alteração pendente"),
            ("F5", "Recarregar"),
            ("Enter no WHERE", "Aplicar o filtro"),
        ],
    ),
];

impl App {
    pub fn help_ui(&mut self, ctx: &egui::Context) {
        if !self.help {
            return;
        }
        let t = theme::tokens();
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("help"))
            .frame(theme::modal_frame(16))
            .show(ctx, |ui| {
                ui.set_width(640.0);
                ui.horizontal(|ui| {
                    ui.label(theme::icon(icon::KEYBOARD).size(16.0).color(t.lime));
                    ui.label(RichText::new("Atalhos").size(15.0).color(t.foreground));
                });
                ui.add_space(8.0);
                egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
                    for (group, keys) in GROUPS {
                        section(ui, group);
                        egui::Grid::new(("help", *group))
                            .num_columns(2)
                            .min_col_width(KEY_COL)
                            .spacing([16.0, 4.0])
                            .show(ui, |ui| {
                                for (k, what) in *keys {
                                    key(ui, k);
                                    ui.label(RichText::new(*what).color(t.muted_foreground));
                                    ui.end_row();
                                }
                            });
                    }
                    section(ui, "Snippets (digite e aceite no autocomplete)");
                    egui::Grid::new("help_snippets")
                        .num_columns(2)
                        .min_col_width(KEY_COL)
                        .spacing([16.0, 4.0])
                        .show(ui, |ui| {
                            for (trigger, what, _) in SNIPPETS {
                                key(ui, trigger);
                                ui.label(RichText::new(*what).color(t.muted_foreground));
                                ui.end_row();
                            }
                        });
                });
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(theme::outline_button("Fechar")).clicked() {
                        close = true;
                    }
                });
            });
        if close || modal.should_close() {
            self.help = false;
        }
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    let t = theme::tokens();
    ui.add_space(10.0);
    ui.label(RichText::new(title.to_uppercase()).size(11.0).color(t.subtle));
    ui.add_space(4.0);
}

/// A shortcut in a key-cap box.
fn key(ui: &mut egui::Ui, k: &str) {
    let t = theme::tokens();
    egui::Frame::new()
        .fill(t.sidebar)
        .stroke(egui::Stroke::new(1.0, t.border))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(k).monospace().size(12.0).color(t.foreground));
        });
}
