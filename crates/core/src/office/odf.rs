//! OpenDocument: odt, ods, odp.

use super::xml::{walk, Node};
use super::{squash, table_row, OfficeError, OfficeFormat, Package, Sink};

const MAX_REPEAT_ROWS: usize = 20;
const MAX_REPEAT_CELLS: usize = 100;
const MAX_EMPTY_RUN: usize = 1024;

struct Block {
    heading: Option<usize>,
    text: String,
}

#[derive(Default)]
struct Table {
    name: String,
    row: Vec<String>,
    pending_empty: usize,
    row_repeat: usize,
    cell: Option<String>,
    cell_repeat: usize,
    rows: usize,
    cells: usize,
    started: bool,
    capped: bool,
}

#[derive(Default)]
struct Slide {
    name: String,
    title: Option<String>,
    body: Vec<String>,
    notes: Vec<String>,
}

struct Odf {
    format: OfficeFormat,
    skip: usize,
    blocks: Vec<Block>,
    list_depth: usize,
    tables: Vec<Table>,
    frames: Vec<Option<String>>,
    slide: Option<Slide>,
    in_notes: bool,
    slides: usize,
    sheets: usize,
    first_heading: Option<String>,
    max_pages: usize,
    max_rows: usize,
    max_cells: usize,
    max_cell: usize,
}

fn repeat(value: Option<&str>) -> usize {
    value.and_then(|v| v.parse().ok()).unwrap_or(1usize).max(1)
}

impl Odf {
    fn event(&mut self, node: Node<'_>, sink: &mut Sink) -> bool {
        if self.skip > 0 {
            match node {
                Node::Open(_) => self.skip += 1,
                Node::Close(_) => self.skip -= 1,
                Node::Text(_) => {}
            }
            return true;
        }
        match node {
            Node::Open(tag) => match tag.name.as_str() {
                "note" | "annotation" | "tracked-changes" => self.skip = 1,
                "p" => self.blocks.push(Block {
                    heading: None,
                    text: String::new(),
                }),
                "h" => self.blocks.push(Block {
                    heading: Some(
                        tag.attr("outline-level")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1usize)
                            .clamp(1, 6),
                    ),
                    text: String::new(),
                }),
                "list-item" => self.list_depth += 1,
                "s" => {
                    if let Some(block) = self.blocks.last_mut() {
                        let count = repeat(tag.attr("c")).min(50);
                        block.text.push_str(&" ".repeat(count));
                    }
                }
                "tab" | "line-break" => {
                    if let Some(block) = self.blocks.last_mut() {
                        block.text.push(' ');
                    }
                }
                "table" => {
                    let top = self.tables.is_empty();
                    self.tables.push(Table {
                        name: tag.attr("name").unwrap_or("Tabelle").to_string(),
                        ..Default::default()
                    });
                    if top && self.format == OfficeFormat::Odt {
                        sink.blank();
                    }
                    if top && self.format == OfficeFormat::Ods {
                        if self.sheets >= self.max_pages {
                            sink.truncated = true;
                            return false;
                        }
                        self.sheets += 1;
                    }
                }
                "table-row" => {
                    if let Some(table) = self.tables.last_mut() {
                        table.row.clear();
                        table.pending_empty = 0;
                        table.row_repeat = repeat(tag.attr("number-rows-repeated"));
                    }
                }
                "table-cell" => {
                    if let Some(table) = self.tables.last_mut() {
                        table.cell = Some(String::new());
                        table.cell_repeat = repeat(tag.attr("number-columns-repeated"));
                    }
                }
                "covered-table-cell" => {
                    if let Some(table) = self.tables.last_mut() {
                        table.cell = Some(String::new());
                        table.cell_repeat = 1;
                    }
                }
                "frame" | "custom-shape" => {
                    self.frames.push(tag.attr("class").map(str::to_string));
                }
                "notes" if self.format == OfficeFormat::Odp => self.in_notes = true,
                "page" if self.format == OfficeFormat::Odp => {
                    self.slide = Some(Slide {
                        name: tag.attr("name").unwrap_or_default().to_string(),
                        ..Default::default()
                    });
                }
                _ => {}
            },
            Node::Text(text) => {
                if let Some(block) = self.blocks.last_mut() {
                    if block.text.len() < 1 << 20 {
                        block.text.push_str(text);
                    }
                }
            }
            Node::Close(name) => match name {
                "p" | "h" => {
                    if let Some(block) = self.blocks.pop() {
                        self.emit(block, sink);
                    }
                }
                "list-item" => self.list_depth = self.list_depth.saturating_sub(1),
                "table-cell" | "covered-table-cell" => self.finish_cell(),
                "table-row" => self.finish_row(sink),
                "table" => self.finish_table(sink),
                "frame" | "custom-shape" => {
                    self.frames.pop();
                }
                "notes" => self.in_notes = false,
                "page" if self.format == OfficeFormat::Odp => self.finish_slide(sink),
                _ => {}
            },
        }
        !sink.full()
    }

    fn emit(&mut self, block: Block, sink: &mut Sink) {
        let text = squash(&block.text);
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
        let list = (self.list_depth > 0 && block.heading.is_none()).then(|| self.list_depth - 1);
        if let Some(level) = block.heading {
            if level == 1 && self.first_heading.is_none() {
                self.first_heading = Some(text.clone());
            }
        }
        if self.format == OfficeFormat::Odp {
            let class = self.frames.last().and_then(|class| class.as_deref());
            let in_notes = self.in_notes;
            let Some(slide) = self.slide.as_mut() else {
                return;
            };
            if in_notes {
                if class == Some("notes") {
                    slide.notes.push(text);
                }
            } else if class == Some("title") && slide.title.is_none() {
                slide.title = Some(text);
            } else if matches!(
                class,
                Some("page-number" | "date-time" | "footer" | "header")
            ) {
            } else if list.is_some() {
                slide.body.push(format!("- {text}"));
            } else {
                slide.body.push(text);
            }
            return;
        }
        if self.format == OfficeFormat::Ods {
            // Text outside of cells in a spreadsheet (shapes, notes) is not part of the data.
            return;
        }
        match (block.heading, list) {
            (Some(level), _) => {
                sink.blank();
                sink.line(&format!("{} {text}", "#".repeat(level)));
                sink.blank();
            }
            (None, Some(level)) => sink.line(&format!("{}- {text}", "  ".repeat(level.min(8)))),
            (None, None) => {
                sink.line(&text);
                sink.blank();
            }
        }
    }

    fn finish_cell(&mut self) {
        let max_cell = self.max_cell;
        let Some(table) = self.tables.last_mut() else {
            return;
        };
        let Some(cell) = table.cell.take() else {
            return;
        };
        let cell: String = cell.chars().take(max_cell).collect();
        if cell.is_empty() {
            table.pending_empty = (table.pending_empty + table.cell_repeat).min(MAX_EMPTY_RUN);
            return;
        }
        for _ in 0..table.pending_empty {
            table.row.push(String::new());
        }
        table.pending_empty = 0;
        for _ in 0..table.cell_repeat.min(MAX_REPEAT_CELLS) {
            table.row.push(cell.clone());
            table.cells += 1;
        }
    }

    fn finish_row(&mut self, sink: &mut Sink) {
        let depth = self.tables.len();
        let Some(table) = self.tables.last_mut() else {
            return;
        };
        let row = std::mem::take(&mut table.row);
        let repeats = table.row_repeat.clamp(1, MAX_REPEAT_ROWS);
        let Some(line) = table_row(&row) else {
            return;
        };
        if table.capped {
            return;
        }
        if depth >= 2 {
            let flat = row
                .iter()
                .filter(|cell| !cell.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" ; ");
            if let Some(parent) = self.tables[depth - 2].cell.as_mut() {
                if parent.len() < self.max_cell * 4 {
                    if !parent.is_empty() {
                        parent.push_str(" ; ");
                    }
                    parent.push_str(&flat);
                }
            }
            return;
        }
        let name = table.name.clone();
        for _ in 0..repeats {
            let table = &mut self.tables[0];
            match self.format {
                OfficeFormat::Ods => {
                    if !table.started {
                        table.started = true;
                        sink.blank();
                        sink.line(&format!("## {}", squash(&name)));
                        sink.blank();
                    }
                    sink.line(&line);
                }
                OfficeFormat::Odp => {
                    if let Some(slide) = self.slide.as_mut() {
                        slide.body.push(line.clone());
                    }
                }
                _ => sink.line(&line),
            }
            table.rows += 1;
            if table.rows >= self.max_rows || table.cells >= self.max_cells {
                table.capped = true;
                sink.truncated = true;
                break;
            }
        }
    }

    fn finish_table(&mut self, sink: &mut Sink) {
        self.tables.pop();
        if self.tables.is_empty() {
            match self.format {
                OfficeFormat::Ods => sink.end_page(),
                OfficeFormat::Odt => sink.blank(),
                _ => {}
            }
        }
    }

    fn finish_slide(&mut self, sink: &mut Sink) {
        let Some(slide) = self.slide.take() else {
            return;
        };
        self.slides += 1;
        if self.slides > self.max_pages {
            sink.truncated = true;
            return;
        }
        let heading = slide
            .title
            .or_else(|| (!slide.name.is_empty()).then(|| slide.name.clone()))
            .unwrap_or_else(|| format!("Folie {}", self.slides));
        sink.line(&format!("# {heading}"));
        sink.blank();
        for line in &slide.body {
            sink.line(line);
        }
        if !slide.notes.is_empty() {
            sink.blank();
            sink.line("Notizen:");
            for line in &slide.notes {
                sink.line(line);
            }
        }
        sink.end_page();
    }
}

fn meta_title(pkg: &mut Package<'_>) -> Option<String> {
    let xml = pkg.read("meta.xml").ok().flatten()?;
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

pub(crate) fn extract(
    mut pkg: Package<'_>,
    sink: &mut Sink,
) -> Result<Option<String>, OfficeError> {
    if let Some(manifest) = pkg.read("META-INF/manifest.xml")? {
        if manifest
            .windows(b"encryption-data".len())
            .any(|window| window == b"encryption-data")
        {
            return Err(OfficeError::Encrypted);
        }
    }
    let content = pkg.require("content.xml")?;
    let title = meta_title(&mut pkg);
    let mut state = Odf {
        format: pkg.format,
        skip: 0,
        blocks: Vec::new(),
        list_depth: 0,
        tables: Vec::new(),
        frames: Vec::new(),
        slide: None,
        in_notes: false,
        slides: 0,
        sheets: 0,
        first_heading: None,
        max_pages: pkg.limits.max_pages,
        max_rows: pkg.limits.max_rows_per_sheet,
        max_cells: pkg.limits.max_cells_per_sheet,
        max_cell: pkg.limits.max_cell_chars,
    };
    walk(&content, pkg.format, |node| state.event(node, sink))?;
    Ok(title.or(state.first_heading))
}
