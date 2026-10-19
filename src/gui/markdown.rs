//! A small Markdown renderer for chat messages: headings, lists, quotes,
//! fenced code blocks, and inline `code`, **bold** and *italic*.

use eframe::egui::{self, text::LayoutJob, Color32, FontFamily, FontId, RichText, TextFormat};

use super::theme;

/// One block-level element.
#[derive(Debug, PartialEq)]
enum Block<'a> {
    Code {
        lang: &'a str,
        text: String,
    },
    Heading {
        level: usize,
        text: &'a str,
    },
    Bullet {
        indent: usize,
        marker: String,
        text: &'a str,
    },
    Quote(&'a str),
    Rule,
    Line(&'a str),
    Blank,
}

fn parse_blocks(text: &str) -> Vec<Block<'_>> {
    let mut blocks = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            let lang = rest.trim();
            let mut code = Vec::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") {
                    break;
                }
                code.push(l);
            }
            blocks.push(Block::Code {
                lang,
                text: code.join("\n"),
            });
            continue;
        }
        if trimmed.is_empty() {
            blocks.push(Block::Blank);
            continue;
        }
        if trimmed.chars().all(|c| c == '-' || c == '*' || c == '_') && trimmed.len() >= 3 {
            blocks.push(Block::Rule);
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            blocks.push(Block::Heading {
                level: hashes,
                text: trimmed[hashes..].trim(),
            });
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("> ") {
            blocks.push(Block::Quote(rest));
            continue;
        }
        let indent = (line.len() - trimmed.len()) / 2;
        if let Some(rest) = ["- ", "* ", "+ "]
            .iter()
            .find_map(|p| trimmed.strip_prefix(p))
        {
            blocks.push(Block::Bullet {
                indent,
                marker: "•".into(),
                text: rest,
            });
            continue;
        }
        let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && digits <= 3 && trimmed[digits..].starts_with(". ") {
            blocks.push(Block::Bullet {
                indent,
                marker: format!("{}.", &trimmed[..digits]),
                text: trimmed[digits + 2..].trim_start(),
            });
            continue;
        }
        blocks.push(Block::Line(line));
    }
    blocks
}

/// Splits a line into styled spans.
#[derive(Debug, PartialEq, Clone, Copy)]
enum Style {
    Plain,
    Code,
    Bold,
    Italic,
}

fn inline_spans(text: &str) -> Vec<(String, Style)> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<(String, Style)> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    let flush = |plain: &mut String, spans: &mut Vec<(String, Style)>| {
        if !plain.is_empty() {
            spans.push((std::mem::take(plain), Style::Plain));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        // Inline code.
        if c == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|x| *x == '`') {
                flush(&mut plain, &mut spans);
                spans.push((chars[i + 1..i + 1 + end].iter().collect(), Style::Code));
                i += end + 2;
                continue;
            }
        }
        // Bold.
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            if let Some(end) = find_pair(&chars, i + 2, "**") {
                flush(&mut plain, &mut spans);
                spans.push((chars[i + 2..end].iter().collect(), Style::Bold));
                i = end + 2;
                continue;
            }
        }
        // Italic (single * or _ not touching spaces).
        if (c == '*' || c == '_')
            && chars
                .get(i + 1)
                .map(|n| !n.is_whitespace() && *n != c)
                .unwrap_or(false)
        {
            let word_start = i == 0 || !chars[i - 1].is_alphanumeric();
            if word_start {
                if let Some(end) = find_pair(&chars, i + 1, &c.to_string()) {
                    if end > i + 1 && !chars[end - 1].is_whitespace() {
                        flush(&mut plain, &mut spans);
                        spans.push((chars[i + 1..end].iter().collect(), Style::Italic));
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        plain.push(c);
        i += 1;
    }
    flush(&mut plain, &mut spans);
    spans
}

fn find_pair(chars: &[char], from: usize, pat: &str) -> Option<usize> {
    let p: Vec<char> = pat.chars().collect();
    (from..chars.len().saturating_sub(p.len() - 1)).find(|&j| chars[j..j + p.len()] == p[..])
}

fn inline_job(text: &str, size: f32, color: Color32, wrap_width: f32, strong: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    for (segment, style) in inline_spans(text) {
        let mut format = TextFormat {
            font_id: FontId::new(size, FontFamily::Proportional),
            color,
            ..Default::default()
        };
        match style {
            Style::Plain => {
                if strong {
                    format.color = theme::text_strong();
                }
            }
            Style::Code => {
                format.font_id = FontId::new(size - 1.0, FontFamily::Monospace);
                format.background = theme::code_bg();
                format.color = theme::accent();
            }
            Style::Bold => format.color = theme::text_strong(),
            Style::Italic => format.italics = true,
        }
        job.append(&segment, 0.0, format);
    }
    job
}

/// Renders `text` into `ui`.
pub fn render(ui: &mut egui::Ui, text: &str) {
    let blocks = parse_blocks(text);
    let mut previous_blank = true;
    for block in blocks {
        match block {
            Block::Blank => {
                if !previous_blank {
                    ui.add_space(4.0);
                }
                previous_blank = true;
                continue;
            }
            Block::Code { lang, text } => {
                code_block(ui, lang, &text);
            }
            Block::Heading { level, text } => {
                let size = match level {
                    1 => 20.0,
                    2 => 18.0,
                    3 => 16.5,
                    _ => 15.0,
                };
                ui.add_space(4.0);
                let job = inline_job(text, size, theme::text_strong(), ui.available_width(), true);
                ui.add(egui::Label::new(job).wrap());
            }
            Block::Bullet {
                indent,
                marker,
                text,
            } => {
                ui.horizontal_top(|ui| {
                    ui.add_space(6.0 + indent as f32 * 14.0);
                    ui.label(RichText::new(marker).color(theme::accent()));
                    let job = inline_job(text, 14.5, theme::text(), ui.available_width(), false);
                    ui.add(egui::Label::new(job).wrap());
                });
            }
            Block::Quote(text) => {
                ui.horizontal_top(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(3.0, 18.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 1.0, theme::faint());
                    let job = inline_job(text, 14.5, theme::muted(), ui.available_width(), false);
                    ui.add(egui::Label::new(job).wrap());
                });
            }
            Block::Rule => {
                ui.separator();
            }
            Block::Line(text) => {
                let job = inline_job(text, 14.5, theme::text(), ui.available_width(), false);
                ui.add(egui::Label::new(job).wrap());
            }
        }
        previous_blank = false;
    }
}

fn code_block(ui: &mut egui::Ui, lang: &str, text: &str) {
    egui::Frame::new()
        .fill(theme::code_bg())
        .stroke(egui::Stroke::new(1.0, theme::border()))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if lang.is_empty() { "code" } else { lang })
                        .small()
                        .color(theme::faint()),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(text.to_string());
                    }
                });
            });
            egui::ScrollArea::horizontal()
                .id_salt(text.len() ^ lang.len())
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(text).monospace().color(theme::text()))
                            .extend(),
                    );
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<&'static str> {
        parse_blocks(text)
            .iter()
            .map(|b| match b {
                Block::Code { .. } => "code",
                Block::Heading { .. } => "heading",
                Block::Bullet { .. } => "bullet",
                Block::Quote(_) => "quote",
                Block::Rule => "rule",
                Block::Line(_) => "line",
                Block::Blank => "blank",
            })
            .collect()
    }

    #[test]
    fn parses_block_types() {
        let md = "# Title\n\nSome text\n- one\n  - nested\n1. first\n> quote\n---\n```python\nprint(1)\n```\nend";
        assert_eq!(
            kinds(md),
            [
                "heading", "blank", "line", "bullet", "bullet", "bullet", "quote", "rule", "code",
                "line"
            ]
        );
    }

    #[test]
    fn code_blocks_keep_their_content() {
        let blocks = parse_blocks("```rust\nfn main() {\n    println!(\"hi\");\n}\n```");
        assert_eq!(
            blocks,
            vec![Block::Code {
                lang: "rust",
                text: "fn main() {\n    println!(\"hi\");\n}".into()
            }]
        );
    }

    #[test]
    fn unterminated_fence_does_not_panic() {
        let blocks = parse_blocks("```\nabc");
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn inline_styles() {
        let spans = inline_spans("use `cargo run` and **bold** with *italic* text");
        assert_eq!(
            spans,
            vec![
                ("use ".to_string(), Style::Plain),
                ("cargo run".to_string(), Style::Code),
                (" and ".to_string(), Style::Plain),
                ("bold".to_string(), Style::Bold),
                (" with ".to_string(), Style::Plain),
                ("italic".to_string(), Style::Italic),
                (" text".to_string(), Style::Plain),
            ]
        );
    }

    #[test]
    fn stray_markers_stay_plain() {
        for text in [
            "2 * 3 * 4",
            "snake_case_name",
            "a ** b",
            "unclosed `code",
            "5*",
        ] {
            let spans = inline_spans(text);
            let joined: String = spans.iter().map(|(s, _)| s.as_str()).collect();
            assert_eq!(joined, text, "text must not be lost");
            assert!(
                spans.iter().all(|(_, s)| *s == Style::Plain),
                "{text}: {spans:?}"
            );
        }
    }
}
