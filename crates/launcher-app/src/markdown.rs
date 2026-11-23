//! A small markdown renderer.
//!
//! The preview pane shows the description a plugin sends with a result, and
//! plugins may mark it as markdown. Flow Launcher renders that in a WebView;
//! this launcher has no web view, so the subset that plugins actually use is
//! rendered directly.
//!
//! Supported: ATX headings, paragraphs, bullet and numbered lists, block
//! quotes, fenced code blocks, and the inline `**bold**`, `*italic*`,
//! `` `code` `` and `[text](url)` markers.

/// How a run of text is styled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpanStyle {
    /// Plain text.
    #[default]
    Normal,
    /// `**bold**`
    Bold,
    /// `*italic*`
    Italic,
    /// `` `code` ``
    Code,
    /// `[text](url)`
    Link,
}

/// A run of text with one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: SpanStyle,
    /// The target of a link.
    pub link: Option<String>,
}

impl Span {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::Normal,
            link: None,
        }
    }
}

/// A block of the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// `# Title`
    Heading { level: usize, spans: Vec<Span> },
    /// A paragraph of text.
    Paragraph(Vec<Span>),
    /// `- item`
    ListItem { ordered: bool, spans: Vec<Span> },
    /// ``` code ```
    Code(String),
    /// `> quote`
    Quote(Vec<Span>),
    /// A horizontal rule.
    Rule,
}

/// Parse a markdown document into blocks.
pub fn parse(input: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();

    let flush_paragraph = |paragraph: &mut Vec<String>, blocks: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            let text = paragraph.join("\n");
            blocks.push(Block::Paragraph(parse_inline(&text)));
            paragraph.clear();
        }
    };

    let mut lines = input.lines().peekable();
    while let Some(raw) = lines.next() {
        let line = raw.trim_end();

        // Fenced code block.
        if let Some(rest) = line.strip_prefix("```") {
            flush_paragraph(&mut paragraph, &mut blocks);
            let language = rest.trim();
            let mut code = String::new();
            for code_line in lines.by_ref() {
                if code_line.trim_start().starts_with("```") {
                    break;
                }
                code.push_str(code_line);
                code.push('\n');
            }
            let _ = language;
            blocks.push(Block::Code(code.trim_end().to_owned()));
            continue;
        }

        // Horizontal rule.
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && (trimmed.chars().all(|c| c == '-') || trimmed.chars().all(|c| c == '*'))
            && trimmed.len() >= 3
        {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block::Rule);
            continue;
        }

        // Heading.
        if let Some(rest) = line.strip_prefix('#') {
            let level = 1 + rest.chars().take_while(|c| *c == '#').count();
            if level <= 6 && rest[rest.chars().take_while(|c| *c == '#').count()..].starts_with(' ')
            {
                flush_paragraph(&mut paragraph, &mut blocks);
                let text = rest.trim_start_matches('#').trim();
                blocks.push(Block::Heading {
                    level,
                    spans: parse_inline(text),
                });
                continue;
            }
        }

        // Quote.
        if let Some(rest) = line.strip_prefix('>') {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block::Quote(parse_inline(rest.trim())));
            continue;
        }

        // Bullet list.
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block::ListItem {
                ordered: false,
                spans: parse_inline(trimmed[2..].trim()),
            });
            continue;
        }

        // Numbered list.
        if let Some(dot) = trimmed.find(". ") {
            if dot > 0 && trimmed[..dot].chars().all(|c| c.is_ascii_digit()) {
                flush_paragraph(&mut paragraph, &mut blocks);
                blocks.push(Block::ListItem {
                    ordered: true,
                    spans: parse_inline(trimmed[dot + 2..].trim()),
                });
                continue;
            }
        }

        if trimmed.is_empty() {
            flush_paragraph(&mut paragraph, &mut blocks);
            continue;
        }

        paragraph.push(line.to_owned());
    }
    flush_paragraph(&mut paragraph, &mut blocks);

    blocks
}

/// Split a line of text into styled spans.
pub fn parse_inline(input: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0;
    let mut plain = String::new();

    macro_rules! flush {
        () => {
            if !plain.is_empty() {
                spans.push(Span::plain(std::mem::take(&mut plain)));
            }
        };
    }

    while index < chars.len() {
        let rest = &chars[index..];

        // `**bold**`
        if rest.starts_with(&['*', '*']) {
            if let Some(end) = find_marker(rest, 2, "**") {
                let inner: String = rest[2..end].iter().collect();
                flush!();
                spans.push(Span {
                    text: inner,
                    style: SpanStyle::Bold,
                    link: None,
                });
                index += end + 2;
                continue;
            }
        }

        // `` `code` ``
        if rest[0] == '`' {
            if let Some(end) = rest[1..].iter().position(|c| *c == '`') {
                let inner: String = rest[1..1 + end].iter().collect();
                flush!();
                spans.push(Span {
                    text: inner,
                    style: SpanStyle::Code,
                    link: None,
                });
                index += end + 2;
                continue;
            }
        }

        // `*italic*`
        if rest[0] == '*' {
            if let Some(end) = find_marker(rest, 1, "*") {
                let inner: String = rest[1..end].iter().collect();
                flush!();
                spans.push(Span {
                    text: inner,
                    style: SpanStyle::Italic,
                    link: None,
                });
                index += end + 1;
                continue;
            }
        }

        // `[text](url)`
        if rest[0] == '[' {
            if let Some(close) = rest.iter().position(|c| *c == ']') {
                if close + 1 < rest.len() && rest[close + 1] == '(' {
                    if let Some(paren) = rest[close + 2..].iter().position(|c| *c == ')') {
                        let text: String = rest[1..close].iter().collect();
                        let url: String = rest[close + 2..close + 2 + paren].iter().collect();
                        flush!();
                        spans.push(Span {
                            text,
                            style: SpanStyle::Link,
                            link: Some(url),
                        });
                        index += close + 2 + paren + 1;
                        continue;
                    }
                }
            }
        }

        plain.push(rest[0]);
        index += 1;
    }
    flush!();
    spans
}

/// Find the closing marker of an inline run.
///
/// `offset` is where the marker starts, `marker` is what closes it.
fn find_marker(chars: &[char], offset: usize, marker: &str) -> Option<usize> {
    let marker_chars: Vec<char> = marker.chars().collect();
    let mut index = offset + marker_chars.len();
    while index + marker_chars.len() <= chars.len() {
        if chars[index..index + marker_chars.len()] == marker_chars[..] {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// The plain text of a block, without any styling.
pub fn plain_text(block: &Block) -> String {
    match block {
        Block::Heading { spans, .. } | Block::Paragraph(spans) | Block::Quote(spans) => {
            spans.iter().map(|s| s.text.as_str()).collect()
        }
        Block::ListItem { spans, .. } => spans.iter().map(|s| s.text.as_str()).collect(),
        Block::Code(code) => code.clone(),
        Block::Rule => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_heading() {
        let blocks = parse("## Title\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Heading { level, spans } => {
                assert_eq!(*level, 2);
                assert_eq!(spans[0].text, "Title");
            }
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[test]
    fn parses_inline_styles() {
        let spans = parse_inline("plain **bold** *italic* `code` [link](https://x.dev)");
        let styles: Vec<SpanStyle> = spans.iter().map(|s| s.style).collect();
        assert_eq!(
            styles,
            vec![
                SpanStyle::Normal,
                SpanStyle::Bold,
                SpanStyle::Normal,
                SpanStyle::Italic,
                SpanStyle::Normal,
                SpanStyle::Code,
                SpanStyle::Normal,
                SpanStyle::Link,
            ]
        );
        let link = spans.last().expect("link");
        assert_eq!(link.text, "link");
        assert_eq!(link.link.as_deref(), Some("https://x.dev"));
    }

    #[test]
    fn parses_a_list() {
        let blocks = parse("- one\n- two\n");
        assert_eq!(blocks.len(), 2);
        for block in &blocks {
            match block {
                Block::ListItem { ordered, spans } => {
                    assert!(!ordered);
                    assert!(!spans.is_empty());
                }
                other => panic!("unexpected block: {other:?}"),
            }
        }
    }

    #[test]
    fn parses_an_ordered_list() {
        let blocks = parse("1. one\n2. two\n");
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            Block::ListItem { ordered, .. } => assert!(ordered),
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[test]
    fn parses_a_code_block() {
        let blocks = parse("```\nlet x = 1;\n```\n");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Code(code) => assert_eq!(code, "let x = 1;"),
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[test]
    fn a_stray_asterisk_does_not_break_the_parser() {
        let spans = parse_inline("2 * 3 = 6");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "2 * 3 = 6");
    }

    #[test]
    fn paragraphs_are_split_on_blank_lines() {
        let blocks = parse("first\n\nsecond\n");
        assert_eq!(blocks.len(), 2);
        assert_eq!(plain_text(&blocks[0]), "first");
        assert_eq!(plain_text(&blocks[1]), "second");
    }

    #[test]
    fn a_quote_is_its_own_block() {
        let blocks = parse("> quoted\n");
        assert_eq!(blocks.len(), 1);
        assert_eq!(plain_text(&blocks[0]), "quoted");
    }
}
