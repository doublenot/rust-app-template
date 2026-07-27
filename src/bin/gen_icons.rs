//! Regenerates icons/icon.png. Replace that file with real branding per app;
//! rerun this only to restore the placeholder: cargo run --bin gen_icons
use image::{Rgba, RgbaImage};

fn main() {
    const S: u32 = 512;
    let mut img = RgbaImage::new(S, S);
    let c = S as i32 / 2;
    let r = (S as i32 / 2) - 24;
    for (x, y, px) in img.enumerate_pixels_mut() {
        let dx = x as i32 - c;
        let dy = y as i32 - c;
        *px = if dx * dx + dy * dy < r * r {
            Rgba([0x4a, 0x7d, 0xfc, 0xff])
        } else {
            Rgba([0, 0, 0, 0])
        };
    }
    std::fs::create_dir_all("icons").unwrap();
    img.save("icons/icon.png").unwrap();
    println!("wrote icons/icon.png");
}
