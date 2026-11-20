#![cfg(feature = "office")]

use three_dcf_core::office::{extract, OfficeFormat};

/// A real BIFF8 workbook (generated with LibreOffice from a three-line CSV).
#[test]
fn xls_fixture_is_read_as_a_table() {
    let bytes = std::fs::read("tests/fixtures/small.xls").unwrap();
    let text = extract(&bytes, OfficeFormat::Xls).unwrap();
    assert_eq!(text.pages.len(), 1);
    let page = &text.pages[0];
    assert!(page.starts_with("## "), "{page}");
    assert!(page.contains("| Name | Betrag | Ort |"), "{page}");
    assert!(page.contains("| Müller | 12.5 | Köln |"), "{page}");
    assert!(page.contains("| Schmidt | 7 | Berlin |"), "{page}");
}

/// Manual check against real files: `OFFICE_FIXTURE_DIR=/dir cargo test --features office -- --nocapture`.
#[test]
fn files_from_a_directory_are_readable() {
    let Ok(dir) = std::env::var("OFFICE_FIXTURE_DIR") else {
        return;
    };
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Some(format) = OfficeFormat::from_extension(ext) else {
            continue;
        };
        let text = extract(&std::fs::read(&path).unwrap(), format)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        println!(
            "== {} (title {:?})\n{}",
            path.display(),
            text.title,
            text.to_text()
        );
    }
}
