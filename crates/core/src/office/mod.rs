//! Text extraction for Office documents (feature `office`, pure Rust).
//!
//! Supported: OOXML (`docx`, `xlsx`, `pptx`), OpenDocument (`odt`, `ods`, `odp`), legacy
//! spreadsheets (`xls`, via `calamine`) and `rtf`. Legacy binary Word/PowerPoint (`doc`, `ppt`)
//! is rejected with [`OfficeError::LegacyBinary`].
//!
//! The result is Markdown-like text split into pages (a sheet or a slide is one page, a
//! document is one page), the same shape the Markdown and HTML inputs produce:
//! headings as `# …`, list items as `- …`, tables as `| a | b |` rows.
//!
//! Untrusted input is expected: zip entries are read with hard size caps (the declared sizes are
//! not trusted), the number of entries, sheets, rows, cells and the output size are limited (see
//! [`OfficeLimits`]; hitting a cap truncates the text and sets [`OfficeText::truncated`], except
//! for input/part size caps which fail), XML is parsed as a stream with a nesting limit, and
//! nothing panics on malformed files (errors are [`OfficeError`]).

mod legacy;
mod odf;
mod ooxml;
mod xml;

use std::io::{Cursor, Read};
use std::path::Path;

use thiserror::Error;

use crate::error::DcfError;

/// The Office formats this module reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficeFormat {
    Docx,
    Xlsx,
    Pptx,
    Odt,
    Ods,
    Odp,
    Xls,
    Rtf,
}

impl OfficeFormat {
    /// Format for a file extension (case-insensitive, without the dot).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "docx" => Some(Self::Docx),
            "xlsx" => Some(Self::Xlsx),
            "pptx" => Some(Self::Pptx),
            "odt" => Some(Self::Odt),
            "ods" => Some(Self::Ods),
            "odp" => Some(Self::Odp),
            "xls" => Some(Self::Xls),
            "rtf" => Some(Self::Rtf),
            _ => None,
        }
    }

    /// Whether the extension is a legacy binary Word/PowerPoint format that is not supported.
    pub fn is_legacy_binary_extension(ext: &str) -> bool {
        matches!(ext.to_ascii_lowercase().as_str(), "doc" | "ppt")
    }

    fn name(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
            Self::Odt => "odt",
            Self::Ods => "ods",
            Self::Odp => "odp",
            Self::Xls => "xls",
            Self::Rtf => "rtf",
        }
    }
}

/// Why an Office document could not be read.
#[derive(Debug, Error)]
pub enum OfficeError {
    #[error("legacy binary Word/PowerPoint files (.doc/.ppt) are not supported")]
    LegacyBinary,
    #[error("the document is encrypted or password protected")]
    Encrypted,
    #[error("the document exceeds a limit: {0}")]
    LimitExceeded(&'static str),
    #[error("malformed {format} document: {detail}")]
    Malformed {
        format: &'static str,
        detail: String,
    },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl OfficeError {
    pub(crate) fn malformed(format: OfficeFormat, detail: impl Into<String>) -> Self {
        Self::Malformed {
            format: format.name(),
            detail: detail.into(),
        }
    }
}

/// Resource caps for one extraction.
#[derive(Debug, Clone)]
pub struct OfficeLimits {
    /// Largest input file.
    pub max_input_bytes: u64,
    /// Most entries in a zip container.
    pub max_entries: usize,
    /// Most bytes read from one zip entry (actual, not declared).
    pub max_part_bytes: u64,
    /// Most bytes read from all zip entries together.
    pub max_total_bytes: u64,
    /// Most bytes of extracted text.
    pub max_output_bytes: usize,
    /// Most sheets or slides.
    pub max_pages: usize,
    /// Most rows per sheet or table.
    pub max_rows_per_sheet: usize,
    /// Most non-empty cells per sheet.
    pub max_cells_per_sheet: usize,
    /// Longest cell text in characters (longer text is cut).
    pub max_cell_chars: usize,
}

impl Default for OfficeLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 100 * 1024 * 1024,
            max_entries: 10_000,
            max_part_bytes: 64 * 1024 * 1024,
            max_total_bytes: 256 * 1024 * 1024,
            max_output_bytes: 4 * 1024 * 1024,
            max_pages: 500,
            max_rows_per_sheet: 10_000,
            max_cells_per_sheet: 200_000,
            max_cell_chars: 2_000,
        }
    }
}

/// Extracted text of an Office document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OfficeText {
    /// Title from the document metadata or, failing that, its first top-level heading.
    pub title: Option<String>,
    /// One entry per sheet or slide; a text document is a single page.
    pub pages: Vec<String>,
    /// A limit cut the text short.
    pub truncated: bool,
}

impl OfficeText {
    /// All pages joined with form feeds (the page separator the text inputs use).
    pub fn to_text(&self) -> String {
        self.pages.join("\u{c}")
    }
}

/// Reads a document from memory with the default limits.
pub fn extract(bytes: &[u8], format: OfficeFormat) -> Result<OfficeText, OfficeError> {
    extract_with_limits(bytes, format, &OfficeLimits::default())
}

/// Reads a document from memory; never panics, whatever the input.
pub fn extract_with_limits(
    bytes: &[u8],
    format: OfficeFormat,
    limits: &OfficeLimits,
) -> Result<OfficeText, OfficeError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(OfficeError::LimitExceeded("input size"));
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        extract_inner(bytes, format, limits)
    }));
    match outcome {
        Ok(result) => result,
        Err(_) => Err(OfficeError::malformed(format, "unreadable structure")),
    }
}

/// Reads a document from a file; the format comes from the extension.
pub fn extract_path(path: &Path) -> Result<OfficeText, DcfError> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();
    if OfficeFormat::is_legacy_binary_extension(ext) {
        return Err(OfficeError::LegacyBinary.into());
    }
    let format = OfficeFormat::from_extension(ext)
        .ok_or_else(|| DcfError::UnsupportedInput(path.to_path_buf()))?;
    let limits = OfficeLimits::default();
    if std::fs::metadata(path)?.len() > limits.max_input_bytes {
        return Err(OfficeError::LimitExceeded("input size").into());
    }
    let bytes = std::fs::read(path)?;
    Ok(extract_with_limits(&bytes, format, &limits)?)
}

fn extract_inner(
    bytes: &[u8],
    format: OfficeFormat,
    limits: &OfficeLimits,
) -> Result<OfficeText, OfficeError> {
    // A spreadsheet saved as .xls is often really xlsx.
    let format = if format == OfficeFormat::Xls && bytes.starts_with(b"PK") {
        OfficeFormat::Xlsx
    } else {
        format
    };
    let mut sink = Sink::new(limits.max_output_bytes);
    let title = match format {
        OfficeFormat::Docx => ooxml::docx(Package::open(bytes, format, limits)?, &mut sink)?,
        OfficeFormat::Pptx => ooxml::pptx(Package::open(bytes, format, limits)?, &mut sink)?,
        OfficeFormat::Xlsx => ooxml::xlsx(Package::open(bytes, format, limits)?, &mut sink)?,
        OfficeFormat::Odt | OfficeFormat::Ods | OfficeFormat::Odp => {
            odf::extract(Package::open(bytes, format, limits)?, &mut sink)?
        }
        OfficeFormat::Xls => legacy::xls(bytes, limits, &mut sink)?,
        OfficeFormat::Rtf => legacy::rtf(bytes, &mut sink)?,
    };
    let (pages, truncated) = sink.finish();
    Ok(OfficeText {
        title: title.map(|t| clean_title(&t)).filter(|t| !t.is_empty()),
        pages,
        truncated,
    })
}

fn clean_title(title: &str) -> String {
    squash(title).chars().take(300).collect()
}

/// Collapses whitespace and control characters into single spaces.
pub(crate) fn squash(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split(|c: char| c.is_whitespace() || c.is_control()) {
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// A table row as `| a | b |`; `None` when every cell is empty.
pub(crate) fn table_row(cells: &[String]) -> Option<String> {
    if cells.iter().all(|cell| cell.is_empty()) {
        return None;
    }
    let mut row = String::from("|");
    for cell in cells {
        row.push(' ');
        row.push_str(&cell.replace('|', "/"));
        row.push_str(" |");
    }
    Some(row)
}

/// Collects the output pages under a byte budget.
pub(crate) struct Sink {
    pages: Vec<String>,
    current: String,
    remaining: usize,
    pub(crate) truncated: bool,
}

impl Sink {
    fn new(max_bytes: usize) -> Self {
        Self {
            pages: Vec::new(),
            current: String::new(),
            remaining: max_bytes,
            truncated: false,
        }
    }

    /// Whether the budget is used up.
    pub(crate) fn full(&self) -> bool {
        self.truncated
    }

    /// Appends a line; an empty line is a paragraph break (never doubled).
    pub(crate) fn line(&mut self, text: &str) {
        if self.truncated {
            return;
        }
        for part in text.split('\n') {
            let clean: String = part
                .chars()
                .map(|c| if c.is_control() && c != '\t' { ' ' } else { c })
                .collect();
            let clean = clean.trim_end();
            if clean.trim().is_empty() {
                self.blank();
                continue;
            }
            if clean.len() + 1 > self.remaining {
                self.truncated = true;
                return;
            }
            self.remaining -= clean.len() + 1;
            self.current.push_str(clean);
            self.current.push('\n');
        }
    }

    /// A paragraph break.
    pub(crate) fn blank(&mut self) {
        if !self.current.is_empty() && !self.current.ends_with("\n\n") && self.remaining > 0 {
            self.current.push('\n');
            self.remaining -= 1;
        }
    }

    /// Ends the current page; blank pages are dropped.
    pub(crate) fn end_page(&mut self) {
        let page = std::mem::take(&mut self.current);
        if !page.trim().is_empty() {
            self.pages.push(page.trim_end().to_string());
        }
    }

    fn finish(mut self) -> (Vec<String>, bool) {
        self.end_page();
        (self.pages, self.truncated)
    }
}

/// A zip container read with hard caps.
pub(crate) struct Package<'a> {
    zip: zip::ZipArchive<Cursor<&'a [u8]>>,
    pub(crate) format: OfficeFormat,
    pub(crate) limits: &'a OfficeLimits,
    total: u64,
}

impl<'a> Package<'a> {
    fn open(
        bytes: &'a [u8],
        format: OfficeFormat,
        limits: &'a OfficeLimits,
    ) -> Result<Self, OfficeError> {
        // Password-protected OOXML files are OLE compound files, not zips.
        if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
            return Err(OfficeError::Encrypted);
        }
        let zip = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|error| OfficeError::malformed(format, format!("not a zip file: {error}")))?;
        if zip.len() > limits.max_entries {
            return Err(OfficeError::LimitExceeded("zip entries"));
        }
        Ok(Self {
            zip,
            format,
            limits,
            total: 0,
        })
    }

    pub(crate) fn names(&self) -> Vec<String> {
        self.zip
            .file_names()
            .filter_map(|name| name.ok().map(|name| name.into_owned()))
            .collect()
    }

    /// Reads one entry; `None` when it does not exist.
    pub(crate) fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, OfficeError> {
        let format = self.format;
        let mut entry = match self.zip.by_name(name) {
            Ok(entry) => entry,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(zip::result::ZipError::UnsupportedArchive(message))
                if message.to_ascii_lowercase().contains("password")
                    || message.to_ascii_lowercase().contains("encrypt") =>
            {
                return Err(OfficeError::Encrypted);
            }
            Err(error) => {
                return Err(OfficeError::malformed(format, format!("{name}: {error}")));
            }
        };
        if entry.encrypted() {
            return Err(OfficeError::Encrypted);
        }
        if entry.size() > self.limits.max_part_bytes {
            return Err(OfficeError::LimitExceeded("part size"));
        }
        let mut data = Vec::new();
        (&mut entry)
            .take(self.limits.max_part_bytes + 1)
            .read_to_end(&mut data)
            .map_err(|error| OfficeError::malformed(format, format!("{name}: {error}")))?;
        if data.len() as u64 > self.limits.max_part_bytes {
            return Err(OfficeError::LimitExceeded("part size"));
        }
        self.total += data.len() as u64;
        if self.total > self.limits.max_total_bytes {
            return Err(OfficeError::LimitExceeded("total uncompressed size"));
        }
        Ok(Some(data))
    }

    /// Like [`read`](Self::read) but a missing entry is an error.
    pub(crate) fn require(&mut self, name: &str) -> Result<Vec<u8>, OfficeError> {
        let format = self.format;
        self.read(name)?
            .ok_or_else(|| OfficeError::malformed(format, format!("{name} is missing")))
    }
}

#[cfg(test)]
mod tests;
