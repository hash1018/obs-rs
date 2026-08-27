use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;

pub fn install_locale_fonts(ctx: &egui::Context) {
    let Some(bytes) = system_cjk_font_paths()
        .into_iter()
        .find_map(|path| std::fs::read(path).ok())
    else {
        eprintln!("no CJK system font found; Korean glyphs may be unavailable");
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    let font_name = "obs-rs-cjk".to_owned();
    fonts.font_data.insert(
        font_name.clone(),
        Arc::new(egui::FontData::from_owned(bytes)),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .push(font_name.clone());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push(font_name);
    ctx.set_fonts(fonts);
}

#[cfg(target_os = "windows")]
fn system_cjk_font_paths() -> Vec<PathBuf> {
    let windows = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    vec![
        windows.join("Fonts").join("malgun.ttf"),
        windows.join("Fonts").join("malgunsl.ttf"),
    ]
}

#[cfg(target_os = "macos")]
fn system_cjk_font_paths() -> Vec<PathBuf> {
    vec![PathBuf::from("/System/Library/Fonts/AppleSDGothicNeo.ttc")]
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn system_cjk_font_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
        PathBuf::from("/usr/share/fonts/truetype/nanum/NanumGothic.ttf"),
    ]
}
