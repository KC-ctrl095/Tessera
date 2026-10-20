use tinycsv::{parse, ParseError};

fn rec(fields: &[&str]) -> Vec<String> {
    fields.iter().map(|s| s.to_string()).collect()
}

fn ok(input: &str) -> Vec<Vec<String>> {
    parse(input).unwrap_or_else(|e| panic!("{input:?} failed: {e}"))
}

#[test]
fn line_endings() {
    assert_eq!(ok("a,b\r\nc,d\r\n"), vec![rec(&["a", "b"]), rec(&["c", "d"])]);
    assert_eq!(ok("a,b\r\nc,d"), vec![rec(&["a", "b"]), rec(&["c", "d"])]);
    assert_eq!(ok("a\rb,c\n"), vec![rec(&["a\rb", "c"])], "a lone \\r is a character");
    assert_eq!(ok("\"x\r\ny\",z\r\n"), vec![rec(&["x\r\ny", "z"])], "line breaks in quotes are kept as written");
}

#[test]
fn empty_inputs_and_lines() {
    assert!(ok("").is_empty());
    assert!(ok("\n").is_empty());
    assert!(ok("\n\r\n\n").is_empty());
    assert_eq!(ok("a\n\n\nb\n"), vec![rec(&["a"]), rec(&["b"])]);
    assert_eq!(ok("\"\"\n"), vec![rec(&[""])], "a quoted empty field is a record");
    assert_eq!(ok(","), vec![rec(&["", ""])]);
    assert_eq!(ok("a,\n,b"), vec![rec(&["a", ""]), rec(&["", "b"])]);
}

#[test]
fn spaces_are_kept() {
    assert_eq!(ok(" a , b \n"), vec![rec(&[" a ", " b "])]);
    assert_eq!(ok("\" q \",  \n"), vec![rec(&[" q ", "  "])]);
}

#[test]
fn quotes() {
    assert_eq!(ok("\"\"\"\""), vec![rec(&["\""])]);
    assert_eq!(ok("\"a,\"\"b\"\",\nc\",d"), vec![rec(&["a,\"b\",\nc", "d"])]);
    assert_eq!(ok("\"\",\"\",x"), vec![rec(&["", "", "x"])]);
    assert_eq!(ok("x,\"multi\n\nline\""), vec![rec(&["x", "multi\n\nline"])], "an empty line inside quotes is kept");
}

#[test]
fn bad_quotes() {
    assert_eq!(parse("ab\"c,d"), Err(ParseError::BadQuote { line: 1, column: 3 }));
    assert_eq!(parse("a,b\nx,\"ok\"y\n"), Err(ParseError::BadQuote { line: 2, column: 7 }));
    assert_eq!(parse("\"ok\" ,b"), Err(ParseError::BadQuote { line: 1, column: 5 }));
    assert_eq!(parse("a\n\n\nb\"\n"), Err(ParseError::BadQuote { line: 4, column: 2 }), "skipped lines still count");
    assert_eq!(parse("1,\"two\nlines\"!\n"), Err(ParseError::BadQuote { line: 2, column: 7 }), "columns restart on the field's second line");
    assert_eq!(parse("é,ü\"\n"), Err(ParseError::BadQuote { line: 1, column: 4 }), "columns count characters");
    assert_eq!(parse("\"a\"\"\"b"), Err(ParseError::BadQuote { line: 1, column: 6 }));
}

#[test]
fn unterminated_quotes() {
    assert_eq!(parse("\""), Err(ParseError::UnterminatedQuote { line: 1 }));
    assert_eq!(parse("a\nb\n\"c\nd\ne"), Err(ParseError::UnterminatedQuote { line: 3 }));
    assert_eq!(parse("x,\"\"\"\n"), Err(ParseError::UnterminatedQuote { line: 1 }));
    assert_eq!(parse("a\r\n\r\nb,\"c"), Err(ParseError::UnterminatedQuote { line: 3 }));
}

#[test]
fn errors_display() {
    let e = parse("ab\"c").unwrap_err();
    assert!(e.to_string().contains("line 1"));
    let _: &dyn std::error::Error = &e;
}

#[test]
fn a_bigger_file() {
    let mut text = String::from("id,name,notes\r\n");
    for i in 0..500 {
        text.push_str(&format!("{i},\"Name {i}, Jr.\",\"line one\nline \"\"two\"\"\"\r\n"));
    }
    let got = ok(&text);
    assert_eq!(got.len(), 501);
    assert_eq!(got[500], rec(&["499", "Name 499, Jr.", "line one\nline \"two\""]));
}
