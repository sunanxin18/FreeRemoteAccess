pub fn rdp_16bit_to_rgb(color: u16) -> [u8; 3] {
    #[expect(clippy::missing_panics_doc, reason = "unreachable panic (checked integer underflow)")]
    let out = {
        let r = u8::try_from(((((color >> 11) & 0x1f) * 527) + 23) >> 6).expect("max possible value is 255");
        let g = u8::try_from(((((color >> 5) & 0x3f) * 259) + 33) >> 6).expect("max possible value is 255");
        let b = u8::try_from((((color & 0x1f) * 527) + 23) >> 6).expect("max possible value is 255");
        [r, g, b]
    };

    out
}

