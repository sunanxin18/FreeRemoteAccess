# UI font policy

FreeRemoteDesk keeps platform UI typography first and uses bundled fonts only
for missing glyphs. `frd-shell-desktop/src/ui_fonts.rs` builds the active chain
in this order:

1. platform UI font;
2. the platform CJK font matching the current UI locale;
3. egui's built-in fonts;
4. bundled Noto Sans SC as the final fallback.

If no platform font can be loaded, Noto Sans SC becomes the proportional UI
font so connection and diagnostic text remains readable. The current UI locale
is `zh-Hans`; changing localized copy must change the locale selection at the
same boundary so unified Han characters receive the intended regional glyphs.

Material Symbols Rounded is registered under a separate named family and is
not part of this text chain. HarmonyOS NEXT uses ArkUI's native font cascade;
the Rust fallback asset is not a replacement for HarmonyOS system typography.

See `noto-sans-sc/SOURCE.md` and `noto-sans-sc/OFL.txt` for provenance and
redistribution terms.
