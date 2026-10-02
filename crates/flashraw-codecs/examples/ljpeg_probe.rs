//! Debug helper: decode the first row of a lossless JPEG stream embedded in a file.
//! `cargo run --release -p flashraw-codecs --example ljpeg_probe -- FILE OFFSET [N]`
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&a[1]).unwrap();
    let off: usize = a[2].parse().unwrap();
    let n: usize = a.get(3).map(|v| v.parse().unwrap()).unwrap_or(8);
    let s = &data[off..];
    let hdr = flashraw_codecs::ljpeg::LjpegHeader::parse(s, Default::default()).unwrap();
    println!("{}x{} clrs {} bits {} pred {}", hdr.width, hdr.height, hdr.clrs, hdr.bits, hdr.predictor);
    let mut first = true;
    flashraw_codecs::ljpeg::decode_rows(s, &hdr, |_, row| {
        if first {
            println!("{:?}", &row[..n.min(row.len())]);
            first = false;
        }
        Ok(())
    })
    .ok();
}
