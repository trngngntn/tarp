use warpui::fonts::{FamilyId, Weight};
use warpui::geometry::rect::RectF;
use warpui::geometry::vector::vec2f;
use warpui::units::IntoPixels;

use super::*;
use crate::terminal::blockgrid_renderer::painted_rows;
use crate::terminal::color;
use crate::terminal::model::ObfuscateSecrets;
use crate::themes::default_themes::dark_theme;

const CELL_HEIGHT: f32 = 20.;
const VIEWPORT_ROWS: usize = 10;

fn params() -> BlockGridParams {
    let size_info = SizeInfo::new(
        vec2f(800., VIEWPORT_ROWS as f32 * CELL_HEIGHT),
        8.0.into_pixels(),
        CELL_HEIGHT.into_pixels(),
        0.0.into_pixels(),
        0.0.into_pixels(),
    );
    let theme = dark_theme();
    BlockGridParams {
        grid_render_params: GridRenderParams {
            warp_theme: theme.clone(),
            font_family: FamilyId(0),
            font_size: 13.,
            font_weight: Weight::Normal,
            line_height_ratio: 1.,
            enforce_minimum_contrast: EnforceMinimumContrast::Never,
            obfuscate_secrets: ObfuscateSecrets::No,
            size_info,
            cell_size: vec2f(8., CELL_HEIGHT),
            use_ligature_rendering: false,
            hide_cursor_cell: false,
        },
        colors: color::List::from(&color::Colors::from(theme)),
        override_colors: color::OverrideList::empty(),
        bounds: RectF::new(vec2f(0., 0.), size_info.pane_size_px()),
    }
}

fn header_over(
    model: &TerminalModel,
    block: &Block,
    grid_origin: Vector2F,
    params: &BlockGridParams,
) -> Option<SnackbarHeader> {
    SnackbarHeader::default().update(
        !model.is_raw(),
        true,
        block,
        grid_origin,
        params.bounds,
        params,
        ScrollPosition::FollowsBottomOfMostRecentBlock,
    )
}

fn tall_output() -> String {
    (0..30).map(|row| format!("r{row}")).join("\r\n")
}

#[test]
fn snackbar_is_not_pinned_over_a_restore_taller_than_the_viewport() {
    let mut model = TerminalModel::new_raw_for_test();
    model.process_bytes(tall_output().as_str());
    let block = model.block_list().active_block();
    assert!(block.should_hide_command_grid());

    let params = params();
    let element_origin = params.bounds.origin();
    let grid_len = block.output_grid().len_displayed();
    // Scrolled to the bottom of the block.
    let grid_origin = element_origin - vec2f(0., (grid_len - VIEWPORT_ROWS) as f32 * CELL_HEIGHT);

    assert_eq!(header_over(&model, block, grid_origin, &params), None);
    let painted = painted_rows(grid_origin, element_origin, &params, grid_len);
    for row in grid_len - VIEWPORT_ROWS..grid_len {
        assert!(
            painted.contains(&row),
            "row {row} is on screen but not painted"
        );
    }
}

#[test]
fn snackbar_is_pinned_over_a_scrolled_command_block() {
    let mut model = TerminalModel::mock(None, None);
    model.simulate_block("ls", tall_output().as_str());
    let block = model
        .block_list()
        .blocks()
        .iter()
        .find(|block| block.command_to_string() == "ls")
        .unwrap();
    let params = params();
    let grid_origin = params.bounds.origin() - vec2f(0., 5. * CELL_HEIGHT);
    assert!(header_over(&model, block, grid_origin, &params).is_some());
}

#[test]
fn snackbar_is_pinned_over_a_scrolled_block_with_a_hidden_command() {
    let mut model = TerminalModel::mock(None, None);
    model.simulate_block("ls", tall_output().as_str());
    model
        .block_list_mut()
        .blocks_mut()
        .iter_mut()
        .find(|block| block.command_to_string() == "ls")
        .unwrap()
        .set_should_hide_command_grid(true);
    let block = model
        .block_list()
        .blocks()
        .iter()
        .find(|block| block.command_to_string() == "ls")
        .unwrap();
    let params = params();
    let grid_origin = params.bounds.origin() - vec2f(0., 5. * CELL_HEIGHT);
    assert!(header_over(&model, block, grid_origin, &params).is_some());
}
