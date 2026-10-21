use anyhow::{Context, Result};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

/// Writes the image on the clipboard to `path` as PNG.
pub fn clipboard_png(path: &Path) -> Result<()> {
    let image = arboard::Clipboard::new()
        .context("opening the clipboard")?
        .get_image()
        .context("no image on the clipboard")?;
    write_png(path, image.width as u32, image.height as u32, &image.bytes)
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_readable_png() {
        let dir = std::env::temp_dir().join(format!("lemen-clip-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.png");
        write_png(&path, 2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
