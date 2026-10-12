// A monochrome server rack, rendered at 2x with subpixel coverage.
// macOS treats the alpha mask as a template and supplies the menu-bar color.
pub const SIZE: u32 = 44;

fn segment(x: f32, y: f32, a: (f32, f32), b: (f32, f32), width: f32) -> bool {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (x - a.0 - t * dx).hypot(y - a.1 - t * dy) <= width / 2.0
}
fn rounded_box(x: f32, y: f32, top: f32, inset: f32) -> bool {
    let radius = (1.2 - inset).max(0.1);
    let half_x = 7.0 - inset;
    let half_y = 2.25 - inset;
    let qx = (x - 11.0).abs() - half_x + radius;
    let qy = (y - top - 2.25).abs() - half_y + radius;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) <= radius
}
pub fn pixels(disconnected: bool) -> Vec<u8> {
    let mut rgba = vec![0; (SIZE * SIZE * 4) as usize];
    for py in 0..SIZE {
        for px in 0..SIZE {
            let mut coverage = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (px as f32 + (sx as f32 + 0.5) / 4.0) / 2.0;
                    let y = (py as f32 + (sy as f32 + 0.5) / 4.0) / 2.0;
                    let mut ink = [2.5, 8.75, 15.0].iter().any(|top| {
                        (rounded_box(x, y, *top, 0.0) && !rounded_box(x, y, *top, 1.15))
                            || (x - 6.75).hypot(y - top - 2.25) <= 0.6
                            || segment(x, y, (10.5, top + 2.25), (15.0, top + 2.25), 1.0)
                    });
                    if disconnected {
                        let a = (2.75, 2.0);
                        let b = (19.25, 20.0);
                        if segment(x, y, a, b, 3.5) {
                            ink = false;
                        }
                        if segment(x, y, a, b, 1.65) {
                            ink = true;
                        }
                    }
                    coverage += u32::from(ink);
                }
            }
            let i = ((py * SIZE + px) * 4) as usize;
            rgba[i + 3] = ((coverage * 255 + 8) / 16) as u8;
            // White on the standard dark Windows taskbar. macOS uses alpha only.
            #[cfg(target_os = "windows")]
            rgba[i..i + 3].fill(255);
        }
    }
    rgba
}
