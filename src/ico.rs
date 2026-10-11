//! Parse an ICO file directory. Offsets and lengths belong to directory
//! entries, never to the embedded DIB/PNG header or a Win32 resource ID.

pub fn image_for_size(data: &[u8], desired: u32) -> Option<&[u8]> {
    let u16_at = |i| Some(u16::from_le_bytes(data.get(i..i + 2)?.try_into().ok()?));
    let u32_at = |i| Some(u32::from_le_bytes(data.get(i..i + 4)?.try_into().ok()?));
    if u16_at(0)? != 0 || u16_at(2)? != 1 {
        return None;
    }
    let count = u16_at(4)? as usize;
    let dir_end = 6usize.checked_add(count.checked_mul(16)?)?;
    if count == 0 || dir_end > data.len() {
        return None;
    }
    let mut best = None;
    let mut score = (u32::MAX, u32::MAX, u16::MAX);
    for i in 0..count {
        let entry = 6 + 16 * i;
        let dimension = |n: u8| if n == 0 { 256 } else { n as u32 };
        let w = dimension(data[entry]);
        let h = dimension(data[entry + 1]);
        let len = u32_at(entry + 8)? as usize;
        let offset = u32_at(entry + 12)? as usize;
        if offset < dir_end || len == 0 {
            continue;
        }
        let Some(end) = offset.checked_add(len) else {
            continue;
        };
        let Some(image) = data.get(offset..end) else {
            continue;
        };
        // Prefer exact dimensions, then a larger image before a smaller one.
        let rank = (
            w.abs_diff(desired) + h.abs_diff(desired),
            u32::from(w < desired || h < desired),
            u16::MAX - u16_at(entry + 6)?,
        );
        if rank < score {
            score = rank;
            best = Some(image);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    const ICON: &[u8] = include_bytes!("../assets/ICON.ico");

    #[test]
    fn selects_real_16_and_32_pixel_dib_data() {
        for size in [16, 32] {
            let image = image_for_size(ICON, size).unwrap();
            assert_eq!(u32::from_le_bytes(image[0..4].try_into().unwrap()), 40);
            assert_eq!(u32::from_le_bytes(image[4..8].try_into().unwrap()), size);
            assert_eq!(
                u32::from_le_bytes(image[8..12].try_into().unwrap()),
                size * 2
            );
            assert_eq!(image.len(), if size == 16 { 1128 } else { 4264 });
            assert!(image[40..40 + (size * size * 4) as usize]
                .chunks(4)
                .any(|p| p[3] == 255));
        }
    }

    #[test]
    fn rejects_truncated_or_non_icon_directory() {
        for len in 0..150 {
            assert!(image_for_size(&ICON[..len], 16).is_none());
        }
        let mut data = ICON.to_vec();
        data[2] = 2;
        assert!(image_for_size(&data, 16).is_none());
    }

    #[test]
    fn rejects_invalid_image_offsets_without_panicking() {
        let mut data = ICON.to_vec();
        for i in 0..9 {
            data[6 + 16 * i + 12..6 + 16 * i + 16].fill(255);
        }
        assert!(image_for_size(&data, 16).is_none());
    }
}
