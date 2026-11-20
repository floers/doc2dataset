use std::io::Write;

use super::*;

fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in files {
        writer.start_file(*name, options).unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

fn docx() -> Vec<u8> {
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document {W}><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Jahresbericht</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">Hallo </w:t></w:r><w:r><w:t>Welt &amp; Co</w:t></w:r><w:r><w:tab/><w:t>nach Tab</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:t>Unterpunkt</w:t></w:r></w:p>
<w:tbl>
<w:tr><w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Wert</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>Alpha</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>1</w:t></w:r></w:p><w:p><w:r><w:t>2</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:delText>gelöscht</w:delText></w:r><w:r><w:t>Schluss</w:t></w:r></w:p>
</w:body></w:document>"#
    );
    zip_of(&[("word/document.xml", &body)])
}

#[test]
fn docx_headings_lists_tables() {
    let text = extract(&docx(), OfficeFormat::Docx).unwrap();
    assert_eq!(text.title.as_deref(), Some("Jahresbericht"));
    assert_eq!(text.pages.len(), 1);
    let page = &text.pages[0];
    assert!(page.starts_with("# Jahresbericht\n"), "{page}");
    assert!(page.contains("Hallo Welt & Co nach Tab"), "{page}");
    assert!(page.contains("  - Unterpunkt"), "{page}");
    assert!(page.contains("| Name | Wert |"), "{page}");
    assert!(page.contains("| Alpha | 1 2 |"), "{page}");
    assert!(
        page.contains("Schluss") && !page.contains("gelöscht"),
        "{page}"
    );
}

#[test]
fn docx_prefers_metadata_title() {
    let core = r#"<cp:coreProperties xmlns:cp="a" xmlns:dc="b"><dc:title>Vertrag Mustermann</dc:title></cp:coreProperties>"#;
    let document = format!(
        r#"<w:document {W}><w:body><w:p><w:r><w:t>x</w:t></w:r></w:p></w:body></w:document>"#
    );
    let bytes = zip_of(&[
        ("word/document.xml", &document),
        ("docProps/core.xml", core),
    ]);
    let text = extract(&bytes, OfficeFormat::Docx).unwrap();
    assert_eq!(text.title.as_deref(), Some("Vertrag Mustermann"));
}

fn xlsx() -> Vec<u8> {
    let workbook = r#"<workbook xmlns:r="r"><sheets><sheet name="Umsatz &amp; Kosten" sheetId="1" r:id="rId1"/><sheet name="Leer" sheetId="2" r:id="rId2"/><sheet name="Geheim" sheetId="3" state="veryHidden" r:id="rId3"/></sheets></workbook>"#;
    let rels = r#"<Relationships><Relationship Id="rId1" Type="x/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="x/worksheet" Target="/xl/worksheets/sheet2.xml"/></Relationships>"#;
    let shared = r#"<sst><si><t>Monat</t></si><si><r><t>Ja</t></r><r><t>nuar</t></r><rPh><t>X</t></rPh></si></sst>"#;
    let sheet1 = r#"<worksheet><sheetData>
<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="inlineStr"><is><t>Betrag</t></is></c></row>
<row r="2"><c r="A2" t="s"><v>1</v></c><c r="C2"><v>12.5</v></c></row>
<row r="3"><c r="A3" t="b"><v>1</v></c></row>
<row r="4"></row>
</sheetData></worksheet>"#;
    let sheet2 = r#"<worksheet><sheetData/></worksheet>"#;
    zip_of(&[
        ("xl/workbook.xml", workbook),
        ("xl/_rels/workbook.xml.rels", rels),
        ("xl/sharedStrings.xml", shared),
        ("xl/worksheets/sheet1.xml", sheet1),
        ("xl/worksheets/sheet2.xml", sheet2),
    ])
}

#[test]
fn xlsx_sheets_as_tables() {
    let text = extract(&xlsx(), OfficeFormat::Xlsx).unwrap();
    assert_eq!(text.pages.len(), 1, "{:?}", text.pages);
    let page = &text.pages[0];
    assert!(page.starts_with("## Umsatz & Kosten\n"), "{page}");
    assert!(page.contains("| Monat | Betrag |"), "{page}");
    assert!(page.contains("| Januar |  | 12.5 |"), "{page}");
    assert!(page.contains("| TRUE |"), "{page}");
    assert!(!text.truncated);
}

#[test]
fn xlsx_row_cap_truncates() {
    let mut rows = String::new();
    for r in 1..=50 {
        rows.push_str(&format!(r#"<row r="{r}"><c r="A{r}"><v>{r}</v></c></row>"#));
    }
    let bytes = zip_of(&[
        (
            "xl/workbook.xml",
            r#"<workbook xmlns:r="r"><sheets><sheet name="S" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships><Relationship Id="rId1" Type="t" Target="worksheets/s.xml"/></Relationships>"#,
        ),
        (
            "xl/worksheets/s.xml",
            &format!("<worksheet><sheetData>{rows}</sheetData></worksheet>"),
        ),
    ]);
    let limits = OfficeLimits {
        max_rows_per_sheet: 10,
        ..Default::default()
    };
    let text = extract_with_limits(&bytes, OfficeFormat::Xlsx, &limits).unwrap();
    assert!(text.truncated);
    assert_eq!(
        text.pages[0]
            .lines()
            .filter(|l| l.starts_with("| "))
            .count(),
        10
    );
}

fn pptx() -> Vec<u8> {
    let presentation = r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId1"/></p:sldIdLst></p:presentation>"#;
    let rels = r#"<Relationships><Relationship Id="rId1" Type="x/slide" Target="slides/slide1.xml"/><Relationship Id="rId2" Type="x/slide" Target="slides/slide2.xml"/></Relationships>"#;
    let slide2 = r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="ctrTitle"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Quartalsziele</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:pPr><a:buChar char="x"/></a:pPr><a:r><a:t>Umsatz steigern</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="sldNum"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>7</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:sld>"#;
    let slide2_rels = r#"<Relationships><Relationship Id="rId1" Type="x/notesSlide" Target="../notesSlides/notesSlide1.xml"/></Relationships>"#;
    let notes = r#"<p:notes xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Sprechernotiz</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:notes>"#;
    let slide1 = r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:t>Nur Text</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:sld>"#;
    zip_of(&[
        ("ppt/presentation.xml", presentation),
        ("ppt/_rels/presentation.xml.rels", rels),
        ("ppt/slides/slide1.xml", slide1),
        ("ppt/slides/slide2.xml", slide2),
        ("ppt/slides/_rels/slide2.xml.rels", slide2_rels),
        ("ppt/notesSlides/notesSlide1.xml", notes),
    ])
}

#[test]
fn pptx_slides_in_order_with_titles_and_notes() {
    let text = extract(&pptx(), OfficeFormat::Pptx).unwrap();
    assert_eq!(text.pages.len(), 2);
    assert!(
        text.pages[0].starts_with("# Quartalsziele\n"),
        "{}",
        text.pages[0]
    );
    assert!(text.pages[0].contains("- Umsatz steigern"));
    assert!(text.pages[0].contains("Notizen:\nSprechernotiz"));
    assert!(!text.pages[0].contains('7'));
    assert!(
        text.pages[1].starts_with("# Folie 2\n"),
        "{}",
        text.pages[1]
    );
    assert!(text.pages[1].contains("Nur Text"));
    assert_eq!(text.title.as_deref(), Some("Quartalsziele"));
}

const ODF_NS: &str =
    r#"xmlns:office="o" xmlns:text="t" xmlns:table="tb" xmlns:draw="d" xmlns:presentation="p""#;

#[test]
fn odt_headings_lists_tables_and_skips_notes() {
    let content = format!(
        r#"<office:document-content {ODF_NS}><office:body><office:text>
<text:h text:outline-level="1">Konzept</text:h>
<text:p>Erster<text:s text:c="2"/>Absatz<text:note><text:note-body><text:p>Fußnote</text:p></text:note-body></text:note>.</text:p>
<text:list><text:list-item><text:p>Punkt A</text:p></text:list-item></text:list>
<table:table table:name="T1"><table:table-row><table:table-cell><text:p>a</text:p></table:table-cell><table:table-cell><text:p>b</text:p></table:table-cell></table:table-row></table:table>
</office:text></office:body></office:document-content>"#
    );
    let meta = r#"<office:document-meta xmlns:office="o" xmlns:dc="dc"><office:meta><dc:title>Mein Konzept</dc:title></office:meta></office:document-meta>"#;
    let bytes = zip_of(&[("content.xml", &content), ("meta.xml", meta)]);
    let text = extract(&bytes, OfficeFormat::Odt).unwrap();
    assert_eq!(text.title.as_deref(), Some("Mein Konzept"));
    let page = &text.pages[0];
    assert!(page.starts_with("# Konzept\n"), "{page}");
    assert!(page.contains("Erster Absatz."), "{page}");
    assert!(!page.contains("Fußnote"), "{page}");
    assert!(page.contains("- Punkt A"), "{page}");
    assert!(page.contains("| a | b |"), "{page}");
}

#[test]
fn ods_sheets_with_repeats() {
    let content = format!(
        r#"<office:document-content {ODF_NS}><office:body><office:spreadsheet>
<table:table table:name="Daten">
<table:table-row><table:table-cell><text:p>Name</text:p></table:table-cell><table:table-cell table:number-columns-repeated="1000"/></table:table-row>
<table:table-row><table:table-cell/><table:table-cell><text:p>x</text:p></table:table-cell></table:table-row>
<table:table-row table:number-rows-repeated="1048000"><table:table-cell table:number-columns-repeated="16000"/></table:table-row>
</table:table>
<table:table table:name="Zwei"><table:table-row><table:table-cell><text:p>y</text:p></table:table-cell></table:table-row></table:table>
</office:spreadsheet></office:body></office:document-content>"#
    );
    let bytes = zip_of(&[("content.xml", &content)]);
    let text = extract(&bytes, OfficeFormat::Ods).unwrap();
    assert_eq!(text.pages.len(), 2);
    assert_eq!(text.pages[0], "## Daten\n\n| Name |\n|  | x |");
    assert_eq!(text.pages[1], "## Zwei\n\n| y |");
}

#[test]
fn odp_slides() {
    let content = format!(
        r#"<office:document-content {ODF_NS}><office:body><office:presentation>
<draw:page draw:name="page1">
<draw:frame presentation:class="title"><draw:text-box><text:p>Agenda</text:p></draw:text-box></draw:frame>
<draw:frame presentation:class="outline"><draw:text-box><text:list><text:list-item><text:p>Punkt</text:p></text:list-item></text:list></draw:text-box></draw:frame>
<presentation:notes><draw:frame presentation:class="notes"><draw:text-box><text:p>Notiz</text:p></draw:text-box></draw:frame></presentation:notes>
</draw:page>
<draw:page draw:name="page2"><draw:frame><draw:text-box><text:p>Rest</text:p></draw:text-box></draw:frame></draw:page>
</office:presentation></office:body></office:document-content>"#
    );
    let bytes = zip_of(&[("content.xml", &content)]);
    let text = extract(&bytes, OfficeFormat::Odp).unwrap();
    assert_eq!(text.pages.len(), 2);
    assert_eq!(text.pages[0], "# Agenda\n\n- Punkt\n\nNotizen:\nNotiz");
    assert_eq!(text.pages[1], "# page2\n\nRest");
}

#[test]
fn encrypted_odf_is_reported() {
    let bytes = zip_of(&[
        (
            "META-INF/manifest.xml",
            "<manifest><encryption-data/></manifest>",
        ),
        ("content.xml", "<x/>"),
    ]);
    assert!(matches!(
        extract(&bytes, OfficeFormat::Odt),
        Err(OfficeError::Encrypted)
    ));
}

#[test]
fn rtf_text_escapes_and_skipped_groups() {
    let rtf = br"{\rtf1\ansi\ansicpg1252{\fonttbl{\f0 Arial;}}{\*\generator Foo;}{\info{\title Geheim}}\pard Gr\'fc\'dfe \u8364? und \u228\'e4 {\b fett}\par
Zweite Zeile\line mit Umbruch\par
\trowd Zelle1\cell Zelle2\cell\row
{\pict\bin3 abc}Ende\\ \{x\}\par}";
    let text = extract(rtf, OfficeFormat::Rtf).unwrap();
    let page = &text.pages[0];
    assert!(page.contains("Grüße € und ä fett"), "{page}");
    assert!(page.contains("Zweite Zeile mit Umbruch"), "{page}");
    assert!(page.contains("Zelle1 | Zelle2"), "{page}");
    assert!(page.contains("Ende\\ {x}"), "{page}");
    assert!(
        !page.contains("Arial") && !page.contains("Geheim") && !page.contains("Foo"),
        "{page}"
    );
}

#[test]
fn rtf_requires_header_and_bounds_nesting() {
    assert!(matches!(
        extract(b"plain text", OfficeFormat::Rtf),
        Err(OfficeError::Malformed { .. })
    ));
    let deep = format!("{{\\rtf1 {}", "{".repeat(1000));
    assert!(matches!(
        extract(deep.as_bytes(), OfficeFormat::Rtf),
        Err(OfficeError::LimitExceeded(_))
    ));
}

#[test]
fn zip_bombs_and_huge_entries_are_refused() {
    let big = "<w:p>".repeat(200_000);
    let bytes = zip_of(&[("word/document.xml", &big)]);
    let limits = OfficeLimits {
        max_part_bytes: 100_000,
        ..Default::default()
    };
    assert!(matches!(
        extract_with_limits(&bytes, OfficeFormat::Docx, &limits),
        Err(OfficeError::LimitExceeded(_))
    ));
    let limits = OfficeLimits {
        max_total_bytes: 100,
        ..Default::default()
    };
    assert!(matches!(
        extract_with_limits(&docx(), OfficeFormat::Docx, &limits),
        Err(OfficeError::LimitExceeded(_))
    ));
    let limits = OfficeLimits {
        max_entries: 1,
        ..Default::default()
    };
    assert!(matches!(
        extract_with_limits(&xlsx(), OfficeFormat::Xlsx, &limits),
        Err(OfficeError::LimitExceeded(_))
    ));
    let limits = OfficeLimits {
        max_input_bytes: 10,
        ..Default::default()
    };
    assert!(matches!(
        extract_with_limits(&docx(), OfficeFormat::Docx, &limits),
        Err(OfficeError::LimitExceeded(_))
    ));
}

#[test]
fn output_budget_truncates() {
    let paragraphs: String = (0..2000)
        .map(|n| format!("<w:p><w:r><w:t>Absatz Nummer {n}</w:t></w:r></w:p>"))
        .collect();
    let document = format!(r#"<w:document {W}><w:body>{paragraphs}</w:body></w:document>"#);
    let bytes = zip_of(&[("word/document.xml", &document)]);
    let limits = OfficeLimits {
        max_output_bytes: 1000,
        ..Default::default()
    };
    let text = extract_with_limits(&bytes, OfficeFormat::Docx, &limits).unwrap();
    assert!(text.truncated);
    assert!(text.pages[0].len() <= 1000);
}

#[test]
fn malformed_and_foreign_input_gives_typed_errors() {
    for format in [
        OfficeFormat::Docx,
        OfficeFormat::Xlsx,
        OfficeFormat::Pptx,
        OfficeFormat::Odt,
        OfficeFormat::Ods,
        OfficeFormat::Odp,
        OfficeFormat::Xls,
        OfficeFormat::Rtf,
    ] {
        for input in [&b""[..], b"garbage", &[0u8; 64][..], b"PK\x03\x04truncated"] {
            assert!(extract(input, format).is_err(), "{format:?} {input:?}");
        }
    }
    // Password-protected OOXML is an OLE compound file.
    let ole = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0, 0, 0];
    assert!(matches!(
        extract(&ole, OfficeFormat::Docx),
        Err(OfficeError::Encrypted)
    ));
    // A zip without the main part, and broken XML.
    let empty = zip_of(&[("other.txt", "x")]);
    assert!(matches!(
        extract(&empty, OfficeFormat::Docx),
        Err(OfficeError::Malformed { .. })
    ));
    let broken = zip_of(&[("word/document.xml", "<w:document><w:p><w:t>unclosed &")]);
    let _ = extract(&broken, OfficeFormat::Docx);
}

#[test]
fn deeply_nested_xml_is_refused() {
    let document = "<w:p>".repeat(5000);
    let bytes = zip_of(&[("word/document.xml", &document)]);
    assert!(matches!(
        extract(&bytes, OfficeFormat::Docx),
        Err(OfficeError::LimitExceeded(_))
    ));
}

#[test]
fn xls_named_file_that_is_xlsx_is_read() {
    let text = extract(&xlsx(), OfficeFormat::Xls).unwrap();
    assert!(text.pages[0].contains("| Monat | Betrag |"));
}

#[test]
fn extensions_map_to_formats() {
    assert_eq!(
        OfficeFormat::from_extension("DOCX"),
        Some(OfficeFormat::Docx)
    );
    assert_eq!(OfficeFormat::from_extension("rtf"), Some(OfficeFormat::Rtf));
    assert_eq!(OfficeFormat::from_extension("doc"), None);
    assert!(OfficeFormat::is_legacy_binary_extension("PPT"));
}

#[test]
fn encode_input_feeds_the_encoder_and_chunker() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bericht.docx");
    std::fs::write(&path, docx()).unwrap();
    let encoder = crate::EncoderBuilder::new("custom").unwrap().build();
    let input = crate::EncodeInput::from_path(&path, encoder.config()).unwrap();
    let (document, _) = encoder.encode(input).unwrap();
    let chunker = crate::Chunker::new(crate::ChunkConfig {
        mode: crate::ChunkMode::Cells,
        cells_per_chunk: 80,
        overlap_cells: 4,
        max_tokens: 512,
        overlap_tokens: 64,
    });
    let chunks = chunker.chunk_document(&document, "doc://1");
    let joined: String = chunks
        .iter()
        .map(|c| c.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("Jahresbericht"), "{joined}");
    assert!(joined.contains("Alpha"), "{joined}");

    let legacy = dir.path().join("alt.doc");
    std::fs::write(&legacy, b"\xD0\xCF\x11\xE0").unwrap();
    let error = crate::EncodeInput::from_path(&legacy, encoder.config()).unwrap_err();
    assert!(
        matches!(error, crate::DcfError::Office(OfficeError::LegacyBinary)),
        "{error}"
    );
}
