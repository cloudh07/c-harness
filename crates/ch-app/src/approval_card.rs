//! One tool call that omp is holding until the person answers.

use ch_core::{ApprovalDecision, ApprovalRequest, Risk, classify};
use ch_ui::{Palette, Status};
use gpui_kit::{
    ClickEvent, Context, Div, FontWeight, InteractiveElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px, rgb,
};

use crate::session_view::SessionView;
use crate::widgets::{MONO, Tone, button, muted};

const DETAILS_HEIGHT: f32 = 160.0;

fn risk_label(risk: Risk) -> (&'static str, Status) {
    match risk {
        Risk::High => ("High risk", Status::Error),
        Risk::Medium => ("Medium risk", Status::NeedsYou),
        Risk::Low => ("Low risk", Status::Done),
    }
}

pub fn approval_card(p: Palette, request: &ApprovalRequest, cx: &Context<SessionView>) -> Div {
    let assessment = classify(request);
    let (label, status) = risk_label(assessment.risk);
    let id = request.id().to_string();

    let choices = [
        (
            ApprovalDecision::ApproveOnce,
            "approve-once",
            "Approve once",
            Tone::Primary,
            request.can_approve(),
        ),
        (
            ApprovalDecision::ApproveForSession,
            "approve-session",
            "Approve for this session",
            Tone::Plain,
            request.approve_for_session_label().is_some(),
        ),
        (ApprovalDecision::Deny, "deny", "Deny", Tone::Danger, true),
    ];
    let buttons =
        choices
            .into_iter()
            .filter(|choice| choice.4)
            .map(|(decision, key, text, tone, _)| {
                let id = id.clone();
                button(p, format!("{key}-{id}"), text, tone, true).on_click(cx.listener(
                    move |this: &mut SessionView, _: &ClickEvent, _, cx| {
                        this.answer(&id, decision, cx);
                    },
                ))
            });
    let actions = div().flex().flex_wrap().gap_2().children(buttons);

    let details = request.details().to_string();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_md()
        .border_1()
        .border_l_4()
        .border_color(rgb(status.color()))
        .bg(rgb(p.surface))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("omp wants to use {}", request.tool())),
                )
                .child(
                    div()
                        .text_xs()
                        .px_2()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(status.color()))
                        .child(label),
                ),
        )
        .child(muted(p, assessment.reason))
        .children(
            request
                .reason()
                .map(|reason| div().text_sm().child(reason.to_string())),
        )
        .children(request.from_mcp().then(|| {
            muted(
                p,
                "This tool comes from an MCP server, not from omp itself.",
            )
        }))
        .children((!details.is_empty()).then(|| {
            div()
                .id(format!("approval-details-{id}"))
                .max_h(px(DETAILS_HEIGHT))
                .overflow_y_scroll()
                .p_2()
                .rounded_md()
                .bg(rgb(p.bg))
                .font_family(MONO)
                .text_xs()
                .child(gpui_kit::base::SelectableText::new("details", details))
        }))
        .children((!request.can_approve()).then(|| {
            muted(
                p,
                "omp offered no way to approve this call, so it can only be denied.",
            )
        }))
        .child(actions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_risk_never_shares_a_color_with_low() {
        let (_, high) = risk_label(Risk::High);
        let (_, low) = risk_label(Risk::Low);
        assert_ne!(high.color(), low.color());
    }
}
