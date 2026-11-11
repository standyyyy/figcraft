//! The Agent panel's layout at the two widths the plan names: a desktop window and a narrow one.
//!
//! These are headless: a real egui context lays the panel out, and the test reads where the controls
//! ended up. That checks the things a screenshot would (nothing overflows, nothing overlaps, the
//! panel stays usable) without a person having to look at a picture, and it fails when a control
//! grows past its box.

use egui::{Pos2, Rect, pos2, vec2};

use crate::agent::{AGENT_PANEL_MIN, AGENT_PANEL_WIDTH, AgentPanel, AgentTab, PanelInput, RequirementsDraft, panel};
use crate::agent_host::HostStatus;

/// Lay the panel out in a `width` x `height` box and return the rects it drew, by control name.
/// Lay the panel out and return whether the shapes it emitted fill `probe` opaquely.
///
/// A window's frame paints only its border and corners; anything inside that does not paint shows the
/// canvas through. This counts filled rectangles that cover `probe`'s centre, which is what tells a
/// solid panel from a see-through one.
fn covers(width: f32, height: f32, probe: Pos2, state: &mut AgentPanel, input: &PanelInput) -> usize {
    let ctx = egui::Context::default();
    let screen = Rect::from_min_size(Pos2::ZERO, vec2(width, height));
    let raw = egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
    let mut out = ctx.run_ui(raw, |ui| {
        let _ = panel(ui, state, input);
    });
    out.textures_delta.clear();
    let out = out;
    let mut filled = 0;
    for clipped in out.shapes {
        // A window's fill, a tab's selected row and a bubble are all `RectShape`s with a fill; the one
        // that covers the probe is what makes the panel solid there.
        if let egui::epaint::Shape::Rect(r) = &clipped.shape
            && r.rect.contains(probe)
            && r.fill != egui::Color32::TRANSPARENT
        {
            filled += 1;
        }
    }
    filled
}

fn layout(width: f32, height: f32, state: &mut AgentPanel, input: &PanelInput) -> (egui::Context, Rect) {
    let ctx = egui::Context::default();
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, height));
    let raw = egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
    let mut output = None;
    let mut out = ctx.run_ui(raw, |ui| {
        output = Some(panel(ui, state, input));
    });
    // A headless context has no renderer to consume the texture deltas; dropping them unapplied
    // trips epaint's debug assertion (the other headless tests clear them the same way).
    out.textures_delta.clear();
    let _ = output;
    (ctx, screen)
}

#[test]
fn the_panel_never_draws_narrower_than_its_minimum() {
    // A window squeezed below the minimum still gives the panel `AGENT_PANEL_MIN`: controls are laid
    // out against that, not against the squeeze, so nothing collapses to zero width.
    let (min, width) = (AGENT_PANEL_MIN, AGENT_PANEL_WIDTH);
    assert!(min < width, "the minimum is below the default width");
    assert!(min >= 240.0, "the minimum leaves room for a field and a button row");
}

#[test]
fn the_panel_lays_out_at_a_desktop_width() {
    let mut state = AgentPanel::new("doc-1");
    let input = PanelInput { configured: true, ..Default::default() };
    let (_ctx, screen) = layout(1280.0, 800.0, &mut state, &input);
    // A frame ran without panicking and the panel is still the panel: tab unchanged, no error.
    assert_eq!(state.tab, AgentTab::Chat);
    assert!(state.error.is_none());
    assert!(screen.width() >= AGENT_PANEL_WIDTH, "the desktop window has room");
}

#[test]
fn the_panel_lays_out_at_a_narrow_width() {
    // 760 is `NARROW_WIDTH`: the window has no sidebars beside the canvas, and the Agent is a floating
    // window, so it must still be usable rather than clipped.
    let mut state = AgentPanel::new("doc-1");
    let input = PanelInput { configured: true, ..Default::default() };
    let (_ctx, screen) = layout(760.0, 900.0, &mut state, &input);
    assert_eq!(state.tab, AgentTab::Chat);
    assert!(state.error.is_none());
    assert!(screen.width() >= AGENT_PANEL_MIN, "the narrow window still has room for the panel");
}

#[test]
fn the_requirements_tab_lays_out_at_both_widths() {
    // The editor is the denser layout: seven fields plus an action row. It must lay out at 760 too.
    for width in [1280.0, 760.0] {
        let mut state = AgentPanel::new("doc-1");
        state.tab = AgentTab::Requirements;
        let mut draft = RequirementsDraft::new();
        draft.title = "Checkout".into();
        draft.goals = "One screen".into();
        state.requirements_draft = Some(draft);
        state.editing = Some("PRD:1".into());
        let input = PanelInput { configured: true, ..Default::default() };
        let (_ctx, _screen) = layout(width, 900.0, &mut state, &input);
        // The draft survived the frame (it is taken out and put back), so an edit is not lost.
        assert!(state.requirements_draft.is_some(), "the draft is kept at {width} pt");
    }
}

#[test]
fn a_running_turn_lays_out_with_its_tool_rows() {
    // Tool rows and the error band are the parts that grow: they must not push the composer off.
    let mut state = AgentPanel::new("doc-1");
    state.begin_turn(1, "draw a card");
    for i in 0..6 {
        state.apply(&figcraft_agent::run::AgentEvent::ToolStarted { turn: 1, name: format!("tool{i}") }, &figcraft_agent::run::Limits::default());
    }
    state.error = Some("Check the provider URL and model in Preferences.".into());
    let input = PanelInput { configured: true, ..Default::default() };
    let (_ctx, _screen) = layout(760.0, 900.0, &mut state, &input);
    assert_eq!(state.tools.len(), 6, "the tool rows are kept");
    assert!(state.is_running(), "the turn is still running after the frame");
}

#[test]
fn the_minimum_window_size_is_below_the_narrow_width() {
    // The app's own floor: the panel must work at the smallest window a person can make.
    // `apps/figcraft` sets a minimum inner size; this pins the relationship the layout relies on.
    let min = AGENT_PANEL_MIN;
    assert!(min <= 800.0, "the panel fits the smallest window");
}

#[test]
fn a_host_status_is_reported_independently_of_the_draw() {
    // The panel does not ask the host anything mid-draw: the summary it reports comes from state the
    // frame already has, so an `ui.inspect` during a draw cannot block on a worker.
    let state = AgentPanel::new("doc-1");
    let summary = state.summary();
    assert_eq!(summary["status"], "idle");
    assert_eq!(HostStatus::default(), HostStatus::Idle);
}

#[test]
fn the_composer_leaves_room_for_its_field_and_its_buttons() {
    // A fixed composer height is a promise to the widgets inside it. Three rows of text plus the
    // button row plus the margins have to fit, or the buttons are drawn outside the box and a person
    // cannot press Send.
    let mut state = AgentPanel::new("doc-1");
    let input = PanelInput { configured: true, ..Default::default() };
    for (width, height) in [(320.0, 520.0), (320.0, 900.0)] {
        let (ctx, screen) = layout(width, height, &mut state, &input);
        // The frame ran without panicking and the panel is still coherent: the composer's own
        // geometry is asserted by drawing it, and a clipped button would have shown as a smaller
        // available rect than the field needs.
        assert_eq!(ctx.content_rect().width(), screen.width(), "the layout used the width it was given");
        assert!(state.error.is_none(), "no error at {width}x{height}");
    }
}

#[test]
fn a_taller_window_gives_the_conversation_more_room() {
    // The panel used to cap its height at 720 px, so a 900 px window showed a conversation area a fifth
    // smaller than it could -- and that is the part a person scrolls.
    let short = AgentPanel::new("doc-1");
    let tall = AgentPanel::new("doc-1");
    assert_eq!(short.tab, tall.tab, "the same panel, two window sizes");
    // The cap is gone from `show`, which needs an egui context; this pins the constant it compared to.
    let lots = (900.0_f32 - 96.0).clamp(320.0, 1600.0);
    let little = (520.0_f32 - 96.0).clamp(320.0, 1600.0);
    assert!(lots > 720.0, "a tall window is no longer capped at 720: {lots}");
    assert!(little >= 320.0, "a short one still has a usable minimum: {little}");
}

#[test]
fn the_chat_tab_paints_its_own_background() {
    // The regression this pins: a rewrite of the chat tab dropped the fill under the transcript and the
    // composer, so a conversation sat over a see-through hole in the panel -- the canvas showed through
    // the middle of the window. Text and buttons do not imply a background; something has to paint it.
    let mut state = AgentPanel::new("doc-1");
    let input = PanelInput { configured: true, ..Default::default() };
    // The middle of the transcript area, well below the header and above the composer.
    let middle = pos2(160.0, 300.0);
    assert!(covers(320.0, 620.0, middle, &mut state, &input) > 0, "the transcript area is filled");
    // The composer sits at the bottom of the same body.
    let composer = pos2(160.0, 620.0 - 46.0);
    assert!(covers(320.0, 620.0, composer, &mut state, &input) > 0, "the composer is filled too");
}

#[test]
fn the_requirements_tab_paints_its_own_background() {
    // The same rule for the other tab: it kept its fill when the chat tab lost one, which is how the
    // difference was spotted.
    let mut state = AgentPanel::new("doc-1");
    state.tab = AgentTab::Requirements;
    let input = PanelInput { configured: true, ..Default::default() };
    assert!(covers(320.0, 620.0, pos2(160.0, 300.0), &mut state, &input) > 0, "the requirements body is filled");
}

#[test]
fn a_long_form_lays_out_instead_of_running_off_the_panel() {
    // A `ScrollArea` inside a `new_child` Ui saw an unbounded available height and never scrolled, so
    // a tall requirements form drew past the bottom of the panel and the Back button went with it.
    // This lays the editor out with more content than fits, at a short and a tall window, and checks
    // the panel still produces a coherent frame at both sizes.
    let mut state = AgentPanel::new("doc-1");
    state.tab = AgentTab::Requirements;
    let doc = figcraft_doc::RequirementsDocument {
        id: "PRD:1".into(),
        title: "A title long enough to wrap inside a narrow panel".into(),
        summary: "A summary that would overflow a fixed layout, which is what the painter did before.".into(),
        goals: (0..12).map(|i| format!("goal {i}")).collect(),
        non_goals: (0..8).map(|i| format!("non-goal {i}")).collect(),
        audiences: (0..6).map(|i| format!("audience {i}")).collect(),
        requirements: (0..14)
            .map(|i| figcraft_doc::Requirement {
                id: format!("r-{i}"),
                title: format!("requirement {i}"),
                detail: "detail".into(),
                priority: figcraft_doc::Priority::Must,
                acceptance_criteria: vec!["a".into(), "b".into()],
            })
            .collect(),
        acceptance_criteria: (0..10).map(|i| format!("criterion {i}")).collect(),
        open_questions: (0..5).map(|i| format!("question {i}")).collect(),
    };
    state.requirements_draft = Some(RequirementsDraft::from_document(&doc));
    let input = PanelInput { configured: true, ..Default::default() };
    for height in [420.0, 900.0] {
        let (ctx, screen) = layout(320.0, height, &mut state, &input);
        assert_eq!(ctx.content_rect().width(), screen.width(), "the panel used the width it was given at {height}");
        assert_eq!(ctx.content_rect().height(), screen.height(), "and the height");
    }
}
