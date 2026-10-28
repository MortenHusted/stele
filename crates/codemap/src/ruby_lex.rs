//! Prism's lexer over FFI, for the Ruby clone token stream.
//!
//! `ruby-prism` exposes parsing but not lexing, while the `libprism.a` the
//! `ruby-prism-sys` build already links exports `pm_serialize_lex`. This
//! module declares that function and the buffer it writes into, decodes
//! the serialized tokens, and normalises them into the shape
//! `measures::token_stream` gives the tree-sitter languages: comments and
//! whitespace dropped, every identifier `$id`, every literal `$lit`,
//! keywords and punctuation as written. No new build step: the symbols
//! come from the archive the sys crate compiled.
//!
//! Serialization, from `src/serialize.c` in the vendored Prism: one record
//! per token of four variable-length unsigned integers (7 bits per byte,
//! high bit continues) — type, start offset, length, lexer state — then a
//! single 0 byte where the next type would be, then metadata this module
//! does not read. Token types are read back by name through
//! `pm_token_type_name`, so nothing here depends on the enum's numbering.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

use crate::clones::Token;

/// `pm_buffer_t` from `prism/util/pm_buffer.h`: length, capacity, value.
#[repr(C)]
struct PmBuffer {
    length: usize,
    capacity: usize,
    value: *mut c_char,
}

unsafe extern "C" {
    fn pm_buffer_init(buffer: *mut PmBuffer) -> bool;
    fn pm_buffer_value(buffer: *const PmBuffer) -> *mut c_char;
    fn pm_buffer_length(buffer: *const PmBuffer) -> usize;
    fn pm_buffer_free(buffer: *mut PmBuffer);
    fn pm_serialize_lex(buffer: *mut PmBuffer, source: *const u8, size: usize, data: *const c_char);
    fn pm_token_type_name(token_type: c_int) -> *const c_char;
}

/// One token as Prism lexed it: its type name and byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexedToken {
    pub kind: &'static str,
    pub start: usize,
    pub end: usize,
}

/// Lex a Ruby source with Prism. Tokens come back in source order; a
/// source Prism cannot lex at all yields no tokens rather than a guess.
pub fn lex(source: &[u8]) -> Vec<LexedToken> {
    let mut buffer = PmBuffer {
        length: 0,
        capacity: 0,
        value: std::ptr::null_mut(),
    };
    // SAFETY: `buffer` is a properly laid out `pm_buffer_t`; Prism
    // initialises it, writes into memory it allocates, and frees it below.
    // `source` outlives the call and is never written. A null options
    // pointer means Prism's defaults (`pm_options_read` returns on NULL).
    let bytes: Vec<u8> = unsafe {
        if !pm_buffer_init(&raw mut buffer) {
            return Vec::new();
        }
        pm_serialize_lex(
            &raw mut buffer,
            source.as_ptr(),
            source.len(),
            std::ptr::null(),
        );
        let value = pm_buffer_value(&raw const buffer).cast::<u8>();
        let length = pm_buffer_length(&raw const buffer);
        let copied = if value.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(value, length).to_vec()
        };
        pm_buffer_free(&raw mut buffer);
        copied
    };
    decode(&bytes, source.len())
}

/// Decode the token records up to the 0 terminator.
fn decode(bytes: &[u8], source_len: usize) -> Vec<LexedToken> {
    let mut tokens = Vec::new();
    let mut position = 0usize;
    while let Some(kind) = varuint(bytes, &mut position) {
        if kind == 0 {
            break;
        }
        let (Some(start), Some(length), Some(_state)) = (
            varuint(bytes, &mut position),
            varuint(bytes, &mut position),
            varuint(bytes, &mut position),
        ) else {
            break;
        };
        let start = start as usize;
        let end = start.saturating_add(length as usize).min(source_len);
        if start > end {
            continue;
        }
        tokens.push(LexedToken {
            kind: token_type_name(kind),
            start,
            end,
        });
    }
    tokens
}

fn varuint(bytes: &[u8], position: &mut usize) -> Option<u32> {
    let mut value: u32 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *bytes.get(*position)?;
        *position += 1;
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift > 28 {
            return None;
        }
    }
}

fn token_type_name(kind: u32) -> &'static str {
    let Ok(kind) = c_int::try_from(kind) else {
        return "UNKNOWN";
    };
    // SAFETY: Prism returns a pointer to a static NUL-terminated string
    // for every value, "UNKNOWN" included; it is never freed.
    unsafe {
        let name = pm_token_type_name(kind);
        if name.is_null() {
            "UNKNOWN"
        } else {
            CStr::from_ptr(name).to_str().unwrap_or("UNKNOWN")
        }
    }
}

/// What a lexed token becomes in the normalised stream.
enum Shape {
    /// Dropped: whitespace, comments, the end marker.
    Skip,
    /// Every name: `$id`.
    Identifier,
    /// Every literal, and the opening delimiter of a string, symbol,
    /// regexp, heredoc, or percent literal, which stands for the whole
    /// literal: `$lit`. The literal's content and closing delimiter are
    /// dropped, so `"a"` and `'b'` are one token each, as a tree-sitter
    /// string literal is; an interpolation's `#{` and `}` and the code
    /// inside stay.
    Literal,
    /// The content and closing delimiter of a literal the opener stood for.
    LiteralTail,
    /// Everything else as written: keywords, operators, punctuation.
    Verbatim,
}

fn shape(kind: &str) -> Shape {
    match kind {
        "EOF" | "NEWLINE" | "IGNORED_NEWLINE" | "WORDS_SEP" | "COMMENT" | "EMBDOC_BEGIN"
        | "EMBDOC_LINE" | "EMBDOC_END" | "__END__" | "MISSING" | "NOT_PROVIDED" | "UNKNOWN" => {
            Shape::Skip
        }
        "IDENTIFIER" | "CONSTANT" | "INSTANCE_VARIABLE" | "CLASS_VARIABLE" | "GLOBAL_VARIABLE"
        | "LABEL" | "METHOD_NAME" | "BACK_REFERENCE" | "NUMBERED_REFERENCE" => Shape::Identifier,
        "INTEGER"
        | "INTEGER_IMAGINARY"
        | "INTEGER_RATIONAL"
        | "INTEGER_RATIONAL_IMAGINARY"
        | "FLOAT"
        | "FLOAT_IMAGINARY"
        | "FLOAT_RATIONAL"
        | "FLOAT_RATIONAL_IMAGINARY"
        | "CHARACTER_LITERAL"
        | "STRING_BEGIN"
        | "SYMBOL_BEGIN"
        | "REGEXP_BEGIN"
        | "HEREDOC_START"
        | "PERCENT_LOWER_I"
        | "PERCENT_LOWER_W"
        | "PERCENT_LOWER_X"
        | "PERCENT_UPPER_I"
        | "PERCENT_UPPER_W"
        | "BACKTICK" => Shape::Literal,
        "STRING_CONTENT" | "STRING_END" | "REGEXP_END" | "HEREDOC_END" | "LABEL_END" => {
            Shape::LiteralTail
        }
        _ => Shape::Verbatim,
    }
}

/// The normalised token stream of a Ruby source, in the shape
/// `measures::token_stream` gives the tree-sitter languages.
pub fn token_stream(source: &[u8]) -> Vec<Token> {
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(
            source
                .iter()
                .enumerate()
                .filter(|(_, byte)| **byte == b'\n')
                .map(|(index, _)| index + 1),
        )
        .collect();
    let line_of = |offset: usize| -> u32 {
        let line = line_starts.partition_point(|start| *start <= offset);
        u32::try_from(line).unwrap_or(u32::MAX)
    };
    lex(source)
        .into_iter()
        .filter_map(|token| {
            let text = match shape(token.kind) {
                Shape::Skip | Shape::LiteralTail => return None,
                Shape::Identifier => "$id".to_string(),
                Shape::Literal => "$lit".to_string(),
                Shape::Verbatim => {
                    let text = String::from_utf8_lossy(&source[token.start..token.end])
                        .trim()
                        .to_string();
                    if text.is_empty() {
                        return None;
                    }
                    text
                }
            };
            let start_line = line_of(token.start);
            // A token's last byte, so one that ends on a newline stays on
            // the line it started.
            let end_line = line_of(token.end.saturating_sub(1).max(token.start));
            Some(Token {
                text,
                start_line,
                end_line,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(source: &str) -> Vec<String> {
        token_stream(source.as_bytes())
            .into_iter()
            .map(|token| token.text)
            .collect()
    }

    #[test]
    fn identifiers_and_literals_are_placeholders_and_keywords_stay() {
        assert_eq!(
            texts("def total(values)\n  values.sum + 1\nend\n"),
            [
                "def", "$id", "(", "$id", ")", "$id", ".", "$id", "+", "$lit", "end"
            ]
        );
    }

    #[test]
    fn a_string_is_one_literal_and_interpolated_code_stays() {
        assert_eq!(texts("x = \"a\"\n"), ["$id", "=", "$lit"]);
        assert_eq!(texts("x = 'a'\n"), ["$id", "=", "$lit"]);
        assert_eq!(
            texts("x = \"a#{name}b\"\n"),
            ["$id", "=", "$lit", "#{", "$id", "}"]
        );
        assert_eq!(texts("y = :tag\n"), ["$id", "=", "$lit", "$id"]);
        assert_eq!(
            texts("z = [1, 2.5]\n"),
            ["$id", "=", "[", "$lit", ",", "$lit", "]"]
        );
    }

    #[test]
    fn comments_and_whitespace_leave_no_token_and_lines_are_kept() {
        let source = "# leading\nx = 1 # trailing\n\n=begin\ndoc\n=end\ny = 2\n";
        let tokens = token_stream(source.as_bytes());
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, ["$id", "=", "$lit", "$id", "=", "$lit"]);
        assert_eq!(tokens[0].start_line, 2);
        assert_eq!(tokens[3].start_line, 7);
        assert_eq!(tokens[5].end_line, 7);
    }

    #[test]
    fn renamed_twins_share_one_stream() {
        let a = texts(
            "def tally(items, limit)\n  kept = 0\n  kept += 1 if items.size > limit\n  kept\nend\n",
        );
        let b = texts(
            "def count_up(numbers, ceiling)\n  sum = 7\n  sum += 2 if numbers.size > ceiling\n  sum\nend\n",
        );
        assert_eq!(a, b);
        assert!(a.len() > 10);
    }

    #[test]
    fn an_empty_source_lexes_to_nothing() {
        assert_eq!(token_stream(b"").len(), 0);
        assert_eq!(token_stream(b"\n\n# only a comment\n").len(), 0);
    }
}
