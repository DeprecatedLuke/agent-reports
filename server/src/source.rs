use std::borrow::Cow;
use std::ops::Range;

use html5gum::{DefaultEmitter, Token, Tokenizer};
use pulldown_cmark::{
    CodeBlockKind, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd, html::push_html,
};
use serde::Serialize;
use serde_yaml_ng::Value as Yaml;

use crate::icons;

/// A validation problem, with a 1-based line number in the *full* source.
#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub message: String,
}

impl Problem {
    fn at(line: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Parsed {
    pub title: String,
    /// Multi-line description, lines separated by `\n` (already trimmed).
    pub description: String,
    /// Everything after the closing `---` line, byte-for-byte.
    pub body: String,
}

/// Extract the body (everything after the closing `---` line) from a stored source.
/// Returns `""` when the source has no frontmatter; stored sources have been validated.
pub fn body_of(source: &str) -> &str {
    let mut offset = 0usize;
    let mut seen_first = false;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r').trim();
        if !seen_first {
            seen_first = true;
            if trimmed != "---" {
                return source;
            }
        } else if trimmed == "---" {
            return &source[offset + line.len()..];
        }
        offset += line.len();
    }
    ""
}

/// Elements that never take a closing tag, so they never open a subtree.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// A body is Markdown unless it is pure HTML: HTML bodies only carry whitespace between their
/// top-level elements, Markdown bodies carry text of their own.
pub fn is_markdown(body: &str) -> bool {
    let mut emitter = DefaultEmitter::<usize>::new_with_span();
    emitter.naively_switch_states(true);
    let tokenizer = Tokenizer::new_with_emitter(body, emitter);
    // Currently open elements, innermost last.
    let mut open: Vec<String> = Vec::new();

    for token in tokenizer.flatten() {
        match token {
            Token::StartTag(tag) => {
                let name = String::from_utf8_lossy(&tag.name).to_ascii_lowercase();
                if !tag.self_closing && !VOID_ELEMENTS.contains(&name.as_str()) {
                    open.push(name);
                }
            }
            Token::EndTag(tag) => {
                let name = String::from_utf8_lossy(&tag.name).to_ascii_lowercase();
                if let Some(pos) = open.iter().rposition(|element| *element == name) {
                    open.truncate(pos);
                }
            }
            Token::String(text) if open.is_empty() => {
                let text = String::from_utf8_lossy(text.value.as_slice());
                if !text.trim().is_empty() {
                    return true;
                }
            }
            _ => {}
        }
    }

    false
}

/// GitHub-flavoured Markdown, minus math (the iframe renders `$...$` itself).
fn markdown_options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// The HTML fragment to serve for a stored body: HTML bodies unchanged, Markdown converted.
pub fn fragment_html(body: &str) -> Cow<'_, str> {
    if !is_markdown(body) {
        return Cow::Borrowed(body);
    }
    Cow::Owned(markdown_to_html(body))
}

fn next_heading_level(level: HeadingLevel) -> HeadingLevel {
    match level {
        HeadingLevel::H1 => HeadingLevel::H2,
        HeadingLevel::H2 => HeadingLevel::H3,
        HeadingLevel::H3 => HeadingLevel::H4,
        HeadingLevel::H4 => HeadingLevel::H5,
        HeadingLevel::H5 | HeadingLevel::H6 => HeadingLevel::H6,
    }
}

fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn markdown_to_html(body: &str) -> String {
    let options = markdown_options();
    // The page title is the document's only `h1`, so Markdown headings shift down one level.
    let demote = Parser::new_ext(body, options).any(|event| {
        matches!(
            event,
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            })
        )
    });

    let mut mermaid: Option<String> = None;
    let mut events: Vec<Event> = Vec::with_capacity(body.len() / 8 + 8);
    for event in Parser::new_ext(body, options) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info.split_whitespace().next() == Some("mermaid") =>
            {
                mermaid = Some(String::new());
            }
            Event::Text(text) => match mermaid.as_mut() {
                Some(source) => source.push_str(&text),
                None => events.push(Event::Text(text)),
            },
            Event::End(TagEnd::CodeBlock) if mermaid.is_some() => {
                let source = escape_text(&mermaid.take().unwrap_or_default());
                events.push(Event::Html(
                    format!("<pre class=\"mermaid\">{source}</pre>").into(),
                ));
            }
            Event::Start(Tag::Heading {
                level,
                id,
                classes,
                attrs,
            }) if demote => events.push(Event::Start(Tag::Heading {
                level: next_heading_level(level),
                id,
                classes,
                attrs,
            })),
            Event::End(TagEnd::Heading(level)) if demote => {
                events.push(Event::End(TagEnd::Heading(next_heading_level(level))));
            }
            event => events.push(event),
        }
    }
    unwrap_image_paragraphs(&mut events);
    let mut out = String::with_capacity(body.len() * 3 / 2);
    push_html(&mut out, events.into_iter());
    out
}

/// Drop the `<p>` around a top-level paragraph that holds only images, one per line,
/// so a `![alt](assets/x.png)` line lands in the body exactly like a bare `<img>` line
/// (a raw HTML block): a direct child of the body, which the stylesheet centres.
/// Images sharing a line stay a paragraph, like `<img> <img>` on one line does.
fn unwrap_image_paragraphs(events: &mut Vec<Event>) {
    let mut remove: Vec<usize> = Vec::new();
    let mut depth = 0usize;
    // Index of the open top-level paragraph's start, while it still holds only images.
    let mut start: Option<usize> = None;
    let mut images = 0usize;
    // An image is on the current line, so the next one must follow a line break.
    let mut on_line = false;
    for (i, event) in events.iter().enumerate() {
        match event {
            Event::Start(Tag::Paragraph) if depth == 0 => {
                start = Some(i);
                images = 0;
                on_line = false;
            }
            Event::End(TagEnd::Paragraph) if depth == 1 => {
                if let Some(s) = start.take()
                    && images > 0
                {
                    remove.extend([s, i]);
                }
            }
            // Directly inside the paragraph: images and the line breaks between them only.
            _ if depth == 1 && start.is_some() => match event {
                Event::Start(Tag::Image { .. }) if !on_line => {
                    images += 1;
                    on_line = true;
                }
                Event::SoftBreak | Event::HardBreak => on_line = false,
                _ => start = None,
            },
            _ => {}
        }
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
    if remove.is_empty() {
        return;
    }
    let mut i = 0usize;
    let mut next = remove.iter().peekable();
    events.retain(|_| {
        let keep = next.peek() != Some(&&i);
        if !keep {
            next.next();
        }
        i += 1;
        keep
    });
}

/// ASCII punctuation a Markdown backslash escapes.
const MARKDOWN_ESCAPABLE: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

/// Blank out everything Markdown renders verbatim (`code`, code blocks, link destinations,
/// backslash escapes) so the HTML pass sees the same text the browser will.
fn mask_markdown(body: &str) -> String {
    let source = body.as_bytes();
    let mut ranges: Vec<Range<usize>> = Vec::new();

    let mut at = 0usize;
    while at + 1 < source.len() {
        if source[at] == b'\\' && MARKDOWN_ESCAPABLE.as_bytes().contains(&source[at + 1]) {
            ranges.push(at..at + 2);
            at += 2;
        } else {
            at += 1;
        }
    }

    let mut code_block_start: Option<usize> = None;
    for (event, range) in Parser::new_ext(body, markdown_options()).into_offset_iter() {
        match event {
            // Inline code, backticks included.
            Event::Code(_) => ranges.push(range),
            Event::Start(Tag::CodeBlock(_)) => code_block_start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = code_block_start.take() {
                    ranges.push(start..range.end);
                }
            }
            // Link/image destinations are attribute values, not prose.
            Event::Start(Tag::Link { link_type, .. } | Tag::Image { link_type, .. }) => {
                match link_type {
                    LinkType::Inline => {
                        if let Some(pos) = body[range.clone()].rfind("](") {
                            ranges.push(range.start + pos + 1..range.end);
                        }
                    }
                    // Autolinks show their (external) URL.
                    LinkType::Autolink | LinkType::Email => ranges.push(range),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let mut bytes = source.to_vec();
    for range in ranges {
        for byte in &mut bytes[range] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Problems only Markdown can have: an image pointing at an external URL.
fn markdown_problems(body: &str, base_line: usize) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (event, range) in Parser::new_ext(body, markdown_options()).into_offset_iter() {
        if let Event::Start(Tag::Image { dest_url, .. }) = event
            && is_external_url(&dest_url)
        {
            problems.push(Problem::at(
                Some(line_at(body, base_line, range.start)),
                "image URLs must point to `assets/...` or a `data:` URL, not an external URL",
            ));
        }
    }
    problems
}

/// Problem line for the first frontmatter key occurrence (1-based, full source).
fn key_line(lines: &[&str], key: &str, fm_end: usize) -> Option<usize> {
    for (i, line) in lines[1..fm_end].iter().enumerate() {
        let trimmed = line.trim_start();
        let candidate = trimmed.trim_start_matches(['"', '\'']);
        if candidate
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
        {
            return Some(i + 2);
        }
    }
    None
}

/// Banned glyphs in the parsed `title`/`description` value, one problem per occurrence. Each is
/// placed on the raw frontmatter line holding it (matched in order within the key's block);
/// glyphs that only exist after YAML unescaping fall back to the key's line.
fn frontmatter_glyph_problems(
    lines: &[&str],
    fm_end: usize,
    key: &str,
    value: &str,
) -> Vec<Problem> {
    let Some(start) = key_line(lines, key, fm_end) else {
        return Vec::new();
    };
    // `start` is 1-based; the key's block runs until the next non-indented, non-blank line.
    let block_end = (start..fm_end)
        .find(|&i| {
            let line = lines[i].trim_end_matches('\r');
            !line.trim().is_empty() && !line.starts_with([' ', '\t'])
        })
        .unwrap_or(fm_end);
    let mut raw: Vec<(char, usize)> = (start - 1..block_end)
        .flat_map(|i| icons::banned_glyphs(lines[i]).map(move |(_, c)| (c, i + 1)))
        .collect();
    icons::banned_glyphs(value)
        .map(|(_, c)| {
            let line = match raw.iter().position(|(rc, _)| *rc == c) {
                Some(pos) => raw.remove(pos).1,
                None => start,
            };
            Problem::at(Some(line), icons::frontmatter_glyph_message(c, key))
        })
        .collect()
}

fn line_at(body: &str, base_line: usize, byte_offset: usize) -> usize {
    let offset = byte_offset.min(body.len());
    base_line + body[..offset].bytes().filter(|b| *b == b'\n').count()
}

/// Parse frontmatter + validate the fragment. Returns every problem found, sorted by line.
pub fn parse_and_validate(
    source: &str,
    icon_names: &icons::IconNames,
) -> Result<Parsed, Vec<Problem>> {
    let lines: Vec<&str> = source.split('\n').collect();
    let first = lines
        .first()
        .copied()
        .unwrap_or("")
        .trim_end_matches('\r')
        .trim();
    if first != "---" {
        return Err(vec![Problem::at(
            Some(1),
            "missing YAML frontmatter: the source must start with a `---` line",
        )]);
    }

    let fm_end = match (1..lines.len()).find(|&i| lines[i].trim_end_matches('\r').trim() == "---") {
        Some(i) => i,
        None => {
            return Err(vec![Problem::at(
                Some(1),
                "unterminated YAML frontmatter: no closing `---` line",
            )]);
        }
    };

    let fm_text = lines[1..fm_end].join("\n");
    let mut problems = Vec::new();

    let title;
    let description;

    let parsed_frontmatter = if fm_text.trim().is_empty() {
        Ok(Yaml::Mapping(serde_yaml_ng::Mapping::new()))
    } else {
        serde_yaml_ng::from_str::<Yaml>(&fm_text)
    };

    match parsed_frontmatter {
        Ok(Yaml::Mapping(map)) => {
            for (k, _) in map.iter() {
                let Some(key) = k.as_str() else {
                    problems.push(Problem::at(
                        Some(2),
                        "frontmatter keys must be plain strings",
                    ));
                    continue;
                };
                if key != "title" && key != "description" {
                    problems.push(Problem::at(
                        key_line(&lines, key, fm_end),
                        format!("unknown frontmatter key `{key}` (only `title` and `description` are allowed)"),
                    ));
                }
            }

            let t = map.get("title");
            match t {
                None | Some(Yaml::Null) => problems.push(Problem::at(
                    key_line(&lines, "title", fm_end).or(Some(2)),
                    "missing required frontmatter key `title`",
                )),
                Some(Yaml::String(s)) => {
                    let s = s.trim_end_matches(['\r', '\n']);
                    if s.trim().is_empty() {
                        problems.push(Problem::at(
                            key_line(&lines, "title", fm_end),
                            "`title` must not be empty",
                        ));
                    } else if s.contains('\n') {
                        problems.push(Problem::at(
                            key_line(&lines, "title", fm_end),
                            "`title` must be a single line",
                        ));
                    } else if s.chars().count() > 200 {
                        problems.push(Problem::at(
                            key_line(&lines, "title", fm_end),
                            "`title` must be at most 200 characters",
                        ));
                    }
                }
                Some(_) => problems.push(Problem::at(
                    key_line(&lines, "title", fm_end),
                    "`title` must be a string",
                )),
            }

            match map.get("description") {
                None | Some(Yaml::Null) => description = String::new(),
                Some(Yaml::String(s)) => {
                    description = s.trim_end_matches(['\r', '\n']).to_string();
                }
                Some(_) => {
                    problems.push(Problem::at(
                        key_line(&lines, "description", fm_end),
                        "`description` must be a string",
                    ));
                    description = String::new();
                }
            }

            title = map
                .get("title")
                .and_then(|v| v.as_str())
                .map(|s| s.trim_end_matches(['\r', '\n']).trim().to_string())
                .unwrap_or_default();

            for (key, value) in [
                ("title", title.as_str()),
                ("description", description.as_str()),
            ] {
                problems.extend(frontmatter_glyph_problems(&lines, fm_end, key, value));
            }
        }
        Ok(_) => {
            problems.push(Problem::at(
                Some(2),
                "frontmatter must be a YAML mapping with `title` (and optional `description`)",
            ));
            title = String::new();
            description = String::new();
        }
        Err(err) => {
            problems.push(Problem::at(
                Some(2),
                format!("invalid YAML frontmatter: {err}"),
            ));
            title = String::new();
            description = String::new();
        }
    }

    let body = lines[fm_end + 1..].join("\n");
    let body_line = fm_end + 2;
    problems.extend(validate_body(&body, body_line, icon_names));

    if problems.is_empty() {
        Ok(Parsed {
            title,
            description,
            body,
        })
    } else {
        problems.sort_by_key(|p| p.line.unwrap_or(usize::MAX));
        Err(problems)
    }
}

const FORBIDDEN_ELEMENTS: &[&str] = &[
    "style", "link", "html", "head", "body", "meta", "base", "iframe", "object", "embed",
];

fn is_external_url(value: &str) -> bool {
    let v = value.trim_start();
    let lower = v.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("//")
}

fn check_url_attr(name: &str, value: &str) -> Option<String> {
    match name {
        "src" | "poster" => is_external_url(value).then(|| {
            format!("`{name}` must point to `assets/...` or a `data:` URL, not an external URL")
        }),
        "srcset" => {
            let bad = value.split(',').any(|candidate| {
                candidate
                    .split_whitespace()
                    .next()
                    .is_some_and(is_external_url)
            });
            bad.then(|| {
                "`srcset` must point to `assets/...` or `data:` URLs, not external URLs".to_string()
            })
        }
        _ => None,
    }
}

/// Subtrees whose content and attributes never need `<x-icon>` (code, data payloads, math, SVG).
const EXEMPT_ELEMENTS: &[&str] = &[
    "pre", "code", "kbd", "samp", "script", "x-diff", "x-chart", "svg", "math",
];

/// Attribute values that may contain banned code points (URLs and machine data, not prose).
const GLYPH_FREE_ATTRS: &[&str] = &["href", "src", "srcset", "poster"];

/// Report every banned glyph in `text`, numbering lines from `first_line`.
fn push_glyph_problems(text: &str, first_line: usize, problems: &mut Vec<Problem>) {
    let mut line = first_line;
    let mut last = 0usize;
    for (idx, c) in icons::banned_glyphs(text) {
        line += text[last..idx].bytes().filter(|b| *b == b'\n').count();
        last = idx;
        problems.push(Problem::at(Some(line), icons::glyph_message(c)));
    }
}

fn validate_body(body: &str, base_line: usize, icon_names: &icons::IconNames) -> Vec<Problem> {
    if !is_markdown(body) {
        return validate_markup(body, base_line, icon_names);
    }
    // Markdown: the HTML pass runs on a masked copy (same length and line breaks), so raw HTML
    // inside the body is still checked while code spans, code blocks and link destinations are not.
    let mut problems = validate_markup(&mask_markdown(body), base_line, icon_names);
    problems.extend(markdown_problems(body, base_line));
    problems
}

fn validate_markup(body: &str, base_line: usize, icon_names: &icons::IconNames) -> Vec<Problem> {
    let mut problems = Vec::new();
    // Currently open exempt subtrees, innermost last.
    let mut exempt: Vec<String> = Vec::new();

    let mut emitter = DefaultEmitter::<usize>::new_with_span();
    emitter.naively_switch_states(true);
    let tokenizer = Tokenizer::new_with_emitter(body, emitter);

    for token in tokenizer.flatten() {
        match token {
            Token::Doctype(spanned) => {
                problems.push(Problem::at(
                    Some(line_at(body, base_line, spanned.span.start)),
                    "<!doctype> is not allowed inside a report body",
                ));
            }
            Token::StartTag(tag) => {
                let line = Some(line_at(body, base_line, tag.span.start));
                let name = String::from_utf8_lossy(&tag.name).to_ascii_lowercase();
                if FORBIDDEN_ELEMENTS.contains(&name.as_str()) {
                    problems.push(Problem::at(
                        line,
                        format!("<{name}> is not allowed inside a report body"),
                    ));
                }
                if name == "script"
                    && tag
                        .attributes
                        .keys()
                        .any(|k| k.eq_ignore_ascii_case(b"src"))
                {
                    problems.push(Problem::at(line, "<script src=...> is not allowed"));
                }
                let is_exempt = EXEMPT_ELEMENTS.contains(&name.as_str());
                let glyphs_checked = exempt.is_empty() && !is_exempt;
                for (attr, value) in &tag.attributes {
                    let aname = String::from_utf8_lossy(attr).to_ascii_lowercase();
                    let aval = String::from_utf8_lossy(value.value.as_slice());
                    if aname == "style" {
                        problems.push(Problem::at(
                            line,
                            "inline `style=` attributes are not allowed",
                        ));
                    }
                    if let Some(msg) = check_url_attr(&aname, &aval) {
                        problems.push(Problem::at(line, msg));
                    }
                    if glyphs_checked
                        && !GLYPH_FREE_ATTRS.contains(&aname.as_str())
                        && !aname.starts_with("data-")
                    {
                        let first_line = line_at(body, base_line, value.span.start);
                        push_glyph_problems(&aval, first_line, &mut problems);
                    }
                }
                if glyphs_checked && name == "x-icon" {
                    let icon_name = tag
                        .attributes
                        .iter()
                        .find(|(attr, _)| attr.eq_ignore_ascii_case(b"name"))
                        .map(|(_, value)| {
                            String::from_utf8_lossy(value.value.as_slice()).to_string()
                        });
                    match icon_name {
                        Some(n) if icon_names.accepts(&n) => {}
                        Some(n) => {
                            problems.push(Problem::at(line, icons::unknown_icon_message(&n)))
                        }
                        None => problems.push(Problem::at(line, icons::unknown_icon_message(""))),
                    }
                }
                if !tag.self_closing && is_exempt {
                    exempt.push(name);
                }
            }
            Token::EndTag(tag) => {
                let name = String::from_utf8_lossy(&tag.name).to_ascii_lowercase();
                if let Some(pos) = exempt.iter().rposition(|open| *open == name) {
                    exempt.truncate(pos);
                }
            }
            Token::String(spanned) => {
                if exempt.is_empty() {
                    let text = String::from_utf8_lossy(spanned.value.as_slice());
                    let first_line = line_at(body, base_line, spanned.span.start);
                    push_glyph_problems(&text, first_line, &mut problems);
                }
            }
            Token::Comment(_) | Token::Error(_) => {}
        }
    }

    problems
}
