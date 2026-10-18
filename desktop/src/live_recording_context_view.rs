use gpui::{Context, Div, IntoElement, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{BLUE, MUTED, ORANGE};
use crate::dashboard::control;
use crate::live_dashboard::LiveDashboard;
use crate::live_notices::recording_context::{
    self, AlternativeChoice, AlternativePage, RecordingContext, SelectionToken,
};

impl LiveDashboard {
    pub(super) fn header_recording_context(&self, mut header: Div) -> Div {
        let catalog = self
            .context_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.catalog);
        let selection = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection);
        header = header.children(
            recording_context::source(RecordingContext { catalog, selection })
                .into_iter()
                .map(|line| div().text_size(px(10.0)).text_color(rgb(MUTED)).child(line)),
        );
        header = header.children(
            crate::live_notices::losses(catalog, selection)
                .into_iter()
                .map(|message| {
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(ORANGE))
                        .child(message)
                }),
        );
        if let Some(error) = &self.error {
            header = header.child(div().text_size(px(11.0)).text_color(rgb(ORANGE)).child(
                format!(
                    "Reading unavailable: {}",
                    recording_context::bounded(error, 512)
                ),
            ));
        }

        header
    }

    pub(super) fn header_alternatives(&self, header: Div, cx: &mut Context<Self>) -> Div {
        let Some(snapshot) = &self.snapshot else {
            return header;
        };
        let page = match AlternativePage::read(
            RecordingContext {
                catalog: &snapshot.catalog,
                selection: &snapshot.selection,
            },
            self.native.alternative_page,
        ) {
            Ok(page) => page,
            Err(error) => return header.child(div().text_color(rgb(ORANGE)).child(error)),
        };
        if page.total < 2 {
            return header;
        }

        let mut choices = div()
            .id("observation-alternatives")
            .flex()
            .flex_wrap()
            .gap(px(5.0))
            .max_h(px(128.0))
            .overflow_y_scroll();
        for choice in &page.choices {
            let token = page.token.clone();
            let identity = choice.identity.clone();
            let generation = self.generation;
            choices = choices.child(
                control(&choice.label)
                    .text_size(px(10.0))
                    .aria_selected(choice.selected)
                    .when(choice.selected, |button| button.text_color(rgb(BLUE)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose_alternative(&token, &identity, generation, cx);
                    })),
            );
        }

        header.child(
            div()
                .mt(px(6.0))
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(MUTED))
                        .child("Committed selection · same-second observation controls"),
                )
                .child(self.header_alternative_pages(&page, cx))
                .child(choices),
        )
    }

    fn header_alternative_pages(
        &self,
        page: &AlternativePage,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let first = u32::from(page.index) * 64 + 1;
        let last = (u32::from(page.index) + 1) * 64;
        let mut navigation = div().flex().items_center().gap(px(8.0)).child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .child(format!(
                    "Same-second observations {first}–{} of {} · page {} of {}",
                    last.min(u32::from(page.total)),
                    page.total,
                    page.index + 1,
                    page.count
                )),
        );
        for (label, target) in [
            ("Previous observations", page.previous),
            ("Next observations", page.next),
        ] {
            let token = page.token.clone();
            let generation = self.generation;
            let button = control(label)
                .when(target.is_none(), |button| {
                    button
                        .opacity(0.4)
                        .tab_stop(false)
                        .cursor_default()
                        .aria_description("Unavailable at this page boundary")
                })
                .when_some(target, |button, target| {
                    button.on_click(cx.listener(move |this, _, _, cx| {
                        if this.generation == generation {
                            if let Some(snapshot) = &this.snapshot {
                                if token.matches(RecordingContext {
                                    catalog: &snapshot.catalog,
                                    selection: &snapshot.selection,
                                }) {
                                    this.native.alternative_page = target;
                                    cx.notify();
                                }
                            }
                        }
                    }))
                });
            navigation = navigation.child(button);
        }

        navigation
    }

    fn choose_alternative(
        &mut self,
        token: &SelectionToken,
        identity: &Value,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if self.generation != generation {
            return;
        }

        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let Some(identity) = token.choose(&AlternativeChoice {
            context: RecordingContext {
                catalog: &snapshot.catalog,
                selection: &snapshot.selection,
            },
            identity,
        }) else {
            return;
        };

        self.request.second = Some(token.second);
        self.request.identity = Some(identity);
        self.request.offset = 0;
        self.request.apps_offset = 0;
        self.request.members_offset = 0;
        self.request.cpu_offset = 0;
        self.submit();
        cx.notify();
    }
}
