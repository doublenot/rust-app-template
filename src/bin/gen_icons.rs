//! Regenerates icons/icon.png. Replace that file with real branding per app;
//! rerun this only to restore the placeholder: cargo run --bin gen_icons
//!
//! The mark is two interlocking rings — a hitch. Three constraints shaped it,
//! all of them about the tray rather than about looking good at 512px:
//!
//!   * **One colour, no fill.** A tray icon sits on a panel that may be light or
//!     dark, and the app cannot know which. A two-tone mark loses whichever tone
//!     matches the panel; an outline in a single mid-weight colour survives both.
//!   * **The weave is drawn as a gap, not a second colour.** Where one ring
//!     passes over the other, the ring underneath is cut away. That reads as
//!     depth at 16px, where shading does not.
//!   * **Anti-aliased by coverage.** The 512px source is downscaled hard — to 32
//!     for the tray. Hard edges alias into grey mush at that ratio; computing
//!     partial coverage per pixel is what keeps it crisp.
use image::{Rgba, RgbaImage};

const S: u32 = 512;
/// Ring centre-line radius.
const R: f32 = 118.0;
/// Half the stroke width.
const T: f32 = 25.0;
/// Horizontal offset of each ring from centre. Twice this is less than 2*R, so
/// the rings genuinely overlap and there is something to weave.
const DX: f32 = 72.0;
/// Width of the cut around the ring that passes over. Wide enough to survive the
/// downscale to 32px, where a hairline gap would close up.
const GAP: f32 = 13.0;
/// Edge softness in pixels.
const AA: f32 = 1.6;

/// Coverage of a ring stroke at a point: 1 inside, 0 outside, fractional across
/// the edge.
fn ring(px: f32, py: f32, cx: f32, cy: f32, half_width: f32) -> f32 {
    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
    // Distance from the stroke's centre-line, then softened across AA pixels.
    let edge = (d - R).abs();
    ((half_width - edge) / AA + 0.5).clamp(0.0, 1.0)
}

fn main() {
    let mut img = RgbaImage::new(S, S);
    let c = S as f32 / 2.0;
    let (ax, bx) = (c - DX, c + DX);

    for (x, y, px) in img.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);

        let a = ring(fx, fy, ax, c, T);
        let b = ring(fx, fy, bx, c, T);
        // The same rings widened by GAP: used to cut the ring passing underneath.
        let a_cut = ring(fx, fy, ax, c, T + GAP);
        let b_cut = ring(fx, fy, bx, c, T + GAP);

        // Alternate which ring is on top between the upper and lower halves —
        // that alternation is what makes it read as woven rather than merely
        // overlapping.
        let alpha = if fy < c {
            a.max(b * (1.0 - a_cut))
        } else {
            b.max(a * (1.0 - b_cut))
        };

        *px = Rgba([0x4a, 0x7d, 0xfc, (alpha * 255.0).round() as u8]);
    }

    std::fs::create_dir_all("icons").unwrap();
    img.save("icons/icon.png").unwrap();
    println!("wrote icons/icon.png");

    write_ico(&img);
}

/// Windows needs a real `.ico` — a desktop shortcut takes its icon from the
/// resource compiled into the `.exe`, and the resource compiler will not accept
/// a PNG file. Without it Explorer draws the generic application icon, even
/// though the taskbar and title bar look right (those come from the page
/// favicon, which is a different mechanism entirely).
///
/// Assembled by hand rather than through another crate: an ICO is a 6-byte
/// header, a 16-byte directory entry per size, and the image payloads. Windows
/// has accepted PNG payloads since Vista, so every entry is just the PNG the
/// `image` crate already produces.
fn write_ico(src: &RgbaImage) {
    // Explorer picks the nearest size and scales; supplying the ones it actually
    // asks for (small icons, list view, tiles, jumbo) avoids it scaling 512 down
    // to 16 and turning the rings to mush.
    const SIZES: [u32; 6] = [16, 32, 48, 64, 128, 256];

    let pngs: Vec<Vec<u8>> = SIZES
        .iter()
        .map(|&s| {
            let scaled = image::imageops::resize(src, s, s, image::imageops::FilterType::Lanczos3);
            let mut buf = std::io::Cursor::new(Vec::new());
            scaled
                .write_to(&mut buf, image::ImageFormat::Png)
                .expect("PNG encode");
            buf.into_inner()
        })
        .collect();

    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // 1 = icon (2 would be a cursor)
    out.extend_from_slice(&(SIZES.len() as u16).to_le_bytes());

    // Payloads start after the header and the whole directory.
    let mut offset = 6 + 16 * SIZES.len() as u32;
    for (&size, png) in SIZES.iter().zip(&pngs) {
        // 256 is encoded as 0: the field is one byte, so 256 does not fit.
        let dim = if size == 256 { 0u8 } else { size as u8 };
        out.push(dim); // width
        out.push(dim); // height
        out.push(0); // palette size; 0 for truecolour
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in &pngs {
        out.extend_from_slice(png);
    }

    std::fs::write("icons/icon.ico", &out).unwrap();
    println!("wrote icons/icon.ico ({} sizes)", SIZES.len());
}
