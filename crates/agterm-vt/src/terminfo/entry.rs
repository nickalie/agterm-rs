//! Ghostty's terminfo entry, `xterm-ghostty`: what `TERM` names in a pane, compiled into the staged terminfo
//! directory, and what XTGETTCAP answers from.
//!
//! Ghostty: `src/terminfo/ghostty.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-terminfo/src/entry.rs` at `RUSTTY_REV` (MIT).
//! Changes: Ghostty's names are restored (rustty's are `xterm-rustty`, `rustty`, `Rustty`), read from
//! [`crate::identity`]; the entry is a constant, as in Ghostty, not rustty's function building one. The capabilities
//! are Ghostty's, in its order, its comments kept (no drift from rustty's oracle to the pin: the two lists are equal).
//! Ghostty's `version`, a Wyhash of the encoded entry, is not ported: only Ghostty's `+ssh` terminfo cache reads it,
//! which is not libghostty-vt's.

use super::source::{Capability, Source, Value};
use crate::identity;

/// Ghostty's terminfo entry.
pub(crate) const GHOSTTY: Source = Source {
    names: &[
        // We support the "xterm-" prefix because some poorly behaved programs
        // use this to detect if the terminal supports 256 colors and other
        // features.
        // HACK: This is a hack on a hack...we use "xterm-ghostty" to prevent
        // vim from breaking, and when we do this as the default we break
        // tcell-based applications (lazygit, aerc, etc). tcell has a bug where
        // the primary terminfo name must be the value of TERM.
        // https://github.com/gdamore/tcell/pull/639 fixes the issue but is not
        // merged yet. Consider switching these in the future.
        identity::TERMINFO_PRIMARY_NAME,
        // The preferred name
        identity::TERMINFO_PREFERRED_NAME,
        // Our "formal" name
        identity::TERMINFO_FORMAL_NAME,
    ],

    // NOTE: These capabilities are super underdocumented and I'm not 100%
    // I've got the list or my understanding of any in this list fully correct.
    // As we learn more, please update the comments to better explain what
    // anything means.
    //
    // I've marked some capabilities as "???" if I don't understand what they
    // mean but I just assume I support since other modern terminals do. In
    // this case, I'd love if anyone could help explain what this means and
    // verify that Ghostty does indeed support it and if not we can fix it.
    capabilities: &[
        // automatic right margin -- when reaching the end of a line, text is
        // wrapped to the next line.
        boolean("am"),
        // background color erase -- screen is erased with the background color
        boolean("bce"),
        // terminal can change color definitions, i.e. we can change the color
        // palette.
        boolean("ccc"),
        // supports changing the window title.
        boolean("hs"),
        // terminal has a meta key
        boolean("km"),
        // terminal will not echo input on the screen on its own
        boolean("mc5i"),
        // safe to move (move what?) while in insert/standout mode. (???)
        boolean("mir"),
        boolean("msgr"),
        // no pad character (???)
        boolean("npc"),
        // newline ignored after 80 cols (???)
        boolean("xenl"),
        // Terminal supports default colors
        boolean("AX"),
        // Tmux "truecolor" mode. Other programs also use this to detect
        // if the terminal supports "truecolor". This means that the terminal
        // can display 24-bit RGB colors.
        boolean("Tc"),
        // Colored underlines. https://sw.kovidgoyal.net/kitty/underlines/
        boolean("Su"),
        // Terminal supports a number of xterm extensions
        boolean("XT"),
        // Full keyboard support using Kitty's keyboard protocol:
        // https://sw.kovidgoyal.net/kitty/keyboard-protocol/
        boolean("fullkbd"),
        // Number of colors in the color palette.
        numeric("colors", 256),
        // Number of columns in a line. Our terminal is variable width on
        // Window resize but this appears to just be the value set by most
        // terminals.
        numeric("cols", 80),
        // Initial tabstop interval.
        numeric("it", 8),
        // Number of lines on a page. Similar to cols this is variable width
        // but this appears to be the value set by most terminals.
        numeric("lines", 24),
        // Number of color pairs on the screen.
        numeric("pairs", 32767),
        // Alternate character set. This is the VT100 alternate character set.
        // I don't know what the value means, I copied this from Kitty and
        // verified with some other terminals (looks similar).
        string(
            "acsc",
            r"++\,\,--..00``aaffgghhiijjkkllmmnnooppqqrrssttuuvvwwxxyyzz{{||}}~~",
        ),
        // Curly, dashed, etc underlines
        string("Smulx", r"\E[4:%p1%dm"),
        // Overline
        string("Smol", r"\E[53m"),
        string("Rmol", r"\E[55m"),
        // Colored underlines
        string(
            "Setulc",
            r"\E[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m",
        ),
        // Cursor styles
        string("Ss", r"\E[%p1%d q"),
        // Cursor style reset (to user configured default)
        string("Se", r"\E[0 q"),
        // OSC 52 Clipboard
        string("Ms", r"\E]52;%p1%s;%p2%s\007"),
        // Synchronized output
        string("Sync", r"\E[?2026%?%p1%{1}%-%tl%eh%;"),
        // Bracketed paste mode
        string("BD", r"\E[?2004l"),
        string("BE", r"\E[?2004h"),
        // Bracketed paste start/end
        string("PS", r"\E[200~"),
        string("PE", r"\E[201~"),
        // Mouse
        string("XM", r"\E[?1006;1000%?%p1%{1}%=%th%el%;"),
        string("xm", r"\E[<%i%p3%d;%p1%d;%p2%d;%?%p4%tM%em%;"),
        // Secondary device attributes request / response
        string("RV", r"\E[>c"),
        string("rv", r"\E\\[[0-9]+;[0-9]+;[0-9]+c"),
        // XTVERSION
        string("XR", r"\E[>0q"),
        string("xr", r"\EP>\\|[ -~]+a\E\\"),
        // DECSLRM (Left/Right Margins)
        string("Enmg", r"\E[?69h"),
        string("Dsmg", r"\E[?69l"),
        string("Clmg", r"\E[s"),
        string("Cmg", r"\E[%i%p1%d;%p2%ds"),
        // Clear screen. E3 is the extension to clear scrollback
        string("clear", r"\E[H\E[2J"),
        string("E3", r"\E[3J"),
        // Focus reporting. Introduced in ncurses 6.4-20231028
        string("fe", r"\E[?1004h"),
        string("fd", r"\E[?1004l"),
        string("kxIN", r"\E[I"),
        string("kxOUT", r"\E[O"),
        // These are all capabilities that should be pretty straightforward
        // and map to input sequences.
        string("bel", r"^G"),
        string("blink", r"\E[5m"),
        string("bold", r"\E[1m"),
        string("cbt", r"\E[Z"),
        string("civis", r"\E[?25l"),
        string("cnorm", r"\E[?12l\E[?25h"),
        string("cr", r"\r"),
        string("csr", r"\E[%i%p1%d;%p2%dr"),
        string("cub", r"\E[%p1%dD"),
        string("cub1", r"^H"),
        string("cud", r"\E[%p1%dB"),
        string("cud1", r"^J"),
        string("cuf", r"\E[%p1%dC"),
        string("cuf1", r"\E[C"),
        string("cup", r"\E[%i%p1%d;%p2%dH"),
        string("cuu", r"\E[%p1%dA"),
        string("cuu1", r"\E[A"),
        string("cvvis", r"\E[?12;25h"),
        string("dch", r"\E[%p1%dP"),
        string("dch1", r"\E[P"),
        string("dim", r"\E[2m"),
        string("dl", r"\E[%p1%dM"),
        string("dl1", r"\E[M"),
        string("dsl", r"\E]2;\007"),
        string("ech", r"\E[%p1%dX"),
        string("ed", r"\E[J"),
        string("el", r"\E[K"),
        string("el1", r"\E[1K"),
        string("flash", r"\E[?5h$<100/>\E[?5l"),
        string("fsl", r"^G"),
        string("home", r"\E[H"),
        string("hpa", r"\E[%i%p1%dG"),
        string("ht", r"^I"),
        string("hts", r"\EH"),
        string("ich", r"\E[%p1%d@"),
        string("ich1", r"\E[@"),
        string("il", r"\E[%p1%dL"),
        string("il1", r"\E[L"),
        string("ind", r"\n"),
        string("indn", r"\E[%p1%dS"),
        string(
            "initc",
            r"\E]4;%p1%d;rgb\:%p2%{255}%*%{1000}%/%2.2X/%p3%{255}%*%{1000}%/%2.2X/%p4%{255}%*%{1000}%/%2.2X\E\\",
        ),
        string("invis", r"\E[8m"),
        string("oc", r"\E]104\007"),
        string("op", r"\E[39;49m"),
        string("rc", r"\E8"),
        string("rep", r"%p1%c\E[%p2%{1}%-%db"),
        string("rev", r"\E[7m"),
        string("ri", r"\EM"),
        string("rin", r"\E[%p1%dT"),
        string("ritm", r"\E[23m"),
        string("rmacs", r"\E(B"),
        string("rmam", r"\E[?7l"),
        string("rmcup", r"\E[?1049l"),
        string("rmir", r"\E[4l"),
        string("rmkx", r"\E[?1l\E>"),
        string("rmso", r"\E[27m"),
        string("rmul", r"\E[24m"),
        string("rmxx", r"\E[29m"),
        string(
            "setab",
            r"\E[%?%p1%{8}%<%t4%p1%d%e%p1%{16}%<%t10%p1%{8}%-%d%e48;5;%p1%d%;m",
        ),
        string(
            "setaf",
            r"\E[%?%p1%{8}%<%t3%p1%d%e%p1%{16}%<%t9%p1%{8}%-%d%e38;5;%p1%d%;m",
        ),
        string("setrgbb", r"\E[48:2:%p1%d:%p2%d:%p3%dm"),
        string("setrgbf", r"\E[38:2:%p1%d:%p2%d:%p3%dm"),
        string(
            "sgr",
            r"%?%p9%t\E(0%e\E(B%;\E[0%?%p6%t;1%;%?%p5%t;2%;%?%p2%t;4%;%?%p1%p3%|%t;7%;%?%p4%t;5%;%?%p7%t;8%;m",
        ),
        string("sgr0", r"\E(B\E[m"),
        string("sitm", r"\E[3m"),
        string("smacs", r"\E(0"),
        string("smam", r"\E[?7h"),
        string("smcup", r"\E[?1049h"),
        string("smir", r"\E[4h"),
        string("smkx", r"\E[?1h\E="),
        string("smso", r"\E[7m"),
        string("smul", r"\E[4m"),
        string("smxx", r"\E[9m"),
        string("tbc", r"\E[3g"),
        string("tsl", r"\E]2;"),
        string("u6", r"\E[%i%d;%dR"),
        string("u7", r"\E[6n"),
        string("u8", r"\E[?%[;0123456789]c"),
        string("u9", r"\E[c"),
        string("vpa", r"\E[%i%p1%dd"),
        //-----------------------------------------------------------
        // Completely unvalidated entries that are blindly copied from
        // other terminals (Kitty, Wezterm, Alacritty) and may or may not
        // actually work with Ghostty. todo is to validate these!
        string("kDC", r"\E[3;2~"),
        string("kDC3", r"\E[3;3~"),
        string("kDC4", r"\E[3;4~"),
        string("kDC5", r"\E[3;5~"),
        string("kDC6", r"\E[3;6~"),
        string("kDC7", r"\E[3;7~"),
        string("kDN", r"\E[1;2B"),
        string("kDN3", r"\E[1;3B"),
        string("kDN4", r"\E[1;4B"),
        string("kDN5", r"\E[1;5B"),
        string("kDN6", r"\E[1;6B"),
        string("kDN7", r"\E[1;7B"),
        string("kEND", r"\E[1;2F"),
        string("kEND3", r"\E[1;3F"),
        string("kEND4", r"\E[1;4F"),
        string("kEND5", r"\E[1;5F"),
        string("kEND6", r"\E[1;6F"),
        string("kEND7", r"\E[1;7F"),
        string("kHOM", r"\E[1;2H"),
        string("kHOM3", r"\E[1;3H"),
        string("kHOM4", r"\E[1;4H"),
        string("kHOM5", r"\E[1;5H"),
        string("kHOM6", r"\E[1;6H"),
        string("kHOM7", r"\E[1;7H"),
        string("kIC", r"\E[2;2~"),
        string("kIC3", r"\E[2;3~"),
        string("kIC4", r"\E[2;4~"),
        string("kIC5", r"\E[2;5~"),
        string("kIC6", r"\E[2;6~"),
        string("kIC7", r"\E[2;7~"),
        string("kLFT", r"\E[1;2D"),
        string("kLFT3", r"\E[1;3D"),
        string("kLFT4", r"\E[1;4D"),
        string("kLFT5", r"\E[1;5D"),
        string("kLFT6", r"\E[1;6D"),
        string("kLFT7", r"\E[1;7D"),
        string("kNXT", r"\E[6;2~"),
        string("kNXT3", r"\E[6;3~"),
        string("kNXT4", r"\E[6;4~"),
        string("kNXT5", r"\E[6;5~"),
        string("kNXT6", r"\E[6;6~"),
        string("kNXT7", r"\E[6;7~"),
        string("kPRV", r"\E[5;2~"),
        string("kPRV3", r"\E[5;3~"),
        string("kPRV4", r"\E[5;4~"),
        string("kPRV5", r"\E[5;5~"),
        string("kPRV6", r"\E[5;6~"),
        string("kPRV7", r"\E[5;7~"),
        string("kRIT", r"\E[1;2C"),
        string("kRIT3", r"\E[1;3C"),
        string("kRIT4", r"\E[1;4C"),
        string("kRIT5", r"\E[1;5C"),
        string("kRIT6", r"\E[1;6C"),
        string("kRIT7", r"\E[1;7C"),
        string("kUP", r"\E[1;2A"),
        string("kUP3", r"\E[1;3A"),
        string("kUP4", r"\E[1;4A"),
        string("kUP5", r"\E[1;5A"),
        string("kUP6", r"\E[1;6A"),
        string("kUP7", r"\E[1;7A"),
        string("kbs", r"^?"),
        string("kcbt", r"\E[Z"),
        string("kcub1", r"\EOD"),
        string("kcud1", r"\EOB"),
        string("kcuf1", r"\EOC"),
        string("kcuu1", r"\EOA"),
        string("kdch1", r"\E[3~"),
        string("kend", r"\EOF"),
        string("kent", r"\EOM"),
        string("kf1", r"\EOP"),
        string("kf10", r"\E[21~"),
        string("kf11", r"\E[23~"),
        string("kf12", r"\E[24~"),
        string("kf13", r"\E[1;2P"),
        string("kf14", r"\E[1;2Q"),
        string("kf15", r"\E[1;2R"),
        string("kf16", r"\E[1;2S"),
        string("kf17", r"\E[15;2~"),
        string("kf18", r"\E[17;2~"),
        string("kf19", r"\E[18;2~"),
        string("kf2", r"\EOQ"),
        string("kf20", r"\E[19;2~"),
        string("kf21", r"\E[20;2~"),
        string("kf22", r"\E[21;2~"),
        string("kf23", r"\E[23;2~"),
        string("kf24", r"\E[24;2~"),
        string("kf25", r"\E[1;5P"),
        string("kf26", r"\E[1;5Q"),
        string("kf27", r"\E[1;5R"),
        string("kf28", r"\E[1;5S"),
        string("kf29", r"\E[15;5~"),
        string("kf3", r"\EOR"),
        string("kf30", r"\E[17;5~"),
        string("kf31", r"\E[18;5~"),
        string("kf32", r"\E[19;5~"),
        string("kf33", r"\E[20;5~"),
        string("kf34", r"\E[21;5~"),
        string("kf35", r"\E[23;5~"),
        string("kf36", r"\E[24;5~"),
        string("kf37", r"\E[1;6P"),
        string("kf38", r"\E[1;6Q"),
        string("kf39", r"\E[1;6R"),
        string("kf4", r"\EOS"),
        string("kf40", r"\E[1;6S"),
        string("kf41", r"\E[15;6~"),
        string("kf42", r"\E[17;6~"),
        string("kf43", r"\E[18;6~"),
        string("kf44", r"\E[19;6~"),
        string("kf45", r"\E[20;6~"),
        string("kf46", r"\E[21;6~"),
        string("kf47", r"\E[23;6~"),
        string("kf48", r"\E[24;6~"),
        string("kf49", r"\E[1;3P"),
        string("kf5", r"\E[15~"),
        string("kf50", r"\E[1;3Q"),
        string("kf51", r"\E[1;3R"),
        string("kf52", r"\E[1;3S"),
        string("kf53", r"\E[15;3~"),
        string("kf54", r"\E[17;3~"),
        string("kf55", r"\E[18;3~"),
        string("kf56", r"\E[19;3~"),
        string("kf57", r"\E[20;3~"),
        string("kf58", r"\E[21;3~"),
        string("kf59", r"\E[23;3~"),
        string("kf6", r"\E[17~"),
        string("kf60", r"\E[24;3~"),
        string("kf61", r"\E[1;4P"),
        string("kf62", r"\E[1;4Q"),
        string("kf63", r"\E[1;4R"),
        string("kf7", r"\E[18~"),
        string("kf8", r"\E[19~"),
        string("kf9", r"\E[20~"),
        string("khome", r"\EOH"),
        string("kich1", r"\E[2~"),
        string("kind", r"\E[1;2B"),
        string("kmous", r"\E[<"),
        string("knp", r"\E[6~"),
        string("kpp", r"\E[5~"),
        string("kri", r"\E[1;2A"),
        string("rs1", r"\E]\E\\\Ec"),
        string("sc", r"\E7"),
    ],
};

const fn boolean(name: &'static str) -> Capability {
    Capability {
        name,
        value: Value::Boolean,
    }
}

const fn numeric(name: &'static str, value: u32) -> Capability {
    Capability {
        name,
        value: Value::Numeric(value),
    }
}

const fn string(name: &'static str, value: &'static str) -> Capability {
    Capability {
        name,
        value: Value::String(value),
    }
}

#[cfg(test)]
mod tests;
