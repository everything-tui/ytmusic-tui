use image::{DynamicImage, Rgb, RgbImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    Shuffle,
    Prev,
    Play,
    Pause,
    Next,
    Repeat,
    RepeatOne,
    Heart,
    HeartOutline,
}

type P = (f32, f32);

fn tri(p: P, a: P, b: P, c: P) -> bool {
    let s = |p1: P, p2: P, p3: P| (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1);
    let (d1, d2, d3) = (s(p, a, b), s(p, b, c), s(p, c, a));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

fn rect(p: P, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
    p.0 >= x0 && p.0 <= x1 && p.1 >= y0 && p.1 <= y1
}

fn seg(p: P, a: P, b: P, half: f32) -> bool {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / (abx * abx + aby * aby)).clamp(0.0, 1.0);
    let (dx, dy) = (p.0 - (a.0 + t * abx), p.1 - (a.1 + t * aby));
    dx * dx + dy * dy <= half * half
}

fn poly(p: P, pts: &[P], half: f32) -> bool {
    pts.windows(2).any(|w| seg(p, w[0], w[1], half))
}

fn heart(u: f32, v: f32, scale: f32) -> bool {
    let x = (u - 0.5) * 2.5 / scale;
    let y = ((0.5 - v) * 2.5 + 0.12) / scale;
    let a = x * x + y * y - 1.0;
    a * a * a - x * x * y * y * y <= 0.0
}

fn inside(icon: Icon, p: P) -> bool {
    match icon {
        Icon::Play => tri(p, (0.26, 0.12), (0.26, 0.88), (0.88, 0.5)),
        Icon::Pause => rect(p, 0.2, 0.14, 0.42, 0.86) || rect(p, 0.58, 0.14, 0.8, 0.86),
        Icon::Prev => {
            rect(p, 0.12, 0.16, 0.26, 0.84) || tri(p, (0.88, 0.16), (0.88, 0.84), (0.3, 0.5))
        }
        Icon::Next => {
            rect(p, 0.74, 0.16, 0.88, 0.84) || tri(p, (0.12, 0.16), (0.12, 0.84), (0.7, 0.5))
        }
        Icon::Shuffle => {
            let h = 0.045;
            poly(p, &[(0.08, 0.7), (0.32, 0.7), (0.62, 0.3), (0.74, 0.3)], h)
                || poly(p, &[(0.08, 0.3), (0.32, 0.3), (0.62, 0.7), (0.74, 0.7)], h)
                || tri(p, (0.72, 0.16), (0.72, 0.44), (0.94, 0.3))
                || tri(p, (0.72, 0.56), (0.72, 0.84), (0.94, 0.7))
        }
        Icon::Repeat | Icon::RepeatOne => {
            let (c, r, half) = ((0.5f32, 0.5f32), 0.3f32, 0.055f32);
            let (dx, dy) = (p.0 - c.0, p.1 - c.1);
            let d = (dx * dx + dy * dy).sqrt();
            // Angles in screen space (y down); gap sits at the top-right.
            let ang = dy.atan2(dx).to_degrees();
            let on_arc = !(-88.0..=-18.0).contains(&ang);
            let ring = (d - r).abs() <= half && on_arc;
            // Arrow head at the arc end (top, pointing right).
            let head = tri(p, (0.5, 0.2 - 0.12), (0.5, 0.2 + 0.12), (0.72, 0.2));
            let one = icon == Icon::RepeatOne
                && (rect(p, 0.47, 0.38, 0.54, 0.64) || tri(p, (0.47, 0.38), (0.54, 0.38), (0.4, 0.46)));
            ring || head || one
        }
        Icon::Heart => heart(p.0, p.1, 1.0),
        Icon::HeartOutline => heart(p.0, p.1, 1.0) && !heart(p.0, p.1, 0.62),
    }
}

pub fn render(icon: Icon, fg: (u8, u8, u8), bg: (u8, u8, u8)) -> DynamicImage {
    const N: u32 = 128;
    const SS: u32 = 3;
    let mut img = RgbImage::new(N, N);
    for py in 0..N {
        for px in 0..N {
            let mut hits = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let u = (px as f32 + (sx as f32 + 0.5) / SS as f32) / N as f32;
                    let v = (py as f32 + (sy as f32 + 0.5) / SS as f32) / N as f32;
                    if inside(icon, (u, v)) {
                        hits += 1;
                    }
                }
            }
            let a = hits as f32 / (SS * SS) as f32;
            let mix = |f: u8, b: u8| (f as f32 * a + b as f32 * (1.0 - a)).round() as u8;
            img.put_pixel(px, py, Rgb([mix(fg.0, bg.0), mix(fg.1, bg.1), mix(fg.2, bg.2)]));
        }
    }
    DynamicImage::ImageRgb8(img)
}
