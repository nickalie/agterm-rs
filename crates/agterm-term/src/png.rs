//! The PNG decoder libghostty-vt calls for Kitty graphics PNG transmissions (`f=100`), on the `png` crate.

use agterm_ghostty_vt::sys::DecodedImage;
use png::{ColorType, Transformations};

/// The largest image decoded (pixels); the library also caps what it stores.
const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// Decode `data` to 8-bit RGBA, or `None` when it is not a PNG this decoder handles.
pub(crate) fn decode(data: &[u8]) -> Option<DecodedImage> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    // palette → RGB, 16-bit → 8-bit, sub-byte gray → 8-bit, tRNS → alpha
    decoder.set_transformations(Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buffer).ok()?;
    let pixels = buffer.get(..frame.buffer_size())?;
    let rgba = match frame.color_type {
        ColorType::Rgba => pixels.to_vec(),
        ColorType::Rgb => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 0xff])
            .collect(),
        ColorType::GrayscaleAlpha => pixels
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[g, a]| [g, g, g, a])
            .collect(),
        ColorType::Grayscale => pixels.iter().flat_map(|&g| [g, g, g, 0xff]).collect(),
        ColorType::Indexed => return None,
    };
    Some(DecodedImage {
        width: frame.width,
        height: frame.height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(color: ColorType, width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(pixels).unwrap();
        writer.finish().unwrap();
        out
    }

    #[test]
    fn every_color_type_comes_out_as_rgba() {
        let rgba = decode(&encode(ColorType::Rgba, 1, 1, &[1, 2, 3, 4])).unwrap();
        assert_eq!((rgba.width, rgba.height, rgba.rgba), (1, 1, vec![1, 2, 3, 4]));
        let rgb = decode(&encode(ColorType::Rgb, 2, 1, &[1, 2, 3, 4, 5, 6])).unwrap();
        assert_eq!(rgb.rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
        let gray = decode(&encode(ColorType::Grayscale, 1, 1, &[7])).unwrap();
        assert_eq!(gray.rgba, [7, 7, 7, 255]);
        let gray_alpha = decode(&encode(ColorType::GrayscaleAlpha, 1, 1, &[7, 8])).unwrap();
        assert_eq!(gray_alpha.rgba, [7, 7, 7, 8]);
    }

    #[test]
    fn garbage_is_refused() {
        assert!(decode(b"not a png").is_none());
        assert!(decode(&[]).is_none());
    }
}
