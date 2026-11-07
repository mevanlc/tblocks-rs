use super::sixteenths;

const fn columns(bits: u8) -> u64 {
    bits as u64 * 0x0101_0101_0101_0101
}

const fn rows(bits: u8) -> u64 {
    let mut mask = 0;
    let mut row = 0;
    while row < 8 {
        if bits & (1 << row) != 0 {
            mask |= 0xFF << (row * 8);
        }
        row += 1;
    }
    mask
}

// Eighth-aligned blocks beyond the 4x4 catalogue. Bits run left to right,
// top to bottom. U+1FB81 is a patterned window title bar, not a solid block.
// https://www.unicode.org/charts/PDF/Unicode-17.0/U170-1FB00.pdf
const BLOCKS: [(u64, char); 33] = [
    (columns(0x01), '\u{258F}'),
    (columns(0x02), '\u{1FB70}'),
    (columns(0x04), '\u{1FB71}'),
    (columns(0x08), '\u{1FB72}'),
    (columns(0x10), '\u{1FB73}'),
    (columns(0x20), '\u{1FB74}'),
    (columns(0x40), '\u{1FB75}'),
    (columns(0x80), '\u{2595}'),
    (rows(0x01), '\u{2594}'),
    (rows(0x02), '\u{1FB76}'),
    (rows(0x04), '\u{1FB77}'),
    (rows(0x08), '\u{1FB78}'),
    (rows(0x10), '\u{1FB79}'),
    (rows(0x20), '\u{1FB7A}'),
    (rows(0x40), '\u{1FB7B}'),
    (rows(0x80), '\u{2581}'),
    (columns(0x07), '\u{258D}'),
    (columns(0x1F), '\u{258B}'),
    (columns(0x7F), '\u{2589}'),
    (columns(0xE0), '\u{1FB88}'),
    (columns(0xF8), '\u{1FB89}'),
    (columns(0xFE), '\u{1FB8B}'),
    (rows(0x07), '\u{1FB83}'),
    (rows(0x1F), '\u{1FB84}'),
    (rows(0x7F), '\u{1FB86}'),
    (rows(0xE0), '\u{2583}'),
    (rows(0xF8), '\u{2585}'),
    (rows(0xFE), '\u{2587}'),
    (columns(0x01) | rows(0x80), '\u{1FB7C}'),
    (columns(0x01) | rows(0x01), '\u{1FB7D}'),
    (columns(0x80) | rows(0x01), '\u{1FB7E}'),
    (columns(0x80) | rows(0x80), '\u{1FB7F}'),
    (rows(0x81), '\u{1FB80}'),
];

pub fn glyph(mask: u64) -> Option<char> {
    BLOCKS
        .iter()
        .find(|(candidate, _)| *candidate == mask)
        .map(|(_, glyph)| *glyph)
        .or_else(|| sixteenth_mask(mask).and_then(sixteenths::glyph))
}

pub fn mask(glyph: char) -> Option<u64> {
    BLOCKS
        .iter()
        .find(|(_, candidate)| *candidate == glyph)
        .map(|(mask, _)| *mask)
        .or_else(|| sixteenths::mask(glyph).map(expand_sixteenths))
}

pub fn expand_sixteenths(mask: u16) -> u64 {
    let mut expanded = 0;
    for row in 0..4 {
        for column in 0..4 {
            if mask & (1 << (row * 4 + column)) != 0 {
                expanded |= 0x0303 << (row * 16 + column * 2);
            }
        }
    }
    expanded
}

/// Collapse only uniform 2x2 groups; partial groups cannot match a coarser grid.
pub fn sixteenth_mask(mask: u64) -> Option<u16> {
    let mut compact = 0;
    for row in 0..4 {
        for column in 0..4 {
            match mask >> (row * 16 + column * 2) & 0x0303 {
                0 => {}
                0x0303 => compact |= 1 << (row * 4 + column),
                _ => return None,
            }
        }
    }
    Some(compact)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn stripes_fills_and_corners_match_unicode_geometry() {
        for (bits, glyph) in [
            (0x1010_1010_1010_1010, '\u{1FB73}'),
            (0x0000_00FF_0000_0000, '\u{1FB79}'),
            (0x0000_0000_00FF_FFFF, '\u{1FB83}'),
            (0xFFFF_FF00_0000_0000, '\u{2583}'),
            (0xE0E0_E0E0_E0E0_E0E0, '\u{1FB88}'),
            (0xFF01_0101_0101_0101, '\u{1FB7C}'),
            (0x0101_0101_0101_01FF, '\u{1FB7D}'),
            (0x8080_8080_8080_80FF, '\u{1FB7E}'),
            (0xFF80_8080_8080_8080, '\u{1FB7F}'),
            (0xFF00_0000_0000_00FF, '\u{1FB80}'),
        ] {
            assert_eq!(super::glyph(bits), Some(glyph));
            assert_eq!(mask(glyph), Some(bits));
        }
    }

    #[test]
    fn catalogue_round_trips_and_coarser_grids_require_complete_groups() {
        let mut glyphs = HashSet::new();
        for bits in u16::MIN..=u16::MAX {
            let expanded = expand_sixteenths(bits);
            assert_eq!(sixteenth_mask(expanded), Some(bits));
            assert_eq!(glyph(expanded), sixteenths::glyph(bits));
            if let Some(character) = glyph(expanded) {
                assert!(glyphs.insert(character));
                assert_eq!(mask(character), Some(expanded));
            }
        }
        for (bits, character) in BLOCKS {
            assert!(glyphs.insert(character));
            assert_eq!(glyph(bits), Some(character));
            assert_eq!(mask(character), Some(bits));
            assert_eq!(sixteenth_mask(bits), None);
        }
        assert_eq!(glyphs.len(), 323);
        // An isolated 1/64 cell and the inner quarter stripe remain unencoded.
        assert_eq!(glyph(1), None);
        assert_eq!(glyph(columns(0x30)), None);
        assert_eq!(glyph(!columns(0x30)), None);
        assert_eq!(mask('a'), None);
    }
}
