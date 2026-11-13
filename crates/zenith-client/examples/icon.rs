//! Writes the game's icon as a PNG, as big as asked: `cargo run -p zenith-client --example icon -- SIZE FILE`.
//! The Android build makes its launcher icons with this.
use std::io::BufWriter;

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let size: u32 = args.next().and_then(|s| s.parse().ok()).ok_or("the first argument is the size in pixels")?;
    let path = args.next().ok_or("the second argument is the file to write")?;
    let file = std::fs::File::create(&path).map_err(|e| format!("{path}: {e}"))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&zenith_client::icon::paint_icon(size)).map_err(|e| e.to_string())
}
