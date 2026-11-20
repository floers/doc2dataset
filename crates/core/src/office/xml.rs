//! Streaming XML walker: local names only, entities resolved, nesting bounded.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::{OfficeError, OfficeFormat};

const MAX_DEPTH: usize = 512;

pub(crate) struct Tag {
    /// Local name (namespace prefix removed).
    pub name: String,
    /// Attributes by local name.
    pub attrs: Vec<(String, String)>,
}

impl Tag {
    /// An attribute by qualified (`r:id`) or, failing that, local name (`id`).
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .or_else(|| self.attrs.iter().find(|(key, _)| local(key) == name))
            .map(|(_, value)| value.as_str())
    }
}

pub(crate) enum Node<'a> {
    Open(&'a Tag),
    Close(&'a str),
    Text(&'a str),
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn tag_of(start: &BytesStart<'_>) -> Tag {
    let name = local(start.name().as_ref()).to_string();
    let mut attrs = Vec::new();
    for attr in start.attributes().with_checks(false).flatten() {
        let key = attr.key.as_ref().to_string();
        attrs.push((key, unescape(&attr.value)));
    }
    Tag { name, attrs }
}

/// Resolves the predefined and numeric entities of an attribute value.
fn unescape(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        match rest.find(';') {
            Some(end) if end <= 10 => {
                match entity(&rest[1..end]) {
                    Some(c) => out.push(c),
                    None => out.push_str(&rest[..=end]),
                }
                rest = &rest[end + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code).filter(|c| *c != '\0')
        }
    }
}

/// Streams `xml` to `visit`; the visitor returns `false` to stop early.
pub(crate) fn walk(
    xml: &[u8],
    format: OfficeFormat,
    mut visit: impl FnMut(Node<'_>) -> bool,
) -> Result<(), OfficeError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().check_end_names = false;
    let mut buffer = Vec::new();
    let mut depth = 0usize;
    loop {
        buffer.clear();
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| OfficeError::malformed(format, error.to_string()))?;
        match event {
            Event::Start(start) => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(OfficeError::LimitExceeded("xml nesting"));
                }
                let tag = tag_of(&start);
                if !visit(Node::Open(&tag)) {
                    return Ok(());
                }
            }
            Event::Empty(start) => {
                let tag = tag_of(&start);
                if !visit(Node::Open(&tag)) || !visit(Node::Close(&tag.name)) {
                    return Ok(());
                }
            }
            Event::End(end) => {
                depth = depth.saturating_sub(1);
                let name = end.name().as_ref().to_string();
                if !visit(Node::Close(local(&name))) {
                    return Ok(());
                }
            }
            Event::Text(text) => {
                if !visit(Node::Text(&text.html_content())) {
                    return Ok(());
                }
            }
            Event::CData(data) => {
                if !visit(Node::Text(&data.html_content())) {
                    return Ok(());
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(c) = entity(&reference) {
                    if !visit(Node::Text(c.encode_utf8(&mut [0; 4]))) {
                        return Ok(());
                    }
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_entities_in_text_and_attributes() {
        let xml = br#"<a:r x:name="Q&amp;A &#228;"><t>1 &lt; 2 &#x41;</t></a:r>"#;
        let mut text = String::new();
        let mut names = Vec::new();
        walk(xml, OfficeFormat::Docx, |node| {
            match node {
                Node::Open(tag) => {
                    if let Some(value) = tag.attr("name") {
                        names.push(value.to_string());
                    }
                }
                Node::Text(value) => text.push_str(value),
                Node::Close(_) => {}
            }
            true
        })
        .unwrap();
        assert_eq!(text, "1 < 2 A");
        assert_eq!(names, ["Q&A ä"]);
    }

    #[test]
    fn deep_nesting_is_refused() {
        let xml = "<a>".repeat(MAX_DEPTH + 10);
        let result = walk(xml.as_bytes(), OfficeFormat::Docx, |_| true);
        assert!(matches!(result, Err(OfficeError::LimitExceeded(_))));
    }
}
