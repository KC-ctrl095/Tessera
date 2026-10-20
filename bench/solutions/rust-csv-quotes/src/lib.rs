//! The CSV reader of our import tool.
//!
//! The format (close to RFC 4180):
//!
//! - Records are separated by line breaks, `\n` or `\r\n`. A line break at
//!   the very end of the input does not start another record. A lone `\r`
//!   is an ordinary character.
//! - Lines with nothing on them are skipped (they still count for line
//!   numbers in errors).
//! - Fields are separated by commas. Nothing is trimmed: spaces are part of
//!   the field. `a,` is two fields, the second one empty.
//! - A field may be quoted with `"`. Inside a quoted field, commas and line
//!   breaks are part of the value (line breaks are kept exactly as written),
//!   and `""` stands for one `"`. `""` alone is an empty field.
//! - A quote may only open a field (as its first character) or appear inside
//!   a quoted field. A quote anywhere else in an unquoted field, or anything
//!   other than a comma or a line break right after a closing quote, is
//!   `ParseError::BadQuote` at that character.
//! - Input that ends inside a quoted field is
//!   `ParseError::UnterminatedQuote`, with the line of its opening quote.
//!
//! Lines and columns in errors start at 1. Columns count characters (not
//! bytes) from the start of the physical line, so a field that spans lines
//! restarts the count.

use std::fmt;

/// Why the input isn't valid CSV.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A quote where none may be, or text right after a closing quote.
    BadQuote { line: usize, column: usize },
    /// The input ended inside a quoted field that opened on `line`.
    UnterminatedQuote { line: usize },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::BadQuote { line, column } => {
                write!(f, "line {line}, column {column}: unexpected quote or text after a quoted field")
            }
            ParseError::UnterminatedQuote { line } => {
                write!(f, "line {line}: a quoted field is never closed")
            }
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// At the start of a field.
    Start,
    /// Inside an unquoted field.
    Unquoted,
    /// Inside a quoted field.
    Quoted,
    /// Right after the closing quote of a quoted field.
    Closed,
}

/// Parses CSV text into records of fields.
pub fn parse(input: &str) -> Result<Vec<Vec<String>>, ParseError> {
    let chars: Vec<char> = input.chars().collect();
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut state = State::Start;
    // Whether anything (even a lone comma or quote) is on the current record.
    let mut started = false;
    let (mut line, mut column) = (1usize, 0usize);
    let mut quote_line = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let crlf = c == '\r' && chars.get(i + 1) == Some(&'\n');
        let newline = c == '\n' || crlf;
        column += 1;
        if state == State::Quoted {
            if c == '"' {
                if chars.get(i + 1) == Some(&'"') {
                    field.push('"');
                    i += 2;
                    column += 1;
                    continue;
                }
                state = State::Closed;
            } else if newline {
                field.push_str(if crlf { "\r\n" } else { "\n" });
                line += 1;
                column = 0;
                i += if crlf { 2 } else { 1 };
                continue;
            } else {
                field.push(c);
            }
            i += 1;
            continue;
        }
        if newline {
            if started {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            started = false;
            state = State::Start;
            line += 1;
            column = 0;
            i += if crlf { 2 } else { 1 };
            continue;
        }
        started = true;
        match c {
            ',' => {
                record.push(std::mem::take(&mut field));
                state = State::Start;
            }
            '"' if state == State::Start => {
                state = State::Quoted;
                quote_line = line;
            }
            _ if state == State::Closed || c == '"' => {
                return Err(ParseError::BadQuote { line, column });
            }
            _ => {
                field.push(c);
                state = State::Unquoted;
            }
        }
        i += 1;
    }
    if state == State::Quoted {
        return Err(ParseError::UnterminatedQuote { line: quote_line });
    }
    if started {
        record.push(field);
        records.push(record);
    }
    Ok(records)
}
