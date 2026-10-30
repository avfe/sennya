//! Locating the configured monospace font on disk and installing it into egui.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

pub const BOLD_FAMILY: &str = "term-bold";

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join("Library/Fonts"));
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
    }
    for d in [
        "/Library/Fonts",
        "/System/Library/Fonts",
        "/System/Library/Fonts/Supplemental",
        "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts",
        "/usr/share/fonts",
        "/usr/local/share/fonts",
    ] {
        dirs.push(PathBuf::from(d));
    }
    dirs
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                collect(&path, depth - 1, out);
            }
        } else if matches!(
            path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
            Some("ttf" | "otf" | "ttc")
        ) {
            out.push(path);
        }
    }
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// (file, face index) of the regular and bold faces of `family`.
fn find_family(family: &str, files: &[PathBuf]) -> Option<((PathBuf, u32), Option<(PathBuf, u32)>)> {
    let fam = norm(family);
    if fam.is_empty() {
        return None;
    }
    let mut regular = None;
    let mut bold = None;
    for path in files {
        let stem = norm(path.file_stem()?.to_str()?);
        let Some(rest) = stem.strip_prefix(&fam) else { continue };
        let is_ttc = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ttc"));
        match rest {
            "" if is_ttc => {
                // Collections like Menlo.ttc: face 0 regular, face 1 bold.
                regular = Some((path.clone(), 0));
                bold = Some((path.clone(), 1));
            }
            "" | "regular" | "book" => regular = Some((path.clone(), 0)),
            "bold" => bold = Some((path.clone(), 0)),
            _ => {}
        }
    }
    regular.map(|r| (r, bold))
}

fn load(path: &Path, index: u32) -> Option<Arc<FontData>> {
    let bytes = fs::read(path).ok()?;
    let mut data = FontData::from_owned(bytes);
    data.index = index;
    Some(Arc::new(data))
}

/// Install fonts into egui. Returns the family that was found, if any.
pub fn install(ctx: &egui::Context, families: &str) -> Option<String> {
    let mut defs = FontDefinitions::default();
    let mut files = Vec::new();
    for dir in font_dirs() {
        collect(&dir, 3, &mut files);
    }
    let mut found = None;
    for family in families.split(',').map(str::trim).filter(|f| !f.is_empty()) {
        let Some(((reg_path, reg_idx), bold)) = find_family(family, &files) else { continue };
        let Some(regular) = load(&reg_path, reg_idx) else { continue };
        defs.font_data.insert("term".into(), regular);
        defs.families.entry(FontFamily::Monospace).or_default().insert(0, "term".into());
        if let Some(bold) = bold.and_then(|(p, i)| load(&p, i)) {
            defs.font_data.insert("term-bold".into(), bold);
            let mut list = vec!["term-bold".to_string()];
            list.extend(defs.families[&FontFamily::Monospace].iter().cloned());
            defs.families.insert(FontFamily::Name(BOLD_FAMILY.into()), list);
        }
        found = Some(family.to_string());
        break;
    }
    if !defs.families.contains_key(&FontFamily::Name(BOLD_FAMILY.into())) {
        let mono = defs.families[&FontFamily::Monospace].clone();
        defs.families.insert(FontFamily::Name(BOLD_FAMILY.into()), mono);
    }
    // The system UI font for everything that is not terminal text.
    for path in ["/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/HelveticaNeue.ttc"] {
        if let Some(data) = Path::new(path).exists().then(|| load(Path::new(path), 0)).flatten() {
            defs.font_data.insert("ui".into(), data);
            defs.families.entry(FontFamily::Proportional).or_default().insert(0, "ui".into());
            break;
        }
    }
    // Symbols the primary font lacks.
    for (name, path) in [
        ("fallback-symbols", "/System/Library/Fonts/Apple Symbols.ttf"),
        ("fallback-menlo", "/System/Library/Fonts/Menlo.ttc"),
    ] {
        if let Some(data) = Path::new(path).exists().then(|| load(Path::new(path), 0)).flatten() {
            defs.font_data.insert(name.into(), data);
            for fam in [FontFamily::Monospace, FontFamily::Proportional, FontFamily::Name(BOLD_FAMILY.into())] {
                defs.families.entry(fam).or_default().push(name.into());
            }
        }
    }
    ctx.set_fonts(defs);
    found
}
