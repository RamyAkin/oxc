use oxc_data_structures::assert_unchecked;

use crate::{
    config::{LexerConfig as Config, NoTokensLexerConfig, RuntimeLexerConfig, TokensLexerConfig},
    diagnostics,
};

use super::{Kind, Lexer};

#[derive(Clone, Copy)]
#[repr(u8)]
enum TokenClass {
    Slow,
    Identifier,
    Punctuation,
    Digit,
    Quote,
    Operator,
}

#[expect(clippy::cast_possible_truncation, reason = "The table loop only visits 0..256")]
const TOKEN_CLASSES: [TokenClass; 256] = {
    let mut out = [TokenClass::Slow; 256];
    let mut i = 0;
    while i < 256 {
        out[i] = match i as u8 {
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => TokenClass::Identifier,
            b'0'..=b'9' => TokenClass::Digit,
            b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^' | b'!' | b'?' | b'=' | b'.'
            | b'<' => TokenClass::Operator,
            b'(' | b')' | b';' | b'{' | b'}' | b'[' | b']' | b',' | b':' | b'~' | b'@' | b'>' => {
                TokenClass::Punctuation
            }
            b'\'' | b'"' => TokenClass::Quote,
            _ => TokenClass::Slow,
        };
        i += 1;
    }
    out
};

pub(super) const PUNCTUATION_KINDS: [Kind; 256] = {
    let mut out = [Kind::Eof; 256];
    out[b'+' as usize] = Kind::Plus;
    out[b'-' as usize] = Kind::Minus;
    out[b'*' as usize] = Kind::Star;
    out[b'/' as usize] = Kind::Slash;
    out[b'%' as usize] = Kind::Percent;
    out[b'&' as usize] = Kind::Amp;
    out[b'|' as usize] = Kind::Pipe;
    out[b'^' as usize] = Kind::Caret;
    out[b'!' as usize] = Kind::Bang;
    out[b'?' as usize] = Kind::Question;
    out[b'=' as usize] = Kind::Eq;
    out[b'.' as usize] = Kind::Dot;
    out[b'<' as usize] = Kind::LAngle;
    out[b'(' as usize] = Kind::LParen;
    out[b')' as usize] = Kind::RParen;
    out[b';' as usize] = Kind::Semicolon;
    out[b'{' as usize] = Kind::LCurly;
    out[b'}' as usize] = Kind::RCurly;
    out[b'[' as usize] = Kind::LBrack;
    out[b']' as usize] = Kind::RBrack;
    out[b',' as usize] = Kind::Comma;
    out[b':' as usize] = Kind::Colon;
    out[b'~' as usize] = Kind::Tilde;
    out[b'@' as usize] = Kind::At;
    // The parser re-lexes `>=` and shifts from this single-byte token.
    out[b'>' as usize] = Kind::RAngle;
    out
};

// Fold the ASCII operator alphabet into five bits; the table initializer checks for collisions.
#[inline]
pub(super) const fn operator_index(first: u8, second: u8) -> usize {
    let first = (first ^ (first >> 3)) & 31;
    let second = (second ^ (second >> 3)) & 31;
    ((first as usize) << 5) | second as usize
}

pub(super) const OPERATOR_PAIRS: [Kind; 32 * 32] = {
    let mut table = [Kind::Eof; 32 * 32];
    let mut i = 0;
    while i < Kind::VARIANTS.len() {
        let kind = Kind::VARIANTS[i];
        let bytes = kind.to_str().as_bytes();
        if bytes.len() == 2 && matches!(TOKEN_CLASSES[bytes[0] as usize], TokenClass::Operator) {
            let index = operator_index(bytes[0], bytes[1]);
            assert!(matches!(table[index], Kind::Eof), "Operator index collision");
            table[index] = kind;
        }
        i += 1;
    }
    table
};

pub(super) const OPERATORS_WITH_EQ: [Kind; 256] = {
    let mut table = [Kind::Eof; 256];
    let mut i = 0;
    while i < Kind::VARIANTS.len() {
        let kind = Kind::VARIANTS[i];
        let bytes = kind.to_str().as_bytes();
        if bytes.len() == 3
            && bytes[2] == b'='
            && matches!(TOKEN_CLASSES[bytes[0] as usize], TokenClass::Operator)
        {
            let pair = OPERATOR_PAIRS[operator_index(bytes[0], bytes[1])];
            table[pair as usize] = kind;
        }
        i += 1;
    }
    table
};

impl<C: Config> Lexer<'_, C> {
    /// Handle next byte of source, returning `None` for trivia.
    /// Known token classes return `Some` directly, avoiding a `Kind::Skip` check after each token.
    ///
    /// # SAFETY
    ///
    /// * Lexer must not be at end of file.
    /// * `byte` must be next byte of source code, corresponding to current position of `lexer.source`.
    // `#[inline(always)]` to ensure is inlined into `read_next_token`
    #[expect(clippy::inline_always)]
    #[inline(always)]
    pub(super) unsafe fn handle_byte(&mut self, byte: u8) -> Option<Kind> {
        match TOKEN_CLASSES[byte as usize] {
            // SAFETY: The dispatch table identifies ASCII identifier starts.
            TokenClass::Identifier => Some(unsafe { self.identifier_name_handler() }),
            TokenClass::Punctuation => {
                // SAFETY: The caller guarantees that this single ASCII byte exists.
                self.source.set_position(unsafe { self.source.position().add(1) });
                Some(PUNCTUATION_KINDS[byte as usize])
            }
            TokenClass::Digit => {
                // SAFETY: The caller guarantees that the first digit exists.
                self.source.set_position(unsafe { self.source.position().add(1) });
                Some(if byte == b'0' {
                    self.read_zero()
                } else {
                    self.decimal_literal_after_first_digit()
                })
            }
            // SAFETY: The dispatch table identifies ASCII quote delimiters.
            TokenClass::Quote => Some(unsafe { self.read_string_literal(byte) }),
            // SAFETY: The dispatch table identifies ASCII operator bytes.
            TokenClass::Operator => unsafe { self.read_operator(byte) },
            TokenClass::Slow => {
                let byte_handlers = self.config.byte_handlers();
                // SAFETY: The caller guarantees the byte handler's invariants.
                let kind = unsafe { byte_handlers[byte as usize](self) };
                if kind == Kind::Skip { None } else { Some(kind) }
            }
        }
    }
}

pub type ByteHandler<C> = unsafe fn(&mut Lexer<'_, C>) -> Kind;
pub type ByteHandlers<C> = [ByteHandler<C>; 256];

/// Table for bytes outside the directly dispatched token classes.
/// Entries handled directly by `handle_byte`, and invalid UTF-8 starts, use `UER`.
/// <https://github.com/ratel-rust/ratel-core/blob/e55a1310ba69a3f5ce2a9a6eef643feced02ac08/ratel/src/lexer/mod.rs>
#[rustfmt::skip]
macro_rules! byte_handlers {
    () => {
        [
        //  0    1    2    3    4    5    6    7    8    9    A    B    C    D    E    F    //
            ERR, ERR, ERR, ERR, ERR, ERR, ERR, ERR, ERR, SPS, LIN, ISP, ISP, LIN, ERR, ERR, // 0
            ERR, ERR, ERR, ERR, ERR, ERR, ERR, ERR, ERR, ERR, ERR, ERR, ERR, ERR, ERR, ERR, // 1
            SPS, UER, UER, HAS, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, // 2
            UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, // 3
            UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, // 4
            UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, ESC, UER, UER, UER, // 5
            TPL, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, // 6
            UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, ERR, // 7
            UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, // 8
            UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, // 9
            UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, // A
            UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, // B
            UER, UER, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, // C
            UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, // D
            UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, UNI, // E
            UNI, UNI, UNI, UNI, UNI, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, UER, // F
        ]
    };
}

pub mod byte_handler_tables {
    use super::*;

    pub static NO_TOKENS: ByteHandlers<NoTokensLexerConfig> = byte_handlers!();
    pub static WITH_TOKENS: ByteHandlers<TokensLexerConfig> = byte_handlers!();
    pub static RUNTIME_TOKENS: ByteHandlers<RuntimeLexerConfig> = byte_handlers!();
}

/// Macro for defining byte handler for an ASCII character.
///
/// Asserts that lexer is not at end of file, and that next char is ASCII.
/// Where the handler is for an ASCII character, these assertions are self-evidently true.
///
/// These assertions produce no runtime code, but hint to the compiler that it can assume that
/// next char is ASCII, and it uses that information to optimize the rest of the handler.
/// e.g. `lexer.consume_char()` becomes just a single assembly instruction.
/// Without the assertions, the compiler is unable to deduce the next char is ASCII, due to
/// the indirection of the byte handlers jump table.
///
/// These assertions are unchecked (i.e. won't panic) and will cause UB if they're incorrect.
///
/// # SAFETY
/// Only use this macro to define byte handlers for ASCII characters.
///
/// ```rust,ignore
/// ascii_byte_handler!(SPS(lexer) {
///     lexer.consume_char();
///     Kind::WhiteSpace
/// });
/// ```
///
/// expands to:
///
/// ```rust,ignore
/// #[expect(non_snake_case)]
/// fn SPS<C: Config>(lexer: &mut Lexer<'_, C>) -> Kind {
///     // SAFETY: This macro is only used for ASCII characters
///     unsafe {
///         assert_unchecked!(!lexer.source.is_eof());
///         assert_unchecked!(lexer.source.peek_byte_unchecked() < 128);
///     }
///     {
///         lexer.consume_char();
///         Kind::WhiteSpace
///     }
/// }
/// ```
macro_rules! ascii_byte_handler {
    ($id:ident($lex:ident) $body:expr) => {
        #[expect(non_snake_case)]
        fn $id<C: Config>($lex: &mut Lexer<'_, C>) -> Kind {
            // SAFETY: This macro is only used for ASCII characters
            unsafe {
                assert_unchecked!(!$lex.source.is_eof());
                assert_unchecked!($lex.source.peek_byte_unchecked() < 128);
            }
            $body
        }
    };
}

// `\0` `\1` etc
ascii_byte_handler!(ERR(lexer) {
    let c = lexer.consume_char();
    lexer.error(diagnostics::invalid_character(c, lexer.unterminated_range()));
    Kind::Undetermined
});

// <SPACE> <TAB> Normal Whitespace
ascii_byte_handler!(SPS(lexer) {
    let bytes = lexer.source.remaining().as_bytes();
    let end = bytes.iter().position(|&b| !matches!(b, b' ' | b'\t')).unwrap_or(bytes.len());
    // SAFETY: Only in-bounds ASCII spaces and tabs were consumed.
    lexer.source.set_position(unsafe { lexer.source.position().add(end) });
    Kind::Skip
});

// <VT> <FF> Irregular Whitespace
ascii_byte_handler!(ISP(lexer) {
    lexer.consume_char();
    lexer.trivia_builder.add_irregular_whitespace(lexer.token.start(), lexer.offset());
    Kind::Skip
});

// '\r' '\n'
ascii_byte_handler!(LIN(lexer) {
    lexer.consume_char();
    lexer.line_break_handler()
});

// #
ascii_byte_handler!(HAS(lexer) {
    lexer.consume_char();
    lexer.private_identifier()
});

// \
ascii_byte_handler!(ESC(lexer) {
    lexer.identifier_backslash_handler()
});

// `
ascii_byte_handler!(TPL(lexer) {
    lexer.consume_char();
    lexer.read_template_literal(Kind::TemplateHead, Kind::NoSubstitutionTemplate)
});

// Non-ASCII characters.
//
// Note: Must not use `ascii_byte_handler!` macro, as this handler is for non-ASCII chars.
#[expect(non_snake_case)]
fn UNI<C: Config>(lexer: &mut Lexer<'_, C>) -> Kind {
    lexer.unicode_char_handler()
}

// Token classes dispatched directly in `handle_byte`, and invalid UTF-8 start bytes.
//
// UTF-8 continuation bytes (0x80 - 0xBF) (i.e. middle of a multi-byte UTF-8 sequence)
// + and byte values which are not legal in UTF-8 strings (0xC0, 0xC1, 0xF5 - 0xFF).
// `handle_byte()` should only be called with 1st byte of a valid UTF-8 character,
// so something has gone wrong if we get here.
// https://datatracker.ietf.org/doc/html/rfc3629
//
// Note: Must not use `ascii_byte_handler!` macro, as this handler also covers non-ASCII bytes.
#[expect(non_snake_case)]
fn UER<C: Config>(_lexer: &mut Lexer<'_, C>) -> Kind {
    unreachable!();
}
