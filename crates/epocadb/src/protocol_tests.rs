//! Parsing of the wire protocol's text lines, in isolation from any socket.

use super::*;

// ── responses ─────────────────────────────────────────────────────

#[test]
fn parse_ok_without_detail() {
    assert_eq!(parse_response_line(b"OK"), Response::Ok(None));
}

#[test]
fn parse_ok_with_detail() {
    assert_eq!(
        parse_response_line(b"OK wrote 281 bytes"),
        Response::Ok(Some("wrote 281 bytes".into()))
    );
}

#[test]
fn parse_err() {
    assert_eq!(parse_response_line(b"ERR file not found"), Response::Err("file not found".into()));
}

#[test]
fn parse_data() {
    assert_eq!(parse_response_line(b"DATA 1024"), Response::Data(1024));
    assert_eq!(parse_response_line(b"DATA 0"), Response::Data(0));
}

#[test]
fn parse_data_invalid_is_zero() {
    assert_eq!(parse_response_line(b"DATA abc"), Response::Data(0));
}

#[test]
fn parse_data_tolerates_trailing_space() {
    // A host that pads the line should not silently become a zero-byte transfer.
    assert_eq!(parse_response_line(b"DATA 512 "), Response::Data(512));
}

#[test]
fn unknown_response_is_an_error() {
    match parse_response_line(b"GARBAGE") {
        Response::Err(msg) => assert!(msg.contains("GARBAGE")),
        other => panic!("expected Err, got {other:?}"),
    }
}

#[test]
fn invalid_utf8_does_not_panic() {
    // Payload bytes misread as a line must produce an error, not an abort. On the device
    // a panic here is the whole application gone.
    match parse_response_line(&[0xff, 0xfe, 0x00]) {
        Response::Err(_) => {}
        other => panic!("expected Err, got {other:?}"),
    }
}

#[test]
fn parse_ok_with_spaces_in_detail() {
    assert_eq!(
        parse_response_line(b"OK device=Nokia E72  sdk=0.1"),
        Response::Ok(Some("device=Nokia E72  sdk=0.1".into()))
    );
}

// ── commands ──────────────────────────────────────────────────────

#[test]
fn pong_is_command_none() {
    assert_eq!(parse_command("pong"), Some(Command::None));
}

#[test]
fn push_command_parses_path_and_size() {
    assert_eq!(
        parse_command("PUSH /Data/file.bin 1024"),
        Some(Command::Push { path: "/Data/file.bin".into(), size: 1024 })
    );
}

#[test]
fn push_command_with_spaces_in_path() {
    // The size is taken from the end, so only the last space is a separator.
    assert_eq!(
        parse_command("PUSH C:\\private\\E1234569\\my file.dat 512"),
        Some(Command::Push { path: "C:\\private\\E1234569\\my file.dat".into(), size: 512 })
    );
}

#[test]
fn pull_command_parses_path() {
    assert_eq!(
        parse_command("PULL /Data/report.txt"),
        Some(Command::Pull { path: "/Data/report.txt".into() })
    );
}

#[test]
fn pull_without_a_path_is_not_a_command() {
    // Otherwise it becomes a Pull of "", and the device answers ERR to a question the
    // host never asked.
    assert_eq!(parse_command("PULL"), None);
    assert_eq!(parse_command("PULL "), None);
}

#[test]
fn install_command_parses_path_and_size() {
    assert_eq!(
        parse_command("INSTALL C:\\Data\\app.sis 93632"),
        Some(Command::Install { path: "C:\\Data\\app.sis".into(), size: 93632 })
    );
}

#[test]
fn quit_is_a_command() {
    assert_eq!(parse_command("QUIT"), Some(Command::Quit));
}

#[test]
fn unknown_verb_is_not_a_command() {
    assert_eq!(parse_command("GARBAGE"), None);
}

#[test]
fn push_missing_size_is_not_a_command() {
    assert_eq!(parse_command("PUSH /path"), None);
}

#[test]
fn push_with_a_non_numeric_size_is_not_a_command() {
    // Reading it as zero would leave the payload in the stream to be parsed as commands.
    assert_eq!(parse_command("PUSH /path abc"), None);
}

#[test]
fn control_is_forwarded_verbatim() {
    // The bridge does not interpret control lines; whatever follows CTL is handed up
    // for the application on top to parse.
    assert_eq!(
        parse_command("CTL monitor enable"),
        Some(Command::Control("monitor enable".into()))
    );
    // Even the argument-less form is a control command, not a parse failure — an empty
    // control line must not fall through to "unparsed reply".
    assert_eq!(parse_command("CTL"), Some(Command::Control(String::new())));
}

#[test]
fn split_last_splits_on_final_space() {
    assert_eq!(split_last("a b c"), Some(("a b", "c")));
    assert_eq!(split_last("a"), None);
    assert_eq!(split_last("ab cd ef"), Some(("ab cd", "ef")));
}

// ── requests ──────────────────────────────────────────────────────

#[test]
fn build_request_formats_correctly() {
    assert_eq!(build_request("PING", ""), "REQ PING\r\n");
    assert_eq!(build_request("PUSH", "file.txt 1024"), "REQ PUSH file.txt 1024\r\n");
}
