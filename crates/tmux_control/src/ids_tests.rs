use super::*;

#[test]
fn ids_parse_and_print_with_their_prefix() {
    assert_eq!("%12".parse::<PaneId>().unwrap(), PaneId(12));
    assert_eq!("@3".parse::<WindowId>().unwrap(), WindowId(3));
    assert_eq!("$0".parse::<SessionId>().unwrap(), SessionId(0));
    assert_eq!(PaneId(7).to_string(), "%7");
    assert_eq!(WindowId(7).to_string(), "@7");
    assert_eq!(SessionId(7).to_string(), "$7");
    assert!("12".parse::<PaneId>().is_err());
    assert!("@3".parse::<PaneId>().is_err());
    assert!("%x".parse::<PaneId>().is_err());
}
