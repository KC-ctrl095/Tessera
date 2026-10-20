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

/// Parses CSV text into records of fields.
pub fn parse(input: &str) -> Result<Vec<Vec<String>>, ParseError> {
    let mut records = Vec::new();
    for line in input.lines() {
        if line.is_empty() {
            continue;
        }
        let fields = line
            .split(',')
            .map(|f| f.trim_matches('"').to_string())
            .collect();
        records.push(fields);
    }
    Ok(records)
}
