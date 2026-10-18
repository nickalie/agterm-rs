//! uucode's parser tests, and each file's parse on small inputs: the values it reads, the defaults it fills in, and
//! the input it refuses.

use super::{
    EastAsianWidth, GeneralCategory, IndicConjunctBreak, NUM_CODE_POINTS, OriginalGraphemeBreak, UcdFile, blocks,
    case_folding, derived_core_properties, east_asian_widths, emoji_data, emoji_vs, grapheme_break_property, parse_cp,
    parse_range, trim, unicode_data,
};

fn file(name: &str, text: &str) -> UcdFile {
    UcdFile {
        name: name.to_owned(),
        text: text.to_owned(),
    }
}

/// uucode's `test "parseCp"`, and the bounds of the code point space.
#[test]
fn parse_cp_reads_hex_inside_the_code_point_space() {
    assert_eq!(parse_cp("0000"), Ok(0));
    assert_eq!(parse_cp("1F600"), Ok(0x1F600));
    assert_eq!(parse_cp("10FFFF"), Ok(0x10_FFFF));
    assert!(parse_cp("110000").is_err());
    assert!(parse_cp("12G4").is_err());
    assert!(parse_cp("").is_err());
}

/// uucode's `test "parseRange"`: an inclusive `..` range, or one code point, as a half-open range.
#[test]
fn parse_range_reads_ranges_and_single_code_points() {
    assert_eq!(parse_range("0030..0039"), Ok(0x30..0x3A));
    assert_eq!(parse_range("1F600"), Ok(0x1F600..0x1F601));
    assert!(parse_range("0039..0030").is_err());
    assert!(parse_range("0030..").is_err());
}

#[test]
fn trim_cuts_the_comment_and_the_surrounding_whitespace() {
    assert_eq!(trim("0041 ; Lu # LATIN CAPITAL LETTER A"), "0041 ; Lu");
    assert_eq!(trim("\t0041;Lu\r"), "0041;Lu");
    assert_eq!(trim("# only a comment"), "");
    assert_eq!(trim("  \r"), "");
}

#[test]
fn a_file_must_name_the_pinned_ucd_version_if_it_names_one() {
    let versioned = file(
        "Blocks.txt",
        "# Blocks-18.0.0.txt\n# Date: 2026-07-08\n0000..007F; Basic Latin\n",
    );
    assert!(versioned.check_version("18.0.0").is_ok());
    let error = versioned.check_version("17.0.0").unwrap_err();
    assert!(error.contains("UCD 18.0.0, but the pin is UCD 17.0.0"), "{error}");

    let emoji = file(
        "emoji/emoji-data.txt",
        "# emoji-data.txt\n# Date: 2026\n#\n# Version: 18.0.0\n",
    );
    assert!(emoji.check_version("18.0.0").is_ok());
    assert!(emoji.check_version("18.0.1").is_err());

    // UnicodeData.txt has no header at all
    let unnamed = file("UnicodeData.txt", "0000;<control>;Cc;0;BN;;;;;N;NULL;;;;\n");
    assert!(unnamed.check_version("18.0.0").is_ok());
}

#[test]
fn data_lines_number_lines_from_one_and_skip_comments() {
    let f = file("x.txt", "# header\n\n0041; A # comment\r\n0042; B\n");
    let lines: Vec<(usize, &str)> = f.data_lines().collect();
    assert_eq!(lines, [(3, "0041; A"), (4, "0042; B")]);
    assert_eq!(f.error(3, "bad"), "x.txt:3: bad");
}

const UNICODE_DATA: &str = "\
0041;LATIN CAPITAL LETTER A;Lu;0;L;;;;;N;;;;0061;
0300;COMBINING GRAVE ACCENT;Mn;230;NSM;;;;;N;NON-SPACING GRAVE;;;;
3400;<CJK Ideograph Extension A, First>;Lo;0;L;;;;;N;;;;;
4DBF;<CJK Ideograph Extension A, Last>;Lo;0;L;;;;;N;;;;;
E000;<Private Use, First>;Co;0;L;;;;;N;;;;;
F8FF;<Private Use, Last>;Co;0;L;;;;;N;;;;;
";

/// Listed code points get their category, a First/Last pair its whole range, and every other code point `Cn`.
#[test]
fn unicode_data_fills_ranges_and_gaps() {
    let column = unicode_data(&file("UnicodeData.txt", UNICODE_DATA)).unwrap();
    assert_eq!(column.len(), NUM_CODE_POINTS);
    assert_eq!(column[0x41], GeneralCategory::LetterUppercase);
    assert_eq!(column[0x300], GeneralCategory::MarkNonspacing);
    for cp in [0x3400, 0x3401, 0x4000, 0x4DBF] {
        assert_eq!(column[cp], GeneralCategory::LetterOther, "U+{cp:X}");
    }
    for cp in [0xE000, 0xF000, 0xF8FF] {
        assert_eq!(column[cp], GeneralCategory::OtherPrivateUse, "U+{cp:X}");
    }
    for cp in [0x0, 0x40, 0x42, 0x2FF, 0x301, 0x33FF, 0x4DC0, 0xF900, 0x10_FFFF] {
        assert_eq!(column[cp], GeneralCategory::OtherNotAssigned, "U+{cp:X}");
    }
}

#[test]
fn unicode_data_refuses_a_broken_range_or_an_unknown_category() {
    let unclosed = "3400;<CJK Ideograph Extension A, First>;Lo;0;L;;;;;N;;;;;\n";
    assert!(
        unicode_data(&file("UnicodeData.txt", unclosed))
            .unwrap_err()
            .contains("ends inside a range")
    );
    let no_last = "3400;<X, First>;Lo;0;L;;;;;N;;;;;\n4DBF;<Y>;Lo;0;L;;;;;N;;;;;\n";
    assert!(
        unicode_data(&file("UnicodeData.txt", no_last))
            .unwrap_err()
            .contains("UnicodeData.txt:2")
    );
    let unknown = "0041;LATIN CAPITAL LETTER A;Xx;0;L;;;;;N;;;;0061;\n";
    let error = unicode_data(&file("UnicodeData.txt", unknown)).unwrap_err();
    assert!(
        error.contains("UnicodeData.txt:1: unknown general category Xx"),
        "{error}"
    );
    let backwards = "0042;B;Lu;0;L;;;;;N;;;;;\n0041;A;Lu;0;L;;;;;N;;;;;\n";
    assert!(
        unicode_data(&file("UnicodeData.txt", backwards))
            .unwrap_err()
            .contains("out of order")
    );
}

const CASE_FOLDING: &str = "\
# CaseFolding-18.0.0.txt
0041; C; 0061; # LATIN CAPITAL LETTER A
0049; C; 0069; # LATIN CAPITAL LETTER I
0049; T; 0131; # LATIN CAPITAL LETTER I
00DF; F; 0073 0073; # LATIN SMALL LETTER SHARP S
0390; F; 03B9 0308 0301; # GREEK SMALL LETTER IOTA WITH DIALYTIKA AND TONOS
1E9E; F; 0073 0073; # LATIN CAPITAL LETTER SHARP S
1E9E; S; 00DF; # LATIN CAPITAL LETTER SHARP S
";

/// The C and F mappings per code point, sorted; S and T lines add nothing to full folding.
#[test]
fn case_folding_keeps_the_common_and_full_mappings() {
    let folds = case_folding(&file("CaseFolding.txt", CASE_FOLDING)).unwrap();
    let summary: Vec<(u32, Option<u32>, Vec<u32>)> = folds.into_iter().map(|(cp, f)| (cp, f.common, f.full)).collect();
    assert_eq!(
        summary,
        [
            (0x41, Some(0x61), vec![]),
            (0x49, Some(0x69), vec![]),
            (0xDF, None, vec![0x73, 0x73]),
            (0x390, None, vec![0x3B9, 0x308, 0x301]),
            (0x1E9E, None, vec![0x73, 0x73]),
        ]
    );
}

#[test]
fn case_folding_refuses_mappings_of_the_wrong_length_or_status() {
    for line in [
        "0041; C; 0061 0062;",
        "00DF; F; 0073;",
        "0041; S;",
        "0041; X; 0061;",
        "0041; F; 1 2 3 4;",
    ] {
        let error = case_folding(&file("CaseFolding.txt", line)).unwrap_err();
        assert!(error.starts_with("CaseFolding.txt:1: status"), "{line}: {error}");
    }
}

const DERIVED_CORE_PROPERTIES: &str = "\
0041..005A    ; Alphabetic # L&  [26] LATIN CAPITAL LETTER A..LATIN CAPITAL LETTER Z
00AD          ; Default_Ignorable_Code_Point # Cf       SOFT HYPHEN
0300..036F    ; InCB; Extend # Mn [112] COMBINING GRAVE ACCENT..COMBINING LATIN SMALL LETTER X
094D          ; InCB; Linker # Mn       DEVANAGARI SIGN VIRAMA
0915..0939    ; InCB; Consonant # Lo  [37] DEVANAGARI LETTER KA..DEVANAGARI LETTER HA
";

#[test]
fn derived_core_properties_reads_incb_and_default_ignorable() {
    let (incb, ignorable) =
        derived_core_properties(&file("DerivedCoreProperties.txt", DERIVED_CORE_PROPERTIES)).unwrap();
    assert_eq!(incb[0x300], IndicConjunctBreak::Extend);
    assert_eq!(incb[0x36F], IndicConjunctBreak::Extend);
    assert_eq!(incb[0x94D], IndicConjunctBreak::Linker);
    assert_eq!(incb[0x915], IndicConjunctBreak::Consonant);
    assert_eq!(incb[0x41], IndicConjunctBreak::None);
    assert!(ignorable[0xAD]);
    assert!(!ignorable[0x41] && !ignorable[0x300]);
}

#[test]
fn derived_core_properties_refuses_unknown_properties_and_values() {
    let error = derived_core_properties(&file("D.txt", "0041; Shiny # x\n")).unwrap_err();
    assert!(error.contains("D.txt:1: unknown property Shiny"), "{error}");
    let error = derived_core_properties(&file("D.txt", "0300; InCB; Glue\n")).unwrap_err();
    assert!(error.contains("unknown InCB value \"Glue\""), "{error}");
}

const EAST_ASIAN_WIDTH: &str = "\
# DerivedEastAsianWidth-18.0.0.txt
# @missing: 0000..10FFFF; Neutral
# @missing: 3400..4DBF; Wide
# @missing: 20000..2FFFD; Wide
0041..005A     ; Na # L&    [26] LATIN CAPITAL LETTER A..LATIN CAPITAL LETTER Z
3400..4DB5     ; W  # Lo  [438] CJK UNIFIED IDEOGRAPH-3400..CJK UNIFIED IDEOGRAPH-4DB5
4DC0           ; N  # So       HEXAGRAM FOR THE CREATIVE HEAVEN
FF01           ; F  # Po       FULLWIDTH EXCLAMATION MARK
00A1           ; A  # Po       INVERTED EXCLAMATION MARK
FF61           ; H  # Po       HALFWIDTH IDEOGRAPHIC FULL STOP
";

/// `Neutral` by default, the `@missing` ranges `Wide`, and listed values over both.
#[test]
fn east_asian_widths_apply_the_missing_defaults_then_the_values() {
    let column = east_asian_widths(&file("W.txt", EAST_ASIAN_WIDTH)).unwrap();
    assert_eq!(column[0x41], EastAsianWidth::Narrow);
    assert_eq!(column[0x3400], EastAsianWidth::Wide);
    // unlisted, but inside a Wide @missing range
    assert_eq!(column[0x4DBF], EastAsianWidth::Wide);
    assert_eq!(column[0x2_0000], EastAsianWidth::Wide);
    assert_eq!(column[0xFF01], EastAsianWidth::Fullwidth);
    assert_eq!(column[0xA1], EastAsianWidth::Ambiguous);
    assert_eq!(column[0xFF61], EastAsianWidth::Halfwidth);
    assert_eq!(column[0x4DC0], EastAsianWidth::Neutral);
    assert_eq!(column[0x10_FFFF], EastAsianWidth::Neutral);
}

#[test]
fn east_asian_widths_refuse_unknown_values() {
    assert!(
        east_asian_widths(&file("W.txt", "# @missing: 0000..10FFFF; Narrow\n"))
            .unwrap_err()
            .contains("@missing")
    );
    assert!(
        east_asian_widths(&file("W.txt", "0041; Q\n"))
            .unwrap_err()
            .contains("W.txt:1: unknown value Q")
    );
}

/// Known values by name, `Other` for anything else, listed or not.
#[test]
fn grapheme_break_property_reads_known_values_and_others_as_other() {
    let text = "0600..0605 ; Prepend\n000D ; CR\n0300 ; Extend\n1F1E6..1F1FF ; Regional_Indicator\n1100 ; L\n\
                AC00 ; LV\nAC01 ; LVT\n200D ; ZWJ\n0041 ; Something_New\n";
    let column = grapheme_break_property(&file("G.txt", text)).unwrap();
    assert_eq!(column[0x600], OriginalGraphemeBreak::Prepend);
    assert_eq!(column[0x605], OriginalGraphemeBreak::Prepend);
    assert_eq!(column[0xD], OriginalGraphemeBreak::Cr);
    assert_eq!(column[0x300], OriginalGraphemeBreak::Extend);
    assert_eq!(column[0x1F1FF], OriginalGraphemeBreak::RegionalIndicator);
    assert_eq!(column[0x1100], OriginalGraphemeBreak::L);
    assert_eq!(column[0xAC00], OriginalGraphemeBreak::Lv);
    assert_eq!(column[0xAC01], OriginalGraphemeBreak::Lvt);
    assert_eq!(column[0x200D], OriginalGraphemeBreak::Zwj);
    assert_eq!(column[0x41], OriginalGraphemeBreak::Other);
    assert_eq!(column[0x42], OriginalGraphemeBreak::Other);
}

#[test]
fn emoji_data_reads_the_four_properties_and_knows_the_other_two() {
    let text = "1F3FB..1F3FF ; Emoji_Modifier\n261D ; Emoji_Modifier_Base\n1F3FB ; Emoji_Component\n\
                1F600 ; Extended_Pictographic\n1F600 ; Emoji\n1F600 ; Emoji_Presentation\n";
    let data = emoji_data(&file("E.txt", text)).unwrap();
    assert!(data.modifier[0x1F3FB] && data.modifier[0x1F3FF] && !data.modifier[0x1F3FA]);
    assert!(data.modifier_base[0x261D]);
    assert!(data.component[0x1F3FB] && !data.component[0x1F3FC]);
    assert!(data.extended_pictographic[0x1F600] && !data.extended_pictographic[0x261D]);
    let Err(error) = emoji_data(&file("E.txt", "1F600 ; Emoji_Sparkle\n")) else {
        panic!("an unknown property was accepted");
    };
    assert!(error.contains("E.txt:1: unknown property Emoji_Sparkle"), "{error}");
}

/// The text-style lines mark the bases; each emoji-style line must follow its base's text-style line.
#[test]
fn emoji_vs_marks_the_text_style_bases_and_checks_the_pairs() {
    let text = "0023 FE0E  ; text style;  # (1.1) NUMBER SIGN\n0023 FE0F  ; emoji style; # (1.1) NUMBER SIGN\n";
    let column = emoji_vs(&file("V.txt", text)).unwrap();
    assert!(column[0x23]);
    assert!(!column[0x24]);
    let unpaired = "0023 FE0F  ; emoji style; # (1.1) NUMBER SIGN\n";
    assert!(emoji_vs(&file("V.txt", unpaired)).unwrap_err().starts_with("V.txt:1:"));
    assert!(emoji_vs(&file("V.txt", "0023 FE00 ; text style;\n")).is_err());
}

#[test]
fn blocks_marks_the_symbol_blocks_only() {
    let text = "# Blocks-18.0.0.txt\n0000..007F; Basic Latin\n2190..21FF; Arrows\n2700..27BF; Dingbats\n\
                1F680..1F6FF; Transport and Map Symbols\n2200..22FF; Mathematical Operators\n";
    let column = blocks(&file("Blocks.txt", text)).unwrap();
    assert!(column[0x2190] && column[0x21FF] && column[0x2700] && column[0x1F6FF]);
    assert!(!column[0x41] && !column[0x2200] && !column[0x218F]);
    assert!(blocks(&file("Blocks.txt", "2190; \n")).is_ok());
    assert!(
        blocks(&file("Blocks.txt", "zzzz..21FF; Arrows\n"))
            .unwrap_err()
            .starts_with("Blocks.txt:1:")
    );
}
