use zgui::{compose::prelude::*, scene::Rect, widgets::Ui};

#[test]
fn ime_input_views_report_their_caret_while_focused_and_unregister_on_unmount() {
    let mut ui = Ui::new(400., 300.);
    let input = div()
        .id("input")
        .size(100., 40.)
        .focusable(true)
        .ime_input(|| Rect::new(8., 17., 8., 17.));
    let mounted = ui.mount(column().p(20.).child(div().h(10.)).child(input));
    ui.prepare_frame();
    let node = mounted.find("input").unwrap();
    assert_eq!(ui.focused_ime_input(), None, "only while focused");

    assert!(ui.input.focus(&ui.scene, Some(node)));
    assert_eq!(
        ui.focused_ime_input(),
        Some((node, Rect::new(28., 47., 8., 17.)))
    );

    mounted.unmount();
    assert_eq!(ui.focused_ime_input(), None);
}
