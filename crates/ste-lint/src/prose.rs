//! Markdown structure and original-source mappings, separate from language rules.

use crate::Format;
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use std::ops::Range;

#[derive(Default)]
pub(crate) struct Prose {
    pub text: String,
    // Each normalized byte maps to the whole original Unicode scalar or entity.
    spans: Vec<Range<usize>>,
}

impl Prose {
    fn push(&mut self, text: &str, span: Range<usize>, exact: bool) {
        for (offset, ch) in text.char_indices() {
            let original = if exact {
                span.start + offset..span.start + offset + ch.len_utf8()
            } else {
                span.clone()
            };
            self.spans
                .extend(std::iter::repeat_n(original, ch.len_utf8()));
            self.text.push(ch);
        }
    }

    pub fn source_span(&self, range: Range<usize>) -> Range<usize> {
        self.spans[range.start].start..self.spans[range.end - 1].end
    }
}

pub(crate) fn parse(source: &str, format: Format) -> Vec<Prose> {
    if matches!(format, Format::Plain) {
        let mut prose = Prose::default();
        prose.push(source, 0..source.len(), true);
        return vec![prose];
    }
    let mut result = Vec::new();
    let mut current = Prose::default();
    let mut excluded = 0;
    let mut autolink = false;
    let flush = |current: &mut Prose, result: &mut Vec<Prose>| {
        if !current.text.is_empty() {
            result.push(std::mem::take(current));
        }
    };
    for (event, span) in Parser::new_ext(source, Options::ENABLE_TABLES).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_) | Tag::BlockQuote(_)) => {
                if excluded == 0 {
                    flush(&mut current, &mut result);
                }
                excluded += 1;
            }
            Event::End(TagEnd::CodeBlock | TagEnd::BlockQuote(_)) => excluded -= 1,
            Event::End(TagEnd::Link) if autolink => autolink = false,
            _ if autolink => {}
            _ if excluded > 0 => {}
            Event::Start(Tag::Link {
                link_type: LinkType::Autolink | LinkType::Email,
                ..
            }) => {
                current.push(" 0 ", span, false);
                autolink = true;
            }
            Event::End(TagEnd::Link) => {}
            Event::Text(text) => {
                let exact = source.get(span.clone()) == Some(text.as_ref());
                current.push(&text, span, exact);
            }
            Event::Code(_) | Event::InlineMath(_) | Event::DisplayMath(_) => {
                current.push(" 0 ", span, false)
            }
            Event::SoftBreak | Event::HardBreak => current.push(" ", span, false),
            Event::End(
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item | TagEnd::TableCell,
            )
            | Event::Rule => flush(&mut current, &mut result),
            // HTML is not a prose escape hatch. The raw block remains subject to checks.
            Event::Html(text) => current.push(&text, span, true),
            _ => {}
        }
    }
    flush(&mut current, &mut result);
    result
}
