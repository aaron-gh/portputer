//! Checks the clipboard code against the real clipboard, then puts back
//! what was there.
//!   cargo run -p portputer-core --example clipboard

use portputer_core::clipboard::{self, Clip};

fn main() {
    let original = clipboard::read();
    let before = clipboard::change_count();
    clipboard::write_text("Portputer clipboard test").unwrap();
    assert!(clipboard::change_count() != before, "change count moves");
    assert_eq!(clipboard::read(), Some(Clip::Text("Portputer clipboard test".into())));
    let file = std::env::temp_dir().join("portputer clipboard test.txt");
    std::fs::write(&file, "test").unwrap();
    clipboard::write_files(&[file.as_path()]).unwrap();
    let read = clipboard::read();
    let canonical = file.canonicalize().unwrap();
    match &read {
        Some(Clip::Files(f)) => assert_eq!(f[0].canonicalize().unwrap(), canonical),
        other => panic!("files read back as {other:?}"),
    }
    match original {
        Some(Clip::Text(t)) => clipboard::write_text(&t).unwrap(),
        Some(Clip::Files(f)) => clipboard::write_files(&f.iter().map(|p| p.as_path()).collect::<Vec<_>>()).unwrap(),
        None => clipboard::write_text("").unwrap(),
    }
    std::fs::remove_file(file).ok();
    println!("clipboard OK");
}
