use super::*;

#[test]
fn octal_escapes_decode_and_utf8_passes_through() {
    assert_eq!(
        unescape_output(b"h\xc3\xa9\\134\\015\\012"),
        b"h\xc3\xa9\\\r\n"
    );
    assert_eq!(unescape_output(br"\033[1mB"), b"\x1b[1mB");
    assert_eq!(unescape_output(br"a\9"), br"a\9"); // not an escape: kept as is
}

const WRAPPED: &[u8] = b"\x1bPtmux;\x1b\x1b]777;notify;warp://cli-agent;{\"x\":1}\x07\x1b\\";
const UNWRAPPED: &[u8] = b"\x1b]777;notify;warp://cli-agent;{\"x\":1}\x07";

#[test]
fn tmux_passthrough_is_unwrapped() {
    let mut out = Vec::new();
    PassthroughUnwrapper::new().push(WRAPPED, &mut out);
    assert_eq!(out, UNWRAPPED);
}

#[test]
fn unwrapping_is_independent_of_chunk_boundaries() {
    for split in 0..=WRAPPED.len() {
        let (mut unwrapper, mut out) = (PassthroughUnwrapper::new(), Vec::new());
        unwrapper.push(&WRAPPED[..split], &mut out);
        unwrapper.push(&WRAPPED[split..], &mut out);
        assert_eq!(out, UNWRAPPED, "split at {split}");
    }
}

#[test]
fn other_escapes_and_dcs_pass_through() {
    let input = b"a\x1b[1mb\x1bP1$r0m\x1b\\c";
    let mut out = Vec::new();
    PassthroughUnwrapper::new().push(input, &mut out);
    assert_eq!(out, input);
}

#[test]
fn a_partial_prefix_is_held_until_decided() {
    let (mut unwrapper, mut out) = (PassthroughUnwrapper::new(), Vec::new());
    unwrapper.push(b"ab\x1bPtm", &mut out);
    assert_eq!(out, b"ab");
    unwrapper.push(b"ux;\x1b\x1bX\x1b\\", &mut out);
    assert_eq!(out, b"ab\x1bX");
}

#[test]
fn doubled_inner_terminator_is_undoubled() {
    let mut out = Vec::new();
    PassthroughUnwrapper::new().push(b"\x1bPtmux;\x1b\x1b]0;t\x1b\x1b\\\x1b\\", &mut out);
    assert_eq!(out, b"\x1b]0;t\x1b\\");
}
