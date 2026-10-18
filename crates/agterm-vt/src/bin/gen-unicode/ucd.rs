//! The UCD files the tables are built from, parsed into one column per property, indexed by code point.
//!
//! uucode: `src/components.zig` at `UUCODE_REV` (`build/pins.env`, MIT): `parseCp`, `parseRange`, `trim` and the
//! build components `UnicodeData`, `CaseFolding`, `DerivedCoreProperties`, `EastAsianWidth`, `GraphemeBreak`,
//! `EmojiData`, `EmojiVs` and `Blocks`, with the property-name maps they use (`types.zig` for the value enums).
//! Changes: only the fields Ghostty's tables read are kept, and a column per field stands in for uucode's rows. Where
//! uucode's build hits `unreachable` (an unknown name, a malformed line) or an `inlineAssert`, this returns an error
//! naming the file and line, so a UCD that breaks an assumption fails the generator instead of being read on.
//! `UnicodeData.txt` is read up to its general category: the checks uucode makes on the later fields (the
//! decomposition's type, the numeric values, the case mappings), which no table reads, are not made. Blocks
//! keeps only whether a code point is in one of the blocks `uucode_config.zig`'s `is_symbol` names, so it cannot
//! refuse an unknown block name as uucode's full block map does. Every file that names its UCD version must name the
//! pinned one.

use std::fs;
use std::path::{Path, PathBuf};

/// The size of the code point space uucode has data for (`config.num_code_points`, `max_code_point + 1`).
pub(crate) const NUM_CODE_POINTS: usize = 0x11_0000;

/// U+200C ZERO WIDTH NON-JOINER (`config.zero_width_non_joiner`).
pub(crate) const ZERO_WIDTH_NON_JOINER: u32 = 0x200C;

/// U+200D ZERO WIDTH JOINER (`config.zero_width_joiner`).
pub(crate) const ZERO_WIDTH_JOINER: u32 = 0x200D;

/// The generator's error: what went wrong, with the file and line when a UCD file is at fault.
pub(crate) type Result<T> = std::result::Result<T, String>;

/// uucode's `types.GeneralCategory`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GeneralCategory {
    LetterUppercase,
    LetterLowercase,
    LetterTitlecase,
    LetterModifier,
    LetterOther,
    MarkNonspacing,
    MarkSpacingCombining,
    MarkEnclosing,
    NumberDecimalDigit,
    NumberLetter,
    NumberOther,
    PunctuationConnector,
    PunctuationDash,
    PunctuationOpen,
    PunctuationClose,
    PunctuationInitialQuote,
    PunctuationFinalQuote,
    PunctuationOther,
    SymbolMath,
    SymbolCurrency,
    SymbolModifier,
    SymbolOther,
    SeparatorSpace,
    SeparatorLine,
    SeparatorParagraph,
    OtherControl,
    OtherFormat,
    OtherSurrogate,
    OtherPrivateUse,
    OtherNotAssigned,
}

/// `general_category_map`.
fn general_category(name: &str) -> Option<GeneralCategory> {
    use GeneralCategory as G;
    Some(match name {
        "Lu" => G::LetterUppercase,
        "Ll" => G::LetterLowercase,
        "Lt" => G::LetterTitlecase,
        "Lm" => G::LetterModifier,
        "Lo" => G::LetterOther,
        "Mn" => G::MarkNonspacing,
        "Mc" => G::MarkSpacingCombining,
        "Me" => G::MarkEnclosing,
        "Nd" => G::NumberDecimalDigit,
        "Nl" => G::NumberLetter,
        "No" => G::NumberOther,
        "Pc" => G::PunctuationConnector,
        "Pd" => G::PunctuationDash,
        "Ps" => G::PunctuationOpen,
        "Pe" => G::PunctuationClose,
        "Pi" => G::PunctuationInitialQuote,
        "Pf" => G::PunctuationFinalQuote,
        "Po" => G::PunctuationOther,
        "Sm" => G::SymbolMath,
        "Sc" => G::SymbolCurrency,
        "Sk" => G::SymbolModifier,
        "So" => G::SymbolOther,
        "Zs" => G::SeparatorSpace,
        "Zl" => G::SeparatorLine,
        "Zp" => G::SeparatorParagraph,
        "Cc" => G::OtherControl,
        "Cf" => G::OtherFormat,
        "Cs" => G::OtherSurrogate,
        "Co" => G::OtherPrivateUse,
        "Cn" => G::OtherNotAssigned,
        _ => return None,
    })
}

/// uucode's `types.EastAsianWidth`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EastAsianWidth {
    Neutral,
    Fullwidth,
    Halfwidth,
    Wide,
    Narrow,
    Ambiguous,
}

/// `east_asian_width_map`.
fn east_asian_width(name: &str) -> Option<EastAsianWidth> {
    use EastAsianWidth as W;
    Some(match name {
        "F" => W::Fullwidth,
        "H" => W::Halfwidth,
        "W" => W::Wide,
        "Na" => W::Narrow,
        "A" => W::Ambiguous,
        "N" => W::Neutral,
        _ => return None,
    })
}

/// uucode's `types.OriginalGraphemeBreak`: `Grapheme_Cluster_Break` as `GraphemeBreakProperty.txt` gives it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OriginalGraphemeBreak {
    Other,
    Prepend,
    Cr,
    Lf,
    Control,
    Extend,
    RegionalIndicator,
    SpacingMark,
    L,
    V,
    T,
    Lv,
    Lvt,
    Zwj,
}

/// `grapheme_break_property_map`; uucode reads an unknown value as `other`.
fn original_grapheme_break(name: &str) -> OriginalGraphemeBreak {
    use OriginalGraphemeBreak as B;
    match name {
        "Prepend" => B::Prepend,
        "CR" => B::Cr,
        "LF" => B::Lf,
        "Control" => B::Control,
        "Extend" => B::Extend,
        "Regional_Indicator" => B::RegionalIndicator,
        "SpacingMark" => B::SpacingMark,
        "L" => B::L,
        "V" => B::V,
        "T" => B::T,
        "LV" => B::Lv,
        "LVT" => B::Lvt,
        "ZWJ" => B::Zwj,
        _ => B::Other,
    }
}

/// uucode's `types.IndicConjunctBreak`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum IndicConjunctBreak {
    None,
    Linker,
    Consonant,
    Extend,
}

/// The `DerivedCoreProperties.txt` properties uucode knows (`derived_core_property_map`), of which the tables read
/// two.
const DERIVED_CORE_PROPERTIES: [&str; 20] = [
    "Math",
    "Alphabetic",
    "Lowercase",
    "Uppercase",
    "Cased",
    "Case_Ignorable",
    "Changes_When_Lowercased",
    "Changes_When_Uppercased",
    "Changes_When_Titlecased",
    "Changes_When_Casefolded",
    "Changes_When_Casemapped",
    "ID_Start",
    "ID_Continue",
    "XID_Start",
    "XID_Continue",
    "Default_Ignorable_Code_Point",
    "Grapheme_Extend",
    "Grapheme_Base",
    "Grapheme_Link",
    "InCB",
];

/// The `emoji-data.txt` properties uucode knows (`emoji_data_property_map`), of which the tables read four.
const EMOJI_PROPERTIES: [&str; 6] = [
    "Emoji",
    "Emoji_Presentation",
    "Emoji_Modifier",
    "Emoji_Modifier_Base",
    "Emoji_Component",
    "Extended_Pictographic",
];

/// The blocks whose code points `uucode_config.zig`'s `IsSymbolComponent` counts as symbols, by their `Blocks.txt`
/// names.
const SYMBOL_BLOCKS: [&str; 8] = [
    "Arrows",
    "Dingbats",
    "Emoticons",
    "Miscellaneous Symbols",
    "Enclosed Alphanumerics",
    "Enclosed Alphanumeric Supplement",
    "Miscellaneous Symbols and Pictographs",
    "Transport and Map Symbols",
];

/// One code point's case folding as `CaseFolding.txt` gives it: its status C mapping and its status F mapping (S
/// and T are parsed and checked, but full folding does not read them).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct CaseFolding {
    pub(crate) common: Option<u32>,
    pub(crate) full: Vec<u32>,
}

/// The parsed UCD: one column per field, each `NUM_CODE_POINTS` long.
pub(crate) struct Ucd {
    pub(crate) general_category: Vec<GeneralCategory>,
    pub(crate) east_asian_width: Vec<EastAsianWidth>,
    pub(crate) original_grapheme_break: Vec<OriginalGraphemeBreak>,
    pub(crate) indic_conjunct_break: Vec<IndicConjunctBreak>,
    pub(crate) is_default_ignorable: Vec<bool>,
    pub(crate) is_emoji_modifier: Vec<bool>,
    pub(crate) is_emoji_modifier_base: Vec<bool>,
    pub(crate) is_emoji_component: Vec<bool>,
    pub(crate) is_extended_pictographic: Vec<bool>,
    pub(crate) is_emoji_vs_base: Vec<bool>,
    pub(crate) in_symbol_block: Vec<bool>,
    /// The code points `CaseFolding.txt` has a C or F line for, sorted, with those mappings.
    pub(crate) case_folding: Vec<(u32, CaseFolding)>,
}

/// A UCD file's text, with its name for errors.
pub(crate) struct UcdFile {
    pub(crate) name: String,
    pub(crate) text: String,
}

impl UcdFile {
    /// Reads `name` under `dir` and checks the UCD version it names, if it names one.
    pub(crate) fn read(dir: &Path, name: &str, version: &str) -> Result<Self> {
        let path: PathBuf = dir.join(name);
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file = Self {
            name: name.to_owned(),
            text,
        };
        file.check_version(version)?;
        Ok(file)
    }

    /// A file's version is on its first lines: `# Name-18.0.0.txt` (the UCD's own files) or `# Version: 18.0.0` (the
    /// emoji files). `UnicodeData.txt` has no header.
    fn check_version(&self, version: &str) -> Result<()> {
        for line in self.text.lines().take(10) {
            let Some(comment) = line.strip_prefix("# ") else {
                continue;
            };
            let named = comment.strip_prefix("Version: ").or_else(|| {
                let stem = comment.strip_suffix(".txt")?;
                Some(&stem[stem.rfind('-')? + 1..])
            });
            if let Some(named) = named.filter(|v| v.starts_with(|c: char| c.is_ascii_digit())) {
                return if named == version {
                    Ok(())
                } else {
                    Err(format!("{}: UCD {named}, but the pin is UCD {version}", self.name))
                };
            }
        }
        Ok(())
    }

    /// The data lines with their 1-based line numbers: comments (from `#`) and surrounding whitespace cut, empty
    /// lines dropped (uucode's `trim`).
    pub(crate) fn data_lines(&self) -> impl Iterator<Item = (usize, &str)> {
        self.text
            .split('\n')
            .enumerate()
            .map(|(i, line)| (i + 1, trim(line)))
            .filter(|(_, line)| !line.is_empty())
    }

    /// An error at `line` of this file.
    pub(crate) fn error(&self, line: usize, what: impl std::fmt::Display) -> String {
        format!("{}:{line}: {what}", self.name)
    }
}

/// uucode's `trim`: the line up to a `#`, without surrounding spaces, tabs and carriage returns.
pub(crate) fn trim(line: &str) -> &str {
    let line = line.find('#').map_or(line, |at| &line[..at]);
    line.trim_matches([' ', '\t', '\r'])
}

/// The `;`-separated fields of a data line, each trimmed as uucode trims them.
fn fields(line: &str) -> impl Iterator<Item = &str> {
    line.split(';').map(|field| field.trim_matches([' ', '\t', '\r']))
}

/// uucode's `parseCp`: a hexadecimal code point, which must be inside the code point space.
pub(crate) fn parse_cp(text: &str) -> Result<u32> {
    let cp = u32::from_str_radix(text, 16).map_err(|e| format!("bad code point {text:?}: {e}"))?;
    if usize::try_from(cp).is_ok_and(|cp| cp < NUM_CODE_POINTS) {
        Ok(cp)
    } else {
        Err(format!("code point {text} is past U+10FFFF"))
    }
}

/// uucode's `parseRange`: `XXXX..YYYY` or a single code point, as the half-open range of indices it covers.
pub(crate) fn parse_range(text: &str) -> Result<std::ops::Range<usize>> {
    let (start, end) = if let Some((start, end)) = text.split_once("..") {
        (parse_cp(start)?, parse_cp(end)?)
    } else {
        let cp = parse_cp(text)?;
        (cp, cp)
    };
    if start > end {
        return Err(format!("range {text} ends before it starts"));
    }
    Ok(index(start)..index(end) + 1)
}

/// A code point as a column index (every parsed code point is below `NUM_CODE_POINTS`).
pub(crate) fn index(cp: u32) -> usize {
    usize::try_from(cp).unwrap_or(usize::MAX)
}

/// The next field of a line, or an error naming what is missing.
fn next_field<'a>(
    file: &UcdFile,
    line: usize,
    parts: &mut impl Iterator<Item = &'a str>,
    what: &str,
) -> Result<&'a str> {
    parts.next().ok_or_else(|| file.error(line, format!("no {what} field")))
}

/// The code point range and the value of a `range ; value` line.
fn range_and_value<'a>(file: &UcdFile, line: usize, text: &'a str) -> Result<(std::ops::Range<usize>, &'a str)> {
    let mut parts = fields(text);
    let range = next_field(file, line, &mut parts, "code point")?;
    let range = parse_range(range).map_err(|e| file.error(line, e))?;
    let value = next_field(file, line, &mut parts, "property")?;
    Ok((range, value))
}

impl Ucd {
    /// Parses every file the tables need from the UCD directory `dir`, holding each to `version`.
    pub(crate) fn read(dir: &Path, version: &str) -> Result<Self> {
        let read = |name: &str| UcdFile::read(dir, name, version);
        let (indic_conjunct_break, is_default_ignorable) =
            derived_core_properties(&read("DerivedCoreProperties.txt")?)?;
        let emoji = emoji_data(&read("emoji/emoji-data.txt")?)?;
        Ok(Self {
            general_category: unicode_data(&read("UnicodeData.txt")?)?,
            east_asian_width: east_asian_widths(&read("extracted/DerivedEastAsianWidth.txt")?)?,
            original_grapheme_break: grapheme_break_property(&read("auxiliary/GraphemeBreakProperty.txt")?)?,
            indic_conjunct_break,
            is_default_ignorable,
            is_emoji_modifier: emoji.modifier,
            is_emoji_modifier_base: emoji.modifier_base,
            is_emoji_component: emoji.component,
            is_extended_pictographic: emoji.extended_pictographic,
            is_emoji_vs_base: emoji_vs(&read("emoji/emoji-variation-sequences.txt")?)?,
            in_symbol_block: blocks(&read("Blocks.txt")?)?,
            case_folding: case_folding(&read("CaseFolding.txt")?)?,
        })
    }
}

#[cfg(test)]
impl Ucd {
    /// A UCD where every code point has the files' defaults, except `Lo` as its category, so it is one cell wide.
    pub(crate) fn blank() -> Self {
        Self {
            general_category: vec![GeneralCategory::LetterOther; NUM_CODE_POINTS],
            east_asian_width: vec![EastAsianWidth::Neutral; NUM_CODE_POINTS],
            original_grapheme_break: vec![OriginalGraphemeBreak::Other; NUM_CODE_POINTS],
            indic_conjunct_break: vec![IndicConjunctBreak::None; NUM_CODE_POINTS],
            is_default_ignorable: vec![false; NUM_CODE_POINTS],
            is_emoji_modifier: vec![false; NUM_CODE_POINTS],
            is_emoji_modifier_base: vec![false; NUM_CODE_POINTS],
            is_emoji_component: vec![false; NUM_CODE_POINTS],
            is_extended_pictographic: vec![false; NUM_CODE_POINTS],
            is_emoji_vs_base: vec![false; NUM_CODE_POINTS],
            in_symbol_block: vec![false; NUM_CODE_POINTS],
            case_folding: Vec::new(),
        }
    }
}

/// `UnicodeData`'s `general_category`: field 2 of each line, a `<..., First>`/`<..., Last>` pair covering its
/// range, and `Cn` for every code point the file does not list.
pub(crate) fn unicode_data(file: &UcdFile) -> Result<Vec<GeneralCategory>> {
    let mut column = Vec::with_capacity(NUM_CODE_POINTS);
    let mut range: Option<GeneralCategory> = None;
    for (line, text) in file.data_lines() {
        let mut parts = text.split(';');
        let cp = parse_cp(next_field(file, line, &mut parts, "code point")?).map_err(|e| file.error(line, e))?;
        let cp = index(cp);
        if cp < column.len() {
            return Err(file.error(line, "code points out of order"));
        }
        // fill the gap up to this line: the range it closes, else unassigned code points
        column.resize(cp, range.unwrap_or(GeneralCategory::OtherNotAssigned));
        let name = next_field(file, line, &mut parts, "name")?;
        if let Some(category) = range.take() {
            if !name.ends_with("Last>") {
                return Err(file.error(line, "a range's First line is not followed by its Last line"));
            }
            column.push(category);
            continue;
        }
        let category = next_field(file, line, &mut parts, "general category")?;
        let category = general_category(category)
            .ok_or_else(|| file.error(line, format!("unknown general category {category}")))?;
        if name.ends_with("First>") {
            range = Some(category);
        }
        column.push(category);
    }
    if range.is_some() {
        return Err(format!("{}: the file ends inside a range", file.name));
    }
    column.resize(NUM_CODE_POINTS, GeneralCategory::OtherNotAssigned);
    Ok(column)
}

/// `CaseFolding`: the C and F mappings of each code point, after checking that S, C and T map to one code point and
/// F to two or three.
pub(crate) fn case_folding(file: &UcdFile) -> Result<Vec<(u32, CaseFolding)>> {
    let mut folds: std::collections::BTreeMap<u32, CaseFolding> = std::collections::BTreeMap::new();
    for (line, text) in file.data_lines() {
        let mut parts = fields(text);
        let cp = parse_cp(next_field(file, line, &mut parts, "code point")?).map_err(|e| file.error(line, e))?;
        let status = next_field(file, line, &mut parts, "status")?;
        let mapping = parts
            .next()
            .unwrap_or("")
            .split(' ')
            .filter(|part| !part.is_empty())
            .map(parse_cp)
            .collect::<Result<Vec<u32>>>()
            .map_err(|e| file.error(line, e))?;
        let single = mapping.len() == 1;
        match status.bytes().next() {
            Some(b'S' | b'T') if single => {}
            Some(b'C') if single => folds.entry(cp).or_default().common = Some(mapping[0]),
            Some(b'F') if (2..=3).contains(&mapping.len()) => folds.entry(cp).or_default().full = mapping,
            _ => return Err(file.error(line, format!("status {status:?} with {} code points", mapping.len()))),
        }
    }
    Ok(folds.into_iter().collect())
}

/// `DerivedCoreProperties`: the two properties the tables read, `InCB` and `Default_Ignorable_Code_Point`, after
/// checking that every property is one uucode knows and every `InCB` value is known.
pub(crate) fn derived_core_properties(file: &UcdFile) -> Result<(Vec<IndicConjunctBreak>, Vec<bool>)> {
    let mut incb = vec![IndicConjunctBreak::None; NUM_CODE_POINTS];
    let mut default_ignorable = vec![false; NUM_CODE_POINTS];
    for (line, text) in file.data_lines() {
        let (range, property) = range_and_value(file, line, text)?;
        if !DERIVED_CORE_PROPERTIES.contains(&property) {
            return Err(file.error(line, format!("unknown property {property}")));
        }
        match property {
            "InCB" => {
                let value = fields(text).nth(2).unwrap_or("");
                let value = match value {
                    "Linker" => IndicConjunctBreak::Linker,
                    "Consonant" => IndicConjunctBreak::Consonant,
                    "Extend" => IndicConjunctBreak::Extend,
                    _ => return Err(file.error(line, format!("unknown InCB value {value:?}"))),
                };
                incb[range].fill(value);
            }
            "Default_Ignorable_Code_Point" => default_ignorable[range].fill(true),
            _ => {}
        }
    }
    Ok((incb, default_ignorable))
}

/// `EastAsianWidth`: `Neutral` by default, the `@missing` lines' `Wide` defaults, then the listed values, in file
/// order.
pub(crate) fn east_asian_widths(file: &UcdFile) -> Result<Vec<EastAsianWidth>> {
    let mut column = vec![EastAsianWidth::Neutral; NUM_CODE_POINTS];
    for (i, raw) in file.text.split('\n').enumerate() {
        let line = i + 1;
        let raw = raw.trim_matches([' ', '\t', '\r']);
        if let Some(missing) = raw.strip_prefix("# @missing:") {
            let (range, value) = range_and_value(file, line, missing)?;
            match value {
                "Neutral" => {}
                "Wide" => column[range].fill(EastAsianWidth::Wide),
                _ => return Err(file.error(line, format!("unknown @missing value {value}"))),
            }
            continue;
        }
        let text = trim(raw);
        if text.is_empty() {
            continue;
        }
        let (range, value) = range_and_value(file, line, text)?;
        let width = east_asian_width(value).ok_or_else(|| file.error(line, format!("unknown value {value}")))?;
        column[range].fill(width);
    }
    Ok(column)
}

/// `GraphemeBreak`: `Grapheme_Cluster_Break`, `Other` where the file lists nothing.
pub(crate) fn grapheme_break_property(file: &UcdFile) -> Result<Vec<OriginalGraphemeBreak>> {
    let mut column = vec![OriginalGraphemeBreak::Other; NUM_CODE_POINTS];
    for (line, text) in file.data_lines() {
        let (range, value) = range_and_value(file, line, text)?;
        column[range].fill(original_grapheme_break(value));
    }
    Ok(column)
}

/// The four `emoji-data.txt` properties the tables read.
pub(crate) struct EmojiData {
    pub(crate) modifier: Vec<bool>,
    pub(crate) modifier_base: Vec<bool>,
    pub(crate) component: Vec<bool>,
    pub(crate) extended_pictographic: Vec<bool>,
}

/// `EmojiData`, after checking that every property is one uucode knows.
pub(crate) fn emoji_data(file: &UcdFile) -> Result<EmojiData> {
    let mut data = EmojiData {
        modifier: vec![false; NUM_CODE_POINTS],
        modifier_base: vec![false; NUM_CODE_POINTS],
        component: vec![false; NUM_CODE_POINTS],
        extended_pictographic: vec![false; NUM_CODE_POINTS],
    };
    for (line, text) in file.data_lines() {
        let (range, property) = range_and_value(file, line, text)?;
        let column = match property {
            "Emoji_Modifier" => &mut data.modifier,
            "Emoji_Modifier_Base" => &mut data.modifier_base,
            "Emoji_Component" => &mut data.component,
            "Extended_Pictographic" => &mut data.extended_pictographic,
            _ if EMOJI_PROPERTIES.contains(&property) => continue,
            _ => return Err(file.error(line, format!("unknown property {property}"))),
        };
        column[range].fill(true);
    }
    Ok(data)
}

/// `EmojiVs`: the bases of the text-style (VS15) sequences. Every emoji-style (VS16) line must follow its base's
/// text-style line: the two styles come in pairs.
pub(crate) fn emoji_vs(file: &UcdFile) -> Result<Vec<bool>> {
    let mut column = vec![false; NUM_CODE_POINTS];
    for (line, text) in file.data_lines() {
        let mut parts = text.split(' ');
        let cp = parse_cp(next_field(file, line, &mut parts, "base")?).map_err(|e| file.error(line, e))?;
        let vs = parse_cp(next_field(file, line, &mut parts, "selector")?).map_err(|e| file.error(line, e))?;
        match vs {
            0xFE0E => column[index(cp)] = true,
            0xFE0F if column[index(cp)] => {}
            _ => return Err(file.error(line, "not a VS15 line, nor a VS16 line after its base's VS15 line")),
        }
    }
    Ok(column)
}

/// `Blocks`, kept to whether a code point is in one of the symbol blocks.
pub(crate) fn blocks(file: &UcdFile) -> Result<Vec<bool>> {
    let mut column = vec![false; NUM_CODE_POINTS];
    for (line, text) in file.data_lines() {
        let (range, name) = range_and_value(file, line, text)?;
        if SYMBOL_BLOCKS.contains(&name) {
            column[range].fill(true);
        }
    }
    Ok(column)
}

#[cfg(test)]
mod tests;
