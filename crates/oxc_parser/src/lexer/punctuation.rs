use oxc_ast::CommentKind;
use oxc_span::Span;

use crate::{config::LexerConfig as Config, diagnostics};

use super::{
    Kind, Lexer, Token,
    byte_handlers::{OPERATOR_PAIRS, OPERATORS_WITH_EQ, PUNCTUATION_KINDS, operator_index},
    cold_branch,
};

impl<C: Config> Lexer<'_, C> {
    /// Read an ASCII operator, including comments and leading decimal points.
    ///
    /// # SAFETY
    /// `byte` must be the current source byte and an ASCII operator start.
    #[expect(clippy::inline_always)]
    #[inline(always)]
    pub(super) unsafe fn read_operator(&mut self, byte: u8) -> Option<Kind> {
        let start = self.source.position();
        let bytes = self.source.remaining().as_bytes();
        if byte == b'.' {
            // SAFETY: The caller guarantees this ASCII byte exists.
            self.source.set_position(unsafe { start.add(1) });
            return Some(self.read_dot());
        }
        if byte == b'/' && matches!(bytes.get(1), Some(b'/' | b'*')) {
            let line = bytes[1] == b'/';
            // SAFETY: Both ASCII comment delimiters were checked above.
            self.source.set_position(unsafe { start.add(2) });
            return cold_branch(|| {
                let kind = if line {
                    self.skip_single_line_comment(CommentKind::Line)
                } else {
                    self.skip_multi_line_comment()
                };
                if kind == Kind::Skip { None } else { Some(kind) }
            });
        }
        if byte == b'<' && bytes.starts_with(b"<!--") {
            // SAFETY: The opening `<` is present and ASCII.
            self.source.set_position(unsafe { start.add(1) });
            return self.read_html_open_comment();
        }
        let mut end = 1;
        let mut kind = PUNCTUATION_KINDS[byte as usize];
        if let Some(&next) = bytes.get(1)
            && b"=<>+-*&|?./!".contains(&next)
        {
            let pair = OPERATOR_PAIRS[operator_index(byte, next)];
            if pair != Kind::Eof
                && !(pair == Kind::QuestionDot && bytes.get(2).is_some_and(u8::is_ascii_digit))
            {
                kind = pair;
                end = 2;
                if kind == Kind::Minus2
                    && bytes.get(2) == Some(&b'>')
                    && self.token.is_on_new_line()
                    && !self.source_type.is_strict()
                {
                    // SAFETY: The three ASCII bytes of `-->` were checked above.
                    self.source.set_position(unsafe { start.add(3) });
                    return cold_branch(|| {
                        self.defer_html_comment_error(3);
                        self.skip_single_line_comment(CommentKind::HtmlClose);
                        None
                    });
                }
                let with_eq = OPERATORS_WITH_EQ[kind as usize];
                if with_eq != Kind::Eof && bytes.get(end) == Some(&b'=') {
                    end += 1;
                    kind = with_eq;
                }
            }
        }
        // SAFETY: The recognized operator consists of `end` checked ASCII bytes.
        self.source.set_position(unsafe { start.add(end) });
        Some(kind)
    }

    /// Section 12.8 Punctuators
    #[expect(clippy::inline_always)]
    #[inline(always)]
    pub(super) fn read_dot(&mut self) -> Kind {
        if self.peek_2_bytes() == Some([b'.', b'.']) {
            // SAFETY: The next two bytes were checked to be ASCII dots.
            self.source.set_position(unsafe { self.source.position().add(2) });
            return Kind::Dot3;
        }
        if self.peek_byte().is_some_and(|b| b.is_ascii_digit()) {
            self.decimal_literal_after_decimal_point()
        } else {
            Kind::Dot
        }
    }

    /// Handle `<!--` after consuming its opening `<`.
    #[cold]
    fn read_html_open_comment(&mut self) -> Option<Kind> {
        if self.source_type.is_module() {
            if !self.token.is_on_new_line() {
                return Some(Kind::LAngle);
            }
            let span = Span::sized(self.token.start(), 4);
            self.errors.push(diagnostics::html_comment_in_module(span));
        } else {
            self.defer_html_comment_error(4);
        }
        self.skip_single_line_comment(CommentKind::HtmlOpen);
        None
    }

    /// Defer HTML comment error for unambiguous mode (emitted if file resolves to module)
    fn defer_html_comment_error(&mut self, len: u32) {
        if self.source_type.is_unambiguous() {
            let span = Span::sized(self.token.start(), len);
            self.deferred_module_errors.push(diagnostics::html_comment_in_module(span));
        }
    }

    pub(crate) fn re_lex_right_angle(&mut self) -> Token {
        self.token.set_start(self.offset() - 1);
        let kind = self.read_right_angle();
        self.finish_next_retokenized(kind)
    }

    fn read_right_angle(&mut self) -> Kind {
        if self.next_ascii_byte_eq(b'>') {
            if self.next_ascii_byte_eq(b'>') {
                if self.next_ascii_byte_eq(b'=') { Kind::ShiftRight3Eq } else { Kind::ShiftRight3 }
            } else if self.next_ascii_byte_eq(b'=') {
                Kind::ShiftRightEq
            } else {
                Kind::ShiftRight
            }
        } else if self.next_ascii_byte_eq(b'=') {
            Kind::GtEq
        } else {
            Kind::RAngle
        }
    }
}
