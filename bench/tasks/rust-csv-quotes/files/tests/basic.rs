use tinycsv::{parse, ParseError};

fn rec(fields: &[&str]) -> Vec<String> {
    fields.iter().map(|s| s.to_string()).collect()
}

#[test]
fn plain_records() {
    let got = parse("name,city\nAna,Porto\nBo,Braga\n").unwrap();
    assert_eq!(got, vec![rec(&["name", "city"]), rec(&["Ana", "Porto"]), rec(&["Bo", "Braga"])]);
}

#[test]
fn quoted_comma() {
    let got = parse("id,address\n7,\"Rua Nova, 12\"\n").unwrap();
    assert_eq!(got[1], rec(&["7", "Rua Nova, 12"]));
}

#[test]
fn doubled_quotes() {
    let got = parse("\"She said \"\"hi\"\"\",x").unwrap();
    assert_eq!(got, vec![rec(&["She said \"hi\"", "x"])]);
}

#[test]
fn quoted_line_break() {
    let got = parse("note,n\n\"two\nlines\",2\n").unwrap();
    assert_eq!(got, vec![rec(&["note", "n"]), rec(&["two\nlines", "2"])]);
}

#[test]
fn unterminated() {
    assert_eq!(parse("a,b\n\"oops,c\n"), Err(ParseError::UnterminatedQuote { line: 2 }));
}
