//! Legacy formats: `xls` (via calamine) and `rtf` (hand-written reader).

use std::io::Cursor;

use calamine::{Data, Reader, Xls};

use super::{squash, table_row, OfficeError, OfficeFormat, OfficeLimits, Sink};

// ---------------------------------------------------------------------------
// xls
// ---------------------------------------------------------------------------

pub(crate) fn xls(
    bytes: &[u8],
    limits: &OfficeLimits,
    sink: &mut Sink,
) -> Result<Option<String>, OfficeError> {
    let format = OfficeFormat::Xls;
    let mut workbook = Xls::new(Cursor::new(bytes)).map_err(|error| {
        let message = error.to_string();
        if message.to_ascii_lowercase().contains("encrypt")
            || message.to_ascii_lowercase().contains("password")
        {
            OfficeError::Encrypted
        } else {
            OfficeError::malformed(format, message)
        }
    })?;
    let names: Vec<String> = workbook
        .sheets_metadata()
        .iter()
        .filter(|sheet| sheet.visible != calamine::SheetVisible::VeryHidden)
        .map(|sheet| sheet.name.clone())
        .collect();
    for (index, name) in names.iter().enumerate() {
        if sink.full() {
            break;
        }
        if index >= limits.max_pages {
            sink.truncated = true;
            break;
        }
        let range = workbook
            .worksheet_range(name)
            .map_err(|error| OfficeError::malformed(format, error.to_string()))?;
        let (mut rows, mut cells, mut started) = (0usize, 0usize, false);
        for row in range.rows() {
            let mut texts: Vec<String> = row
                .iter()
                .map(|cell| match cell {
                    Data::Empty | Data::Error(_) => String::new(),
                    other => squash(&other.to_string())
                        .chars()
                        .take(limits.max_cell_chars)
                        .collect(),
                })
                .collect();
            while texts.last().is_some_and(String::is_empty) {
                texts.pop();
            }
            let Some(line) = table_row(&texts) else {
                continue;
            };
            if !started {
                started = true;
                sink.blank();
                sink.line(&format!("## {}", squash(name)));
                sink.blank();
            }
            sink.line(&line);
            rows += 1;
            cells += texts.iter().filter(|text| !text.is_empty()).count();
            if rows >= limits.max_rows_per_sheet || cells >= limits.max_cells_per_sheet {
                sink.truncated = true;
                break;
            }
            if sink.full() {
                break;
            }
        }
        sink.end_page();
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// rtf
// ---------------------------------------------------------------------------

const MAX_GROUP_DEPTH: usize = 256;

/// Destinations whose content is not document text.
const SKIPPED_DESTINATIONS: &[&str] = &[
    "fonttbl",
    "colortbl",
    "stylesheet",
    "info",
    "pict",
    "header",
    "headerl",
    "headerr",
    "headerf",
    "footer",
    "footerl",
    "footerr",
    "footerf",
    "footnote",
    "object",
    "listtable",
    "listoverridetable",
    "revtbl",
    "rsidtbl",
    "themedata",
    "colorschememapping",
    "datastore",
    "latentstyles",
    "xmlnstbl",
    "generator",
    "fldinst",
    "bkmkstart",
    "bkmkend",
    "private",
    "sp",
    "nonshppict",
    "xe",
    "tc",
];

const CP1252_HIGH: [char; 32] = [
    '€', '?', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '?', 'Ž', '?', '?', '‘', '’',
    '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '?', 'ž', 'Ÿ',
];

fn cp1252(byte: u8) -> char {
    match byte {
        0x80..=0x9f => CP1252_HIGH[(byte - 0x80) as usize],
        other => char::from(other),
    }
}

#[derive(Clone, Copy)]
struct Group {
    skip: bool,
    unicode_skip: usize,
}

fn hex_value(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|d| d as u8)
}

pub(crate) fn rtf(bytes: &[u8], sink: &mut Sink) -> Result<Option<String>, OfficeError> {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    if !bytes[start..].starts_with(b"{\\rtf") {
        return Err(OfficeError::malformed(
            OfficeFormat::Rtf,
            "missing {\\rtf header",
        ));
    }
    let mut current = Group {
        skip: false,
        unicode_skip: 1,
    };
    let mut stack: Vec<Group> = Vec::new();
    let mut at_group_start = false;
    let mut pending_skip = 0usize;
    let mut line = String::new();
    let mut i = start;

    fn push_char(line: &mut String, c: char) {
        if line.len() < 1 << 20 {
            line.push(c);
        }
    }
    fn paragraph(line: &mut String, sink: &mut Sink, blank: bool) {
        let text = squash(line);
        line.clear();
        if text.is_empty() {
            if blank {
                sink.blank();
            }
            return;
        }
        sink.line(&text);
        if blank {
            sink.blank();
        }
    }

    while i < bytes.len() && !sink.full() {
        let byte = bytes[i];
        match byte {
            b'{' => {
                if stack.len() >= MAX_GROUP_DEPTH {
                    return Err(OfficeError::LimitExceeded("rtf nesting"));
                }
                stack.push(current);
                at_group_start = true;
                i += 1;
            }
            b'}' => {
                if let Some(group) = stack.pop() {
                    current = group;
                }
                at_group_start = false;
                i += 1;
            }
            b'\r' | b'\n' => i += 1,
            b'\\' => {
                let Some(&next) = bytes.get(i + 1) else {
                    break;
                };
                if next.is_ascii_alphabetic() {
                    let word_start = i + 1;
                    let mut j = word_start;
                    while j < bytes.len() && bytes[j].is_ascii_alphabetic() && j - word_start < 32 {
                        j += 1;
                    }
                    let word = String::from_utf8_lossy(&bytes[word_start..j]).into_owned();
                    let mut param: Option<i64> = None;
                    let negative = bytes.get(j) == Some(&b'-');
                    let mut k = if negative { j + 1 } else { j };
                    let digits_start = k;
                    while k < bytes.len() && bytes[k].is_ascii_digit() && k - digits_start < 10 {
                        k += 1;
                    }
                    if k > digits_start {
                        let number: i64 = String::from_utf8_lossy(&bytes[digits_start..k])
                            .parse()
                            .unwrap_or(0);
                        param = Some(if negative { -number } else { number });
                        j = k;
                    }
                    if bytes.get(j) == Some(&b' ') {
                        j += 1;
                    }
                    i = j;
                    if at_group_start && SKIPPED_DESTINATIONS.contains(&word.as_str()) {
                        current.skip = true;
                    }
                    at_group_start = false;
                    if current.skip {
                        continue;
                    }
                    match word.as_str() {
                        "par" | "sect" | "page" => paragraph(&mut line, sink, true),
                        "row" => {
                            let trimmed = line.trim_end_matches([' ', '|']).to_string();
                            line = trimmed;
                            paragraph(&mut line, sink, false);
                        }
                        "line" | "tab" | "emspace" | "enspace" => push_char(&mut line, ' '),
                        "cell" => line.push_str(" | "),
                        "emdash" => push_char(&mut line, '—'),
                        "endash" => push_char(&mut line, '–'),
                        "lquote" => push_char(&mut line, '‘'),
                        "rquote" => push_char(&mut line, '’'),
                        "ldblquote" => push_char(&mut line, '“'),
                        "rdblquote" => push_char(&mut line, '”'),
                        "bullet" => push_char(&mut line, '•'),
                        "uc" => current.unicode_skip = param.unwrap_or(1).clamp(0, 8) as usize,
                        "u" => {
                            let code = param.unwrap_or(0);
                            let code = if code < 0 { code + 65536 } else { code };
                            if let Some(c) = u32::try_from(code).ok().and_then(char::from_u32) {
                                push_char(&mut line, c);
                            }
                            pending_skip = current.unicode_skip;
                        }
                        "bin" => {
                            let length = param.unwrap_or(0).max(0) as usize;
                            i = i.saturating_add(length).min(bytes.len());
                        }
                        _ => {}
                    }
                    continue;
                }
                at_group_start = at_group_start && next == b'*';
                match next {
                    b'*' => {
                        if at_group_start {
                            current.skip = true;
                        }
                        i += 2;
                    }
                    b'\'' => {
                        let value = match (bytes.get(i + 2), bytes.get(i + 3)) {
                            (Some(&high), Some(&low)) => {
                                hex_value(high).zip(hex_value(low)).map(|(h, l)| h * 16 + l)
                            }
                            _ => None,
                        };
                        i += if value.is_some() { 4 } else { 2 };
                        if let (Some(value), false) = (value, current.skip) {
                            if pending_skip > 0 {
                                pending_skip -= 1;
                            } else {
                                push_char(&mut line, cp1252(value));
                            }
                        }
                    }
                    b'\\' | b'{' | b'}' => {
                        if !current.skip {
                            if pending_skip > 0 {
                                pending_skip -= 1;
                            } else {
                                push_char(&mut line, char::from(next));
                            }
                        }
                        i += 2;
                    }
                    b'~' => {
                        if !current.skip {
                            push_char(&mut line, ' ');
                        }
                        i += 2;
                    }
                    b'_' => {
                        if !current.skip {
                            push_char(&mut line, '-');
                        }
                        i += 2;
                    }
                    b'\r' | b'\n' => {
                        if !current.skip {
                            paragraph(&mut line, sink, true);
                        }
                        i += 2;
                    }
                    _ => i += 2,
                }
            }
            other => {
                at_group_start = false;
                i += 1;
                if current.skip {
                    continue;
                }
                if pending_skip > 0 {
                    pending_skip -= 1;
                } else {
                    push_char(&mut line, cp1252(other));
                }
            }
        }
    }
    paragraph(&mut line, sink, false);
    Ok(None)
}
