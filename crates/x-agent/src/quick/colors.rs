//! Pure color conversion for X Quick. Does not capture a screen or read clipboard.
//! sRGB is converted using the standard transfer function and Oklab matrices.

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Formats {
    pub hex: String,
    pub rgb: String,
    pub hsl: String,
    pub oklch: String,
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let hex = s.trim().strip_prefix('#')?;
    match hex.len() {
        3 if hex.bytes().all(|x| x.is_ascii_hexdigit()) => {
            let digits = hex.as_bytes();
            Some([
                (digits[0] as char).to_digit(16)? as u8 * 17,
                (digits[1] as char).to_digit(16)? as u8 * 17,
                (digits[2] as char).to_digit(16)? as u8 * 17,
            ])
        }
        6 if hex.bytes().all(|x| x.is_ascii_hexdigit()) => Some([
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        ]),
        _ => None,
    }
}

fn parse_rgb(s: &str) -> Option<[u8; 3]> {
    let raw = s.trim().strip_prefix("rgb(")?.strip_suffix(')')?;
    let mut values = raw.split(',').map(|v| v.trim().parse::<u8>().ok());
    let rgb = [values.next()??, values.next()??, values.next()??];
    values.next().is_none().then_some(rgb)
}

fn linear(value: u8) -> f64 {
    let c = f64::from(value) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn oklch(rgb: [u8; 3]) -> String {
    let [r, g, b] = rgb.map(linear);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    let lightness = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s;
    let a = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s;
    let b_axis = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s;
    let chroma = (a * a + b_axis * b_axis).sqrt();
    let hue = if chroma < 1e-7 {
        0.0
    } else {
        b_axis.atan2(a).to_degrees().rem_euclid(360.0)
    };
    format!("oklch({:.1}% {:.4} {:.1})", lightness * 100.0, chroma, hue)
}

fn hsl(rgb: [u8; 3]) -> String {
    let [r, g, b] = rgb.map(|c| f64::from(c) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let lightness = (max + min) / 2.0;
    let saturation = if delta == 0.0 {
        0.0
    } else {
        delta / (1.0 - (2.0 * lightness - 1.0).abs())
    };
    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        (60.0 * ((g - b) / delta)).rem_euclid(360.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    format!(
        "hsl({:.1} {:.1}% {:.1}%)",
        hue,
        saturation * 100.0,
        lightness * 100.0
    )
}

pub(super) fn convert(input: &str) -> Option<Formats> {
    let s = input.trim();
    let s = s.strip_prefix("color ").unwrap_or(s).trim();
    let rgb = parse_hex(s).or_else(|| parse_rgb(s))?;
    let [r, g, b] = rgb;
    Some(Formats {
        hex: format!("#{r:02X}{g:02X}{b:02X}"),
        rgb: format!("rgb({r}, {g}, {b})"),
        hsl: hsl(rgb),
        oklch: oklch(rgb),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_short_and_long_hex_or_rgb() {
        let color = convert("#fff").unwrap();
        assert_eq!(color.hex, "#FFFFFF");
        assert_eq!(color.rgb, "rgb(255, 255, 255)");
        assert_eq!(color.hsl, "hsl(0.0 0.0% 100.0%)");
        assert_eq!(convert("color #000").unwrap().hex, "#000000");
        assert_eq!(convert("rgb(37, 99, 235)").unwrap().hex, "#2563EB");
    }

    #[test]
    fn color_space_conversions_are_finite_and_consistent() {
        let black = convert("#000000").unwrap();
        assert_eq!(black.oklch, "oklch(0.0% 0.0000 0.0)");
        let white = convert("#ffffff").unwrap();
        assert!(white.oklch.starts_with("oklch(100.0%"));
        let red = convert("#FF0000").unwrap();
        assert_eq!(red.hsl, "hsl(0.0 100.0% 50.0%)");
        assert!(red.oklch.starts_with("oklch(62.8%"));
    }

    #[test]
    fn invalid_colors_do_not_get_coerced() {
        for bad in [
            "#GGGGGG",
            "#12",
            "#12345",
            "#12345678",
            "rgb(256,0,0)",
            "rgb(1,2)",
            "color not a color",
        ] {
            assert!(convert(bad).is_none(), "{bad}");
        }
    }
}
