use crate::config::LexerConfig as Config;

use super::{Kind, Lexer};

impl<C: Config> Lexer<'_, C> {
    #[inline]
    pub(super) fn line_break_handler(&mut self) -> Kind {
        self.token.set_is_on_new_line(true);
        self.trivia_builder.handle_newline();

        // Indentation is common after a line break.
        // Consume it, along with any further line breaks.
        // Irregular line breaks and whitespace are not consumed.
        // They're uncommon, so leave them for the next call to `handle_byte` to take care of.
        let bytes = self.source.remaining().as_bytes();
        let mut end = 0;
        while let Some(chunk) = bytes.get(end..end + 8) {
            let different = u64::from_le_bytes(chunk.try_into().unwrap()) ^ 0x2020_2020_2020_2020;
            end += (different.trailing_zeros() / 8) as usize;
            if different != 0 {
                break;
            }
        }
        while matches!(bytes.get(end), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            end += 1;
        }
        // SAFETY: Only in-bounds ASCII whitespace has been consumed.
        self.source.set_position(unsafe { self.source.position().add(end) });

        Kind::Skip
    }
}
