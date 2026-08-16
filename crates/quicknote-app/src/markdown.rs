//! Markdown 解析、消毒预览与单张便签导出。

use crate::{MarkdownLink, MarkdownPreview, NoteDocument};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// 把允许的 Markdown 解析成 Slint 可安全展示的纯文本。
pub(crate) fn render_preview(source: &str) -> MarkdownPreview {
    let options = Options::ENABLE_TASKLISTS;
    let mut text = String::new();
    let mut links = Vec::new();
    let mut open_links: Vec<(String, String)> = Vec::new();

    for event in Parser::new_ext(source, options) {
        match event {
            Event::Text(value) | Event::Code(value) => {
                text.push_str(&value);
                if let Some((_, label)) = open_links.last_mut() {
                    label.push_str(&value);
                }
            }
            Event::SoftBreak | Event::HardBreak => push_newline(&mut text),
            Event::Rule => {
                push_newline(&mut text);
                text.push_str("────────");
                push_newline(&mut text);
            }
            Event::TaskListMarker(checked) => {
                text.push_str(if checked { "☑ " } else { "☐ " });
            }
            Event::Start(Tag::Item) => text.push_str("• "),
            Event::Start(Tag::BlockQuote(_)) => text.push_str("› "),
            Event::Start(Tag::Link { dest_url, .. }) => {
                open_links.push((dest_url.into_string(), String::new()));
            }
            Event::End(TagEnd::Link) => {
                if let Some((url, label)) = open_links.pop()
                    && (url.starts_with("https://") || url.starts_with("http://"))
                {
                    links.push(MarkdownLink { label, url });
                }
            }
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::List(_)
                | TagEnd::BlockQuote(_)
                | TagEnd::CodeBlock,
            ) => push_newline(&mut text),
            // 原始 HTML、脚本和未启用的扩展事件不会进入预览输出。
            Event::Html(_) | Event::InlineHtml(_) => {}
            _ => {}
        }
    }

    MarkdownPreview {
        text: text.trim_end_matches('\n').to_owned(),
        links,
    }
}

/// 生成 UTF-8/LF front matter，并在其后原样附加 Markdown 正文。
pub(crate) fn markdown_export(note: &NoteDocument) -> String {
    let mut output = String::new();
    output.push_str("---\nquicknote_export_version: 1\n");
    output.push_str(&format!("id: {}\n", yaml_string(&note.id.to_string())));
    output.push_str(&format!("title: {}\n", yaml_string(&note.title)));
    output.push_str(&format!("lifecycle: {:?}\n", note.lifecycle).to_ascii_lowercase());
    output.push_str(&format!("content_revision: {}\n", note.content_revision));
    output.push_str(&format!("created_at_ms: {}\n", note.created_at_ms));
    output.push_str(&format!("updated_at_ms: {}\n", note.updated_at_ms));
    push_optional_number(&mut output, "archived_at_ms", note.archived_at_ms);
    push_optional_number(&mut output, "trashed_at_ms", note.trashed_at_ms);
    push_optional_number(&mut output, "due_at_ms", note.due_at_ms);
    output.push_str("---\n");
    output.push_str(&note.body);
    output
}

fn push_newline(text: &mut String) {
    if !text.ends_with('\n') {
        text.push('\n');
    }
}

fn yaml_string(value: &str) -> String {
    // JSON 双引号字符串是 YAML 1.2 的合法标量，并能安全保留中文和语法字符。
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

fn push_optional_number(output: &mut String, key: &str, value: Option<i64>) {
    match value {
        Some(value) => output.push_str(&format!("{key}: {value}\n")),
        None => output.push_str(&format!("{key}: null\n")),
    }
}

#[cfg(test)]
mod tests {
    use super::render_preview;

    #[test]
    fn preview_omits_html_and_requires_explicit_links() {
        let preview = render_preview(
            "# 标题\n\n- [x] 完成\n\n<script>alert('x')</script>\n\n[官网](https://example.com)",
        );
        assert!(preview.text.contains("标题"));
        assert!(preview.text.contains("☑ 完成"));
        assert!(!preview.text.contains("script"));
        assert_eq!(preview.links.len(), 1);
        assert_eq!(preview.links[0].url, "https://example.com");
    }
}
