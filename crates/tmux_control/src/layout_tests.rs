use super::*;

fn size(columns: usize, rows: usize) -> PaneSize {
    PaneSize { columns, rows }
}

#[test]
fn a_single_pane_layout_has_the_window_size() {
    assert_eq!(
        pane_sizes("e9dd,30x4,0,0,0"),
        Some(vec![(PaneId(0), size(30, 4))])
    );
}

#[test]
fn split_layouts_list_every_pane() {
    assert_eq!(
        pane_sizes("285d,40x5,0,0[40x2,0,0,0,40x2,0,3,1]"),
        Some(vec![(PaneId(0), size(40, 2)), (PaneId(1), size(40, 2))])
    );
    assert_eq!(
        pane_sizes("a1b2,80x24,0,0{40x24,0,0,3,39x24,41,0[39x12,41,0,4,39x11,41,13,5]}"),
        Some(vec![
            (PaneId(3), size(40, 24)),
            (PaneId(4), size(39, 12)),
            (PaneId(5), size(39, 11)),
        ])
    );
}

#[test]
fn malformed_layouts_are_rejected() {
    for layout in [
        "",
        "e9dd",
        "e9dd,30x4,0,0",
        "e9dd,30x4,0,0[",
        "e9dd,30,0,0,0",
        "e9dd,30x4,0,0,0x",
    ] {
        assert_eq!(pane_sizes(layout), None, "{layout}");
    }
}
