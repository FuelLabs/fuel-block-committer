static CONTROL_PANEL_TEMPLATE: &str = include_str!("../templates/control_panel.html");

/// Renders the control panel template by replacing the placeholders.
pub fn render_control_panel(current_block_size: usize, current_compress: &str) -> String {
    let sel_random = if current_compress == "random" {
        "selected"
    } else {
        ""
    };
    let sel_low = if current_compress == "low" {
        "selected"
    } else {
        ""
    };
    let sel_medium = if current_compress == "medium" {
        "selected"
    } else {
        ""
    };
    let sel_high = if current_compress == "high" {
        "selected"
    } else {
        ""
    };
    let sel_full = if current_compress == "full" {
        "selected"
    } else {
        ""
    };

    CONTROL_PANEL_TEMPLATE
        .replace("{{current_block_size}}", &current_block_size.to_string())
        .replace("{{sel_random}}", sel_random)
        .replace("{{sel_low}}", sel_low)
        .replace("{{sel_medium}}", sel_medium)
        .replace("{{sel_high}}", sel_high)
        .replace("{{sel_full}}", sel_full)
}
