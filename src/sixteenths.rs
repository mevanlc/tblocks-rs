use super::octant_glyph;

// U+1CEA0..U+1CEAF, in code point order. Bits run left to right, top to bottom.
// https://www.unicode.org/charts/PDF/Unicode-17.0/U170-1CC00.pdf
const EDGE_BLOCKS: [u16; 16] = [
    0xC000, 0xE000, 0x7000, 0x3000, 0x1100, 0x1110, 0x0111, 0x0011, 0x0003, 0x0007, 0x000E, 0x000C,
    0x0088, 0x0888, 0x8880, 0x8800,
];

// Other quarter-aligned blocks that cannot be represented by a 2x4 octant mask.
const QUARTER_BLOCKS: [(u16, char); 6] = [
    (0x1111, '\u{258E}'),  // Left one quarter.
    (0x7777, '\u{258A}'),  // Left three quarters.
    (0x8888, '\u{1FB87}'), // Right one quarter.
    (0xEEEE, '\u{1FB8A}'), // Right three quarters.
    (0x0066, '\u{1FBE4}'), // Upper centre one quarter.
    (0x6600, '\u{1FBE5}'), // Lower centre one quarter.
];

pub fn glyph(mask: u16) -> Option<char> {
    if mask.is_power_of_two() {
        return char::from_u32(0x1CE90 + mask.trailing_zeros());
    }
    if let Some(index) = EDGE_BLOCKS.iter().position(|candidate| *candidate == mask) {
        return char::from_u32(0x1CEA0 + index as u32);
    }
    if let Some((_, glyph)) = QUARTER_BLOCKS
        .iter()
        .find(|(candidate, _)| *candidate == mask)
    {
        return Some(*glyph);
    }

    octant_mask(mask).map(octant_glyph)
}

/// Collapse complete horizontal pairs into the logical cells of a 2x4 grid.
pub fn octant_mask(mask: u16) -> Option<u8> {
    let mut octants = 0;
    for row in 0..4 {
        for column in 0..2 {
            match mask >> (row * 4 + column * 2) & 0b11 {
                0 => {}
                3 => octants |= 1 << (row * 2 + column),
                _ => return None,
            }
        }
    }
    Some(octants)
}

pub fn mask(glyph: char) -> Option<u16> {
    match glyph {
        '\u{1CE90}'..='\u{1CE9F}' => return Some(1 << (glyph as u32 - 0x1CE90)),
        '\u{1CEA0}'..='\u{1CEAF}' => return Some(EDGE_BLOCKS[glyph as usize - 0x1CEA0]),
        ' ' => return Some(0),
        _ => {}
    }
    if let Some((mask, _)) = QUARTER_BLOCKS
        .iter()
        .find(|(_, candidate)| *candidate == glyph)
    {
        return Some(*mask);
    }
    let octants = (u8::MIN..=u8::MAX).find(|candidate| octant_glyph(*candidate) == glyph)?;
    let mut mask = 0;
    for row in 0..4 {
        for column in 0..2 {
            if octants & (1 << (row * 2 + column)) != 0 {
                mask |= 0b11 << (row * 4 + column * 2);
            }
        }
    }
    Some(mask)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn single_cells_follow_row_major_unicode_order() {
        for row in 0..4 {
            for column in 0..4 {
                let bits = 1 << (row * 4 + column);
                let expected = char::from_u32(0x1CE90 + row * 4 + column).unwrap();
                assert_eq!(glyph(bits), Some(expected));
                assert_eq!(mask(expected), Some(bits));
            }
        }
    }

    #[test]
    fn block_combinations_match_their_unicode_geometry() {
        for (bits, expected) in [
            (0x1000, '\u{1CE9C}'),
            (0x3000, '\u{1CEA3}'),
            (0x7000, '\u{1CEA2}'),
            (0xE000, '\u{1CEA1}'),
            (0xC000, '\u{1CEA0}'),
            (0x1100, '\u{1CEA4}'),
            (0x1110, '\u{1CEA5}'),
            (0x0111, '\u{1CEA6}'),
            (0x0011, '\u{1CEA7}'),
            (0x0003, '\u{1CEA8}'),
            (0x0007, '\u{1CEA9}'),
            (0x000E, '\u{1CEAA}'),
            (0x000C, '\u{1CEAB}'),
            (0x0088, '\u{1CEAC}'),
            (0x0888, '\u{1CEAD}'),
            (0x8880, '\u{1CEAE}'),
            (0x8800, '\u{1CEAF}'),
            (0x1111, '▎'),
            (0x7777, '▊'),
            (0x8888, '\u{1FB87}'),
            (0xEEEE, '\u{1FB8A}'),
            (0x0066, '\u{1FBE4}'),
            (0x6600, '\u{1FBE5}'),
            (0x0330, '\u{1FBE6}'),
            (0x0CC0, '\u{1FBE7}'),
            (0x0033, '▘'),
            (0xCC00, '▗'),
            (0x00FF, '▀'),
            (0xFF00, '▄'),
            (0x3333, '▌'),
            (0xCCCC, '▐'),
            (0xFFFF, '█'),
            (0x0000, '\u{00A0}'),
        ] {
            assert_eq!(glyph(bits), Some(expected), "mask {bits:#06x}");
            assert_eq!(mask(expected), Some(bits), "glyph {expected:?}");
        }
    }

    #[test]
    fn all_supported_patterns_round_trip_without_approximating_other_selections() {
        let mut glyphs = HashSet::new();
        for bits in u16::MIN..=u16::MAX {
            if let Some(character) = glyph(bits) {
                assert!(glyphs.insert(character), "duplicate glyph {character:?}");
                assert_eq!(mask(character), Some(bits));
            }
        }
        assert_eq!(glyphs.len(), 290);
        assert_eq!(glyph(0x1001), None);
        assert_eq!(glyph(0x0060), None);
        assert_eq!(mask('a'), None);
    }
}
