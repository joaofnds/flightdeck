use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px, rgb};
use num_traits::ToPrimitive;

use super::{Entry, Metric, Ranking, Rankings, TopApps};
use crate::charts::{BLUE, GREEN, GRID, MUTED, ORANGE, PANEL};
use crate::dashboard::control;
use crate::live_chart_view::ChartHelp;
use crate::live_dashboard::LiveDashboard;
use crate::live_hover::Target;
use crate::live_source::{AppKey, identity_validate};

impl LiveDashboard {
    pub fn app_rankings(&self, metrics: &[usize], cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .gap(px(16.0))
            .children(
                metrics
                    .iter()
                    .take(3)
                    .map(|index| self.app_ranking(*index, cx)),
            )
            .into_any_element()
    }

    fn app_ranking(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let metric = [Metric::Cpu, Metric::Resident, Metric::Power][index];
        let mut card = div()
            .flex_1()
            .min_w_0()
            .p(px(16.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .child(ranking_heading(metric));

        let (rankings, target) = match self.ranking_reading() {
            Ok(reading) => reading,
            Err(error) => return card.child(notice(&error)).into_any_element(),
        };
        let ranking = &rankings.metrics[index];

        card = card
            .child(notice(&format!(
                "At Unix second {} · top measured totals",
                target.second
            )))
            .child(Self::app_ranking_entries(ranking, &target, cx))
            .child(notice(&coverage(ranking)))
            .child(notice(&format!(
                "{} Apps · membership {} · {} ungrouped processes",
                rankings.app_count,
                if rankings.membership_complete {
                    "complete"
                } else {
                    "partial"
                },
                rankings.ungrouped_members
            )));

        if index == 0 {
            card = card.child(notice(
                "App network, disk and GPU readings are unavailable.",
            ));
        }

        card.into_any_element()
    }

    fn ranking_reading(&self) -> Result<(&TopApps, Target), String> {
        let snapshot = self
            .reading_snapshot()
            .ok_or("App totals unavailable while reading the selection")?;

        if let Some(preview) = &self.preview {
            if let Some(error) = &preview.failure {
                return Err(format!("App totals unavailable: {error}"));
            }

            if !preview.complete {
                return Err("Loading exact App totals".into());
            }
        }

        let rankings = snapshot
            .rankings
            .as_deref()
            .ok_or("App totals unavailable at this observation")?;
        let rankings = match rankings {
            Rankings::Ready(rankings) => rankings,
            Rankings::Unavailable(error) => return Err(format!("App totals unavailable: {error}")),
        };

        identity_validate(&rankings.selected, &snapshot.selection["selected"])?;
        let second = snapshot.selection["second"]
            .as_u64()
            .ok_or("App observation time unavailable")?;
        let target = Target::exact(second, &rankings.selected)?;

        Ok((rankings, target))
    }

    fn app_ranking_entries(
        ranking: &Ranking,
        target: &Target,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if ranking.entries.is_empty() {
            return notice("No measured App totals at this observation").into_any_element();
        }

        div()
            .id(SharedString::from(format!(
                "ranking-{}",
                ranking.metric.title()
            )))
            .max_h(px(280.0))
            .overflow_y_scroll()
            .children(
                ranking
                    .entries
                    .iter()
                    .take(super::RANKS_MAX)
                    .map(|entry| Self::app_ranking_entry(entry, ranking, target, cx)),
            )
            .into_any_element()
    }

    fn app_ranking_entry(
        entry: &Entry,
        ranking: &Ranking,
        target: &Target,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = entry.app.key.clone();
        let target = target.clone();
        let keyboard_key = key.clone();
        let keyboard_target = target.clone();
        let metric = ranking.metric;
        let value = value(metric, entry);
        let selector = format!("ranking:{}:{}:{}", metric.title(), entry.app.name, value);
        let coverage = format!(
            "{} measured · {} unavailable",
            entry.measured_members, entry.unavailable_members
        );

        div()
            .id(SharedString::from(format!(
                "rank-{}-{}",
                key.kind, key.bytes_hex
            )))
            .debug_selector(move || selector.clone())
            .role(gpui::Role::Button)
            .aria_label(format!(
                "Inspect {} at Unix second {}; {value}; {coverage}",
                entry.app.name, target.second
            ))
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .focus(|style| style.bg(rgb(0x32_46_61)))
            .hover(|style| style.bg(rgb(0x20_29_36)))
            .border_b_1()
            .border_color(rgb(GRID))
            .py(px(7.0))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_ranked_app(&key, &target, cx);
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.open_ranked_app(&keyboard_key, &keyboard_target, cx);
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }),
            )
            .child(ranking_cell(entry, ranking, value))
            .child(notice(&coverage))
            .into_any_element()
    }

    fn open_ranked_app(&mut self, key: &AppKey, target: &Target, cx: &mut Context<Self>) {
        self.request.second = Some(target.second);
        self.request.identity.clone_from(&target.identity);
        self.request.app = Some(key.clone());
        self.request.apps_offset = 0;
        self.request.members_offset = 0;
        self.executable = None;
        self.app_picker_open = false;
        self.navigate("apps", cx);
    }
}

fn ranking_heading(metric: Metric) -> gpui::Div {
    let selector = format!("ranking-help:{}", metric.title());
    let help = metric_help(metric);

    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .child(
            div()
                .text_size(px(15.0))
                .child(format!("{} · Top apps", metric.title())),
        )
        .child(
            control("ⓘ")
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .aria_label(format!("About {} App rankings", metric.title()))
                .aria_description(help)
                .tooltip(move |_, cx| cx.new(|_| ChartHelp(help.into())).into()),
        )
}

fn ranking_cell(entry: &Entry, ranking: &Ranking, value: String) -> gpui::Div {
    let maximum = ranking.entries.first().map_or(0, |entry| entry.amount);
    let fraction = ranking_fraction(entry, maximum);
    let track = format!(
        "ranking-track:{}:{}",
        ranking.metric.title(),
        entry.app.key.bytes_hex
    );
    let fill = format!(
        "ranking-fill:{}:{}",
        ranking.metric.title(),
        entry.app.key.bytes_hex
    );
    let color = metric_color(ranking.metric);

    div()
        .flex()
        .gap(px(8.0))
        .items_center()
        .justify_between()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(entry.app.name.clone()),
        )
        .child(
            div()
                .debug_selector(move || track.clone())
                .relative()
                .w(gpui::relative(0.48))
                .min_w(px(85.0))
                .h(px(24.0))
                .rounded(px(2.0))
                .bg(rgb(GRID))
                .overflow_hidden()
                .child(
                    div()
                        .debug_selector(move || fill.clone())
                        .absolute()
                        .left_0()
                        .top_0()
                        .h_full()
                        .w(gpui::relative(fraction))
                        .bg(rgb(color))
                        .opacity(0.3),
                )
                .child(
                    div()
                        .relative()
                        .size_full()
                        .px(px(6.0))
                        .flex()
                        .items_center()
                        .justify_end()
                        .text_color(rgb(color))
                        .child(value),
                ),
        )
}

fn ranking_fraction(entry: &Entry, maximum: u64) -> f32 {
    assert!(entry.amount <= maximum);
    if maximum == 0 {
        return 0.0;
    }

    let value = entry
        .amount
        .to_f64()
        .unwrap_or_else(|| unreachable!("u64 fits f64"));
    let maximum = maximum
        .to_f64()
        .unwrap_or_else(|| unreachable!("u64 fits f64"));

    (value / maximum)
        .to_f32()
        .unwrap_or_else(|| unreachable!("fraction fits f32"))
}

fn metric_color(metric: Metric) -> u32 {
    match metric {
        Metric::Cpu => BLUE,
        Metric::Resident => GREEN,
        Metric::Power => ORANGE,
    }
}

fn metric_help(metric: Metric) -> &'static str {
    match metric {
        Metric::Cpu => {
            "One-core CPU time divided by the captured continuous interval. Known \
            subtotals exclude unavailable members; zero is a measured reading."
        }
        Metric::Resident => {
            "Selected resident memory, summed across measured live process \
            instances. It is separate from machine used memory and physical footprint."
        }
        Metric::Power => {
            "Recorded process energy divided by the captured continuous interval. \
            It is separate from GPU power and whole-machine power."
        }
    }
}

fn notice(text: &str) -> gpui::Div {
    let text = crate::live_notices::recording_context::bounded(text, 512);
    div()
        .debug_selector(|| text.clone())
        .mt(px(8.0))
        .text_size(px(11.0))
        .text_color(rgb(MUTED))
        .child(text)
}

fn coverage(ranking: &Ranking) -> String {
    format!(
        "{} unavailable · {} overflowed · {} partial known subtotals",
        ranking.unavailable, ranking.overflowed, ranking.partial
    )
}

fn value(metric: Metric, entry: &Entry) -> String {
    let value = match metric {
        Metric::Cpu => crate::live_app_views::rate(entry.rate, "%"),
        Metric::Resident => crate::live_app_views::memory(Some(entry.amount)),
        Metric::Power => match entry.rate.and_then(|value| value.to_f32()) {
            Some(value) => crate::live_projection::quantity_text(value, "W"),
            None => "Unavailable".into(),
        },
    };
    if entry.unavailable_members > 0 {
        return format!("{value} known");
    }

    value
}
