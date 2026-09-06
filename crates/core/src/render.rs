use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image::{DynamicImage, RgbaImage};
use pdfium_render::prelude::*;

use crate::config::Paths;

/// Where the PDFium dynamic library lives. Order: `RAG_PDFIUM_PATH`, the data
/// dir's `pdfium/` folder, beside the executable, then the current directory.
/// The library itself is a prebuilt `libpdfium.dylib`/`.so`/`.dll` downloaded
/// from bblanchon/pdfium-binaries; pdfium-render only binds at runtime.
pub fn pdfium_dir(paths: &Paths) -> PathBuf {
    if let Ok(dir) = std::env::var("RAG_PDFIUM_PATH") {
        return PathBuf::from(dir);
    }
    let data_dir = paths.data.join("pdfium");
    if data_dir.exists() {
        return data_dir;
    }
    let executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.to_path_buf()));
    if let Some(dir) = executable {
        if dir.join("libpdfium.dylib").exists() || dir.join("libpdfium.so").exists() {
            return dir;
        }
    }
    PathBuf::from(".")
}

/// Render one page to PNG bytes. `width_px`/`height_px` are the target bitmap
/// size; pick them from page dimensions and a DPI you want.
pub fn render_page_png(path: &Path, page_index: usize, width_px: u32, height_px: u32) -> Result<Vec<u8>> {
    let dir = pdfium_dir(&crate::config::Paths::resolve()?);
    let bindings = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&dir))
        .with_context(|| {
            format!(
                "could not load PDFium from {} — put libpdfium there (bblanchon/pdfium-binaries) or set RAG_PDFIUM_PATH",
                dir.display()
            )
        })?;
    let pdfium = Pdfium::new(bindings);

    let document = pdfium
        .load_pdf_from_file(path, None)
        .with_context(|| format!("could not open {}", path.display()))?;
    let page = document
        .pages()
        .get(page_index as u16)
        .with_context(|| format!("page {} does not exist", page_index + 1))?;

    let bitmap = page
        .render_with_config(
            &PdfRenderConfig::new()
                .set_fixed_size(width_px as i32, height_px as i32)
                .set_format(PdfBitmapFormat::BGRA),
        )
        .with_context(|| format!("could not render page {}", page_index + 1))?;

    let width = bitmap.width() as usize;
    let height = bitmap.height() as usize;
    let mut rgba = RgbaImage::from_raw(width as u32, height as u32, bitmap.as_raw_bytes())
        .ok_or_else(|| anyhow::anyhow!("render produced a bad bitmap"))?;
    // PDFium hands us BGRA; the PNG encoder wants RGBA.
    for pixel in rgba.pixels_mut() {
        let [b, g, r, a] = pixel.0;
        pixel.0 = [r, g, b, a];
    }

    let mut cursor = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(rgba)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .context("could not encode PNG")?;
    Ok(cursor.into_inner())
}

/// PDFium is loaded at runtime, so "is it there" is a question about files.
pub fn library_present(paths: &Paths) -> bool {
    let dir = pdfium_dir(paths);
    ["libpdfium.dylib", "libpdfium.so", "pdfium.dll"]
        .iter()
        .any(|name| dir.join(name).exists())
}

/// Load PDFium from wherever this install keeps it.
pub fn bind(paths: &Paths) -> Result<Pdfium> {
    let dir = pdfium_dir(paths);
    let bindings = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&dir))
        .with_context(|| {
            format!(
                "could not load PDFium from {} — put libpdfium there (bblanchon/pdfium-binaries) or set RAG_PDFIUM_PATH",
                dir.display()
            )
        })?;
    Ok(Pdfium::new(bindings))
}

/// Render `pages` (0-based) of one PDF to PNG at `dpi`, opening the document
/// once. Captioning a document means rendering tens of pages, and re-parsing a
/// 958-page PDF per figure would cost more than the vision call itself.
pub fn render_pages(
    paths: &Paths,
    path: &Path,
    pages: &[usize],
    dpi: u32,
) -> Result<Vec<(usize, Vec<u8>)>> {
    if pages.is_empty() {
        return Ok(Vec::new());
    }
    let pdfium = bind(paths)?;
    let document = pdfium
        .load_pdf_from_file(path, None)
        .with_context(|| format!("could not open {}", path.display()))?;
    let scale = dpi as f32 / 72.0;

    let mut rendered = Vec::with_capacity(pages.len());
    for index in pages {
        let page = document
            .pages()
            .get(u16::try_from(*index).context("page number too large for PDFium")?)
            .with_context(|| format!("page {} does not exist", index + 1))?;
        // Scale the page itself rather than the bitmap, so the vector lines and
        // the text on it rasterise at the same resolution.
        let config = PdfRenderConfig::new()
            .set_fixed_size((page.width().value as f32 * scale) as i32,
                           (page.height().value as f32 * scale) as i32)
            .set_format(PdfBitmapFormat::BGRA);
        let bitmap = page.render_with_config(&config)
            .with_context(|| format!("could not render page {} of {}", index + 1, path.display()))?;
        rendered.push((*index, encode_bitmap(&bitmap)?));
    }
    Ok(rendered)
}

/// PDFium hands us BGRA; the PNG encoder wants RGBA.
fn encode_bitmap(bitmap: &pdfium_render::prelude::PdfBitmap) -> Result<Vec<u8>> {
    let width = bitmap.width() as u32;
    let height = bitmap.height() as u32;
    let mut rgba = RgbaImage::from_raw(width, height, bitmap.as_raw_bytes().to_vec())
        .ok_or_else(|| anyhow::anyhow!("render produced a bad bitmap"))?;
    for pixel in rgba.pixels_mut() {
        let [b, g, r, a] = pixel.0;
        pixel.0 = [r, g, b, a];
    }
    let mut cursor = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(rgba)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .context("could not encode PNG")?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, because env vars are process-global and cargo runs tests on
    /// parallel threads. Uses a temp data dir rather than the real one.
    #[test]
    fn pdfium_dir_resolution_order() {
        unsafe { std::env::set_var("RAG_PDFIUM_PATH", "/tmp/pdfium-override") };
        let overridden = pdfium_dir(&Paths::resolve().unwrap());
        unsafe { std::env::remove_var("RAG_PDFIUM_PATH") };
        assert_eq!(overridden, PathBuf::from("/tmp/pdfium-override"));

        let root = std::env::temp_dir().join(format!("rag-pdfium-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("pdfium")).unwrap();
        let temp_paths = Paths {
            data: root.clone(),
            models: root.join("models"),
            index: root.join("index"),
        };
        assert_eq!(pdfium_dir(&temp_paths), root.join("pdfium"));
        std::fs::remove_dir_all(&root).ok();
    }

    /// Renders a real page. Defaults to this repo's `docs/`; set RAG_RENDER_PDF
    /// for another PDF. The data-dir pdfium/ folder must hold the library.
    #[test]
    #[ignore = "needs a PDF and the PDFium library"]
    fn render_a_real_page() {
        let path = std::env::var("RAG_RENDER_PDF").unwrap_or_else(|_| {
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../docs/Doc 8168 Vol 2 - Aircraft Operations, Construction of Visual and Instrument Flight Procedures.pdf"
            )
            .to_string()
        });
        let bytes = render_page_png(Path::new(&path), 0, 1200, 1600).unwrap();
        println!("rendered page 1: {} PNG bytes", bytes.len());
        assert!(bytes.len() > 1000);
        assert!(bytes[0..8] == *b"\x89PNG\r\n\x1a\n");
    }
}
