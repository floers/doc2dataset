//! OOXML: docx, pptx, xlsx.

use super::xml::{walk, Node};
use super::{squash, table_row, OfficeError, Package, Sink};

struct Rel {
    id: String,
    kind: String,
    target: String,
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

fn resolve(base_dir: &str, target: &str) -> String {
    let joined = match target.strip_prefix('/') {
        Some(absolute) => absolute.to_string(),
        None if base_dir.is_empty() => target.to_string(),
        None => format!("{base_dir}/{target}"),
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn rels(pkg: &mut Package<'_>, part: &str) -> Result<Vec<Rel>, OfficeError> {
    let file = part.rsplit('/').next().unwrap_or(part);
    let path = format!("{}/_rels/{file}.rels", dir_of(part));
    let path = path.trim_start_matches('/');
    let Some(xml) = pkg.read(path)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    walk(&xml, pkg.format, |node| {
        if let Node::Open(tag) = node {
            if tag.name == "Relationship" && tag.attr("TargetMode") != Some("External") {
                out.push(Rel {
                    id: tag.attr("Id").unwrap_or_default().to_string(),
                    kind: tag.attr("Type").unwrap_or_default().to_string(),
                    target: tag.attr("Target").unwrap_or_default().to_string(),
                });
            }
        }
        true
    })?;
    Ok(out)
}

/// `dc:title` of `docProps/core.xml`; best effort.
fn core_title(pkg: &mut Package<'_>) -> Option<String> {
    let xml = pkg.read("docProps/core.xml").ok().flatten()?;
    let mut title = String::new();
    let mut inside = false;
    let _ = walk(&xml, pkg.format, |node| {
        match node {
            Node::Open(tag) => inside = tag.name == "title",
            Node::Text(text) if inside => title.push_str(text),
            Node::Close("title") => return false,
            _ => {}
        }
        true
    });
    let title = squash(&title);
    (!title.is_empty()).then_some(title)
}

fn cap_chars(text: String, max: usize) -> String {
    if text.chars().count() <= max {
        text
    } else {
        text.chars().take(max).collect()
    }
}

// ---------------------------------------------------------------------------
// docx
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Para {
    text: String,
    heading: Option<usize>,
    list: Option<usize>,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: Option<String>,
}

struct Docx {
    paras: Vec<Para>,
    tables: Vec<Table>,
    in_text: bool,
    fallback: usize,
    first_heading: Option<String>,
    max_cell: usize,
    max_rows: usize,
}

fn heading_level(style: Option<&str>) -> Option<usize> {
    let style = style?.to_lowercase();
    if style == "title" || style == "titel" {
        return Some(1);
    }
    for prefix in ["heading", "berschrift", "überschrift"] {
        if let Some(rest) = style.strip_prefix(prefix) {
            let level: usize = rest.trim().parse().ok()?;
            if (1..=9).contains(&level) {
                return Some(level.min(6));
            }
        }
    }
    None
}

impl Docx {
    fn event(&mut self, node: Node<'_>, sink: &mut Sink) -> bool {
        match node {
            Node::Open(tag) => match tag.name.as_str() {
                "Fallback" => self.fallback += 1,
                "tbl" => self.tables.push(Table::default()),
                "tr" => {
                    if let Some(table) = self.tables.last_mut() {
                        table.row.clear();
                    }
                }
                "tc" => {
                    if let Some(table) = self.tables.last_mut() {
                        table.cell = Some(String::new());
                    }
                }
                "p" => self.paras.push(Para::default()),
                "pStyle" => {
                    if let Some(para) = self.paras.last_mut() {
                        para.heading = heading_level(tag.attr("val"));
                    }
                }
                "numPr" => {
                    if let Some(para) = self.paras.last_mut() {
                        para.list.get_or_insert(0);
                    }
                }
                "ilvl" => {
                    if let Some(para) = self.paras.last_mut() {
                        if para.list.is_some() {
                            para.list = Some(
                                tag.attr("val")
                                    .and_then(|v| v.parse().ok())
                                    .unwrap_or(0usize)
                                    .min(8),
                            );
                        }
                    }
                }
                "t" if self.fallback == 0 => self.in_text = true,
                "tab" | "br" | "cr" if self.fallback == 0 => {
                    if let Some(para) = self.paras.last_mut() {
                        para.text.push(' ');
                    }
                }
                _ => {}
            },
            Node::Text(text) => {
                if self.in_text {
                    if let Some(para) = self.paras.last_mut() {
                        if para.text.len() < 1 << 20 {
                            para.text.push_str(text);
                        }
                    }
                }
            }
            Node::Close(name) => match name {
                "t" => self.in_text = false,
                "Fallback" => self.fallback = self.fallback.saturating_sub(1),
                "p" => {
                    if let Some(para) = self.paras.pop() {
                        self.finish_para(para, sink);
                    }
                }
                "tc" => {
                    if let Some(table) = self.tables.last_mut() {
                        if let Some(cell) = table.cell.take() {
                            table.row.push(cap_chars(cell, self.max_cell));
                        }
                    }
                }
                "tr" => {
                    if let Some(table) = self.tables.last_mut() {
                        let row = std::mem::take(&mut table.row);
                        table.rows.push(row);
                    }
                }
                "tbl" => {
                    if let Some(table) = self.tables.pop() {
                        self.finish_table(table, sink);
                    }
                }
                _ => {}
            },
        }
        !sink.full()
    }

    fn finish_para(&mut self, para: Para, sink: &mut Sink) {
        let text = squash(&para.text);
        if text.is_empty() {
            return;
        }
        if let Some(cell) = self.tables.last_mut().and_then(|t| t.cell.as_mut()) {
            if cell.len() < self.max_cell * 4 {
                if !cell.is_empty() {
                    cell.push(' ');
                }
                cell.push_str(&text);
            }
            return;
        }
        if let Some(level) = para.heading {
            if level == 1 && self.first_heading.is_none() {
                self.first_heading = Some(text.clone());
            }
            sink.blank();
            sink.line(&format!("{} {text}", "#".repeat(level)));
            sink.blank();
        } else if let Some(level) = para.list {
            sink.line(&format!("{}- {text}", "  ".repeat(level)));
        } else {
            sink.line(&text);
            sink.blank();
        }
    }

    fn finish_table(&mut self, table: Table, sink: &mut Sink) {
        if let Some(parent) = self.tables.last_mut().and_then(|t| t.cell.as_mut()) {
            for cell in table.rows.iter().flatten().filter(|cell| !cell.is_empty()) {
                if parent.len() < self.max_cell * 4 {
                    if !parent.is_empty() {
                        parent.push_str(" ; ");
                    }
                    parent.push_str(cell);
                }
            }
            return;
        }
        sink.blank();
        for row in table.rows.iter().take(self.max_rows) {
            if let Some(line) = table_row(row) {
                sink.line(&line);
            }
        }
        if table.rows.len() > self.max_rows {
            sink.truncated = true;
        }
        sink.blank();
    }
}

pub(crate) fn docx(mut pkg: Package<'_>, sink: &mut Sink) -> Result<Option<String>, OfficeError> {
    let xml = pkg.require("word/document.xml")?;
    let title = core_title(&mut pkg);
    let mut state = Docx {
        paras: Vec::new(),
        tables: Vec::new(),
        in_text: false,
        fallback: 0,
        first_heading: None,
        max_cell: pkg.limits.max_cell_chars,
        max_rows: pkg.limits.max_rows_per_sheet,
    };
    walk(&xml, pkg.format, |node| state.event(node, sink))?;
    Ok(title.or(state.first_heading))
}

// ---------------------------------------------------------------------------
// pptx
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Slide {
    title: Option<String>,
    body: Vec<String>,
}

struct Shape {
    placeholder: Option<String>,
    lines: Vec<String>,
}

#[derive(Default)]
struct SlideParser {
    notes: bool,
    slide: Slide,
    shape: Option<Shape>,
    para: Option<(String, bool)>,
    in_text: bool,
    cell: Option<String>,
    row: Vec<String>,
    rows: usize,
    max_cell: usize,
    max_rows: usize,
}

impl SlideParser {
    fn event(&mut self, node: Node<'_>) -> bool {
        match node {
            Node::Open(tag) => match tag.name.as_str() {
                "sp" => {
                    self.shape = Some(Shape {
                        placeholder: None,
                        lines: Vec::new(),
                    })
                }
                "ph" => {
                    if let Some(shape) = self.shape.as_mut() {
                        shape.placeholder = Some(tag.attr("type").unwrap_or("").to_string());
                    }
                }
                "p" => self.para = Some((String::new(), false)),
                "buChar" | "buAutoNum" => {
                    if let Some(para) = self.para.as_mut() {
                        para.1 = true;
                    }
                }
                "t" => self.in_text = true,
                "br" => {
                    if let Some(para) = self.para.as_mut() {
                        para.0.push(' ');
                    }
                }
                "tr" => self.row.clear(),
                "tc" => self.cell = Some(String::new()),
                _ => {}
            },
            Node::Text(text) => {
                if self.in_text {
                    if let Some(para) = self.para.as_mut() {
                        if para.0.len() < 1 << 20 {
                            para.0.push_str(text);
                        }
                    }
                }
            }
            Node::Close(name) => match name {
                "t" => self.in_text = false,
                "p" => {
                    if let Some((text, bullet)) = self.para.take() {
                        self.finish_para(&text, bullet);
                    }
                }
                "tc" => {
                    if let Some(cell) = self.cell.take() {
                        self.row.push(cap_chars(cell, self.max_cell));
                    }
                }
                "tr" => {
                    let row = std::mem::take(&mut self.row);
                    if self.rows < self.max_rows {
                        if let Some(line) = table_row(&row) {
                            self.slide.body.push(line);
                            self.rows += 1;
                        }
                    }
                }
                "sp" => {
                    if let Some(shape) = self.shape.take() {
                        self.finish_shape(shape);
                    }
                }
                _ => {}
            },
        }
        true
    }

    fn finish_para(&mut self, text: &str, bullet: bool) {
        let text = squash(text);
        if text.is_empty() {
            return;
        }
        if let Some(cell) = self.cell.as_mut() {
            if cell.len() < self.max_cell * 4 {
                if !cell.is_empty() {
                    cell.push(' ');
                }
                cell.push_str(&text);
            }
            return;
        }
        let line = if bullet { format!("- {text}") } else { text };
        match self.shape.as_mut() {
            Some(shape) => shape.lines.push(line),
            None => self.slide.body.push(line),
        }
    }

    fn finish_shape(&mut self, shape: Shape) {
        let kind = shape.placeholder.as_deref();
        if self.notes {
            if kind == Some("body") {
                self.slide.body.extend(shape.lines);
            }
            return;
        }
        match kind {
            Some("sldNum" | "dt" | "ftr" | "hdr") => {}
            Some("title" | "ctrTitle") if self.slide.title.is_none() => {
                let title = shape
                    .lines
                    .iter()
                    .map(|line| line.trim_start_matches("- "))
                    .collect::<Vec<_>>()
                    .join(" ");
                self.slide.title = Some(title);
            }
            _ => self.slide.body.extend(shape.lines),
        }
    }
}

fn parse_slide(xml: &[u8], notes: bool, pkg: &Package<'_>) -> Result<Slide, OfficeError> {
    let mut parser = SlideParser {
        notes,
        max_cell: pkg.limits.max_cell_chars,
        max_rows: pkg.limits.max_rows_per_sheet,
        ..Default::default()
    };
    walk(xml, pkg.format, |node| parser.event(node))?;
    Ok(parser.slide)
}

fn slide_paths(pkg: &mut Package<'_>) -> Result<Vec<String>, OfficeError> {
    let presentation = pkg.require("ppt/presentation.xml")?;
    let relations = rels(pkg, "ppt/presentation.xml")?;
    let mut paths = Vec::new();
    walk(&presentation, pkg.format, |node| {
        if let Node::Open(tag) = node {
            if tag.name == "sldId" {
                if let Some(rel) = tag
                    .attr("r:id")
                    .and_then(|id| relations.iter().find(|rel| rel.id == id))
                {
                    paths.push(resolve("ppt", &rel.target));
                }
            }
        }
        true
    })?;
    if paths.is_empty() {
        let mut numbered: Vec<(u32, String)> = pkg
            .names()
            .into_iter()
            .filter_map(|name| {
                let number = name
                    .strip_prefix("ppt/slides/slide")?
                    .strip_suffix(".xml")?
                    .parse()
                    .ok()?;
                Some((number, name))
            })
            .collect();
        numbered.sort();
        paths = numbered.into_iter().map(|(_, name)| name).collect();
    }
    Ok(paths)
}

pub(crate) fn pptx(mut pkg: Package<'_>, sink: &mut Sink) -> Result<Option<String>, OfficeError> {
    let paths = slide_paths(&mut pkg)?;
    let mut first_title = None;
    let max_pages = pkg.limits.max_pages;
    for (index, path) in paths.into_iter().enumerate() {
        if sink.full() {
            break;
        }
        if index >= max_pages {
            sink.truncated = true;
            break;
        }
        let Some(xml) = pkg.read(&path)? else {
            continue;
        };
        let slide = parse_slide(&xml, false, &pkg)?;
        let mut notes = Vec::new();
        let slide_rels = rels(&mut pkg, &path)?;
        if let Some(rel) = slide_rels
            .iter()
            .find(|rel| rel.kind.ends_with("/notesSlide"))
        {
            let notes_path = resolve(dir_of(&path), &rel.target);
            if let Some(xml) = pkg.read(&notes_path)? {
                notes = parse_slide(&xml, true, &pkg)?.body;
            }
        }
        let heading = slide
            .title
            .clone()
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| format!("Folie {}", index + 1));
        if first_title.is_none() {
            first_title = slide.title.clone().filter(|title| !title.is_empty());
        }
        sink.line(&format!("# {heading}"));
        sink.blank();
        for line in &slide.body {
            sink.line(line);
        }
        if !notes.is_empty() {
            sink.blank();
            sink.line("Notizen:");
            for line in &notes {
                sink.line(line);
            }
        }
        sink.end_page();
    }
    Ok(core_title(&mut pkg).or(first_title))
}

// ---------------------------------------------------------------------------
// xlsx
// ---------------------------------------------------------------------------

fn shared_strings(pkg: &mut Package<'_>) -> Result<Vec<String>, OfficeError> {
    let Some(xml) = pkg.read("xl/sharedStrings.xml")? else {
        return Ok(Vec::new());
    };
    let max_cell = pkg.limits.max_cell_chars;
    let mut strings = Vec::new();
    let mut current = String::new();
    let mut in_text = false;
    let mut phonetic = 0usize;
    walk(&xml, pkg.format, |node| {
        match node {
            Node::Open(tag) => match tag.name.as_str() {
                "si" => current.clear(),
                "rPh" => phonetic += 1,
                "t" if phonetic == 0 => in_text = true,
                _ => {}
            },
            Node::Text(text) => {
                if in_text && current.len() < max_cell * 4 {
                    current.push_str(text);
                }
            }
            Node::Close(name) => match name {
                "t" => in_text = false,
                "rPh" => phonetic = phonetic.saturating_sub(1),
                "si" => strings.push(cap_chars(squash(&current), max_cell)),
                _ => {}
            },
        }
        true
    })?;
    Ok(strings)
}

fn column_index(reference: &str) -> Option<usize> {
    let mut index = 0usize;
    let mut letters = 0;
    for c in reference.chars().take_while(char::is_ascii_alphabetic) {
        index = index
            .checked_mul(26)?
            .checked_add((c.to_ascii_uppercase() as u8 - b'A') as usize + 1)?;
        letters += 1;
    }
    (letters > 0).then(|| index - 1)
}

const MAX_COLUMNS: usize = 16_384;

struct SheetParser<'a> {
    shared: &'a [String],
    name: &'a str,
    row: Vec<String>,
    next_col: usize,
    col: usize,
    cell_type: String,
    value: String,
    inline: String,
    in_value: bool,
    in_inline: bool,
    in_cell: bool,
    started: bool,
    rows: usize,
    cells: usize,
    max_rows: usize,
    max_cells: usize,
    max_cell: usize,
    capped: bool,
}

impl SheetParser<'_> {
    fn event(&mut self, node: Node<'_>, sink: &mut Sink) -> bool {
        match node {
            Node::Open(tag) => match tag.name.as_str() {
                "row" => {
                    self.row.clear();
                    self.next_col = 0;
                }
                "c" => {
                    self.col = tag
                        .attr("r")
                        .and_then(column_index)
                        .unwrap_or(self.next_col);
                    self.cell_type = tag.attr("t").unwrap_or("").to_string();
                    self.value.clear();
                    self.inline.clear();
                    self.in_cell = true;
                }
                "v" if self.in_cell => self.in_value = true,
                "is" if self.in_cell => self.in_inline = true,
                _ => {}
            },
            Node::Text(text) => {
                if self.in_value && self.value.len() < self.max_cell * 4 {
                    self.value.push_str(text);
                } else if self.in_inline && self.inline.len() < self.max_cell * 4 {
                    self.inline.push_str(text);
                }
            }
            Node::Close(name) => match name {
                "v" => self.in_value = false,
                "is" => self.in_inline = false,
                "c" => self.finish_cell(),
                "row" => return self.finish_row(sink),
                _ => {}
            },
        }
        true
    }

    fn finish_cell(&mut self) {
        self.in_cell = false;
        let raw = match self.cell_type.as_str() {
            "s" => self
                .value
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|index| self.shared.get(index))
                .cloned()
                .unwrap_or_default(),
            "inlineStr" => self.inline.clone(),
            "b" => (if self.value.trim() == "1" {
                "TRUE"
            } else {
                "FALSE"
            })
            .to_string(),
            "e" => String::new(),
            _ => self.value.trim().to_string(),
        };
        let value = cap_chars(squash(&raw), self.max_cell);
        self.next_col = self.col + 1;
        if value.is_empty() || self.col >= MAX_COLUMNS {
            return;
        }
        self.cells += 1;
        if self.row.len() <= self.col {
            self.row.resize(self.col + 1, String::new());
        }
        self.row[self.col] = value;
    }

    fn finish_row(&mut self, sink: &mut Sink) -> bool {
        if let Some(line) = table_row(&self.row) {
            if !self.started {
                self.started = true;
                sink.blank();
                sink.line(&format!("## {}", squash(self.name)));
                sink.blank();
            }
            sink.line(&line);
            self.rows += 1;
        }
        if self.rows >= self.max_rows || self.cells >= self.max_cells {
            self.capped = true;
            return false;
        }
        !sink.full()
    }
}

pub(crate) fn xlsx(mut pkg: Package<'_>, sink: &mut Sink) -> Result<Option<String>, OfficeError> {
    let workbook = pkg.require("xl/workbook.xml")?;
    let relations = rels(&mut pkg, "xl/workbook.xml")?;
    let mut sheets: Vec<(String, String)> = Vec::new();
    walk(&workbook, pkg.format, |node| {
        if let Node::Open(tag) = node {
            if tag.name == "sheet" && tag.attr("state") != Some("veryHidden") {
                sheets.push((
                    tag.attr("name").unwrap_or("Tabelle").to_string(),
                    tag.attr("r:id").unwrap_or_default().to_string(),
                ));
            }
        }
        true
    })?;
    let shared = shared_strings(&mut pkg)?;
    for (index, (name, id)) in sheets.iter().enumerate() {
        if sink.full() {
            break;
        }
        if index >= pkg.limits.max_pages {
            sink.truncated = true;
            break;
        }
        let Some(rel) = relations.iter().find(|rel| &rel.id == id) else {
            continue;
        };
        let Some(xml) = pkg.read(&resolve("xl", &rel.target))? else {
            continue;
        };
        let mut parser = SheetParser {
            shared: &shared,
            name,
            row: Vec::new(),
            next_col: 0,
            col: 0,
            cell_type: String::new(),
            value: String::new(),
            inline: String::new(),
            in_value: false,
            in_inline: false,
            in_cell: false,
            started: false,
            rows: 0,
            cells: 0,
            max_rows: pkg.limits.max_rows_per_sheet,
            max_cells: pkg.limits.max_cells_per_sheet,
            max_cell: pkg.limits.max_cell_chars,
            capped: false,
        };
        walk(&xml, pkg.format, |node| parser.event(node, sink))?;
        if parser.capped {
            sink.truncated = true;
        }
        sink.end_page();
    }
    Ok(core_title(&mut pkg))
}
