use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px, rgb};
use num_traits::ToPrimitive;

use super::{Entry, Metric, Ranking, Rankings, TopApps};
use crate::charts::{BLUE, GREEN, GRID, MUTED, ORANGE, PANEL, PURPLE, TEXT, YELLOW};
use crate::live_chart_view::{ChartHelp, chart_head_button};
use crate::live_dashboard::LiveDashboard;
use crate::live_hover::Target;
use crate::live_source::{AppKey, identity_validate};

/// One card of the Overview's top-app grid, in the prototype's order.
#[derive(Clone, Copy)]
enum OverviewSlot {
    /// A recorded measure, by its index among the rankings' metrics.
    Ranked(usize),
    /// A measure the collector does not record per app.
    Uncaptured(&'static str, &'static str),
}

/// The title a ranking card carries. Beside an App chart the prototype calls the card Top apps
/// and names the chart's unit, while on the Overview it names the measure.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RankingHeading {
    Measure,
    TopApps,
}

const OVERVIEW_SLOTS: [OverviewSlot; 6] = [
    OverviewSlot::Ranked(0),
    OverviewSlot::Ranked(1),
    OverviewSlot::Uncaptured(
        "Network",
        "Per-app network traffic is not recorded by this collector, so no app is ranked.",
    ),
    OverviewSlot::Ranked(3),
    OverviewSlot::Ranked(4),
    OverviewSlot::Ranked(2),
];

/// The measures a ranking card shows, by index: the recording's four, then GPU time.
const RANKED_METRICS: [Metric; 5] = [
    Metric::Cpu,
    Metric::Resident,
    Metric::Power,
    Metric::Disk,
    Metric::Gpu,
];

/// The number of cards in the Overview's top-app grid.
pub(crate) const OVERVIEW_SLOT_COUNT: usize = OVERVIEW_SLOTS.len();

impl LiveDashboard {
    pub fn app_rankings(&self, metrics: &[usize], cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .gap(px(12.0))
            .children(
                metrics
                    .iter()
                    .take(3)
                    .map(|index| self.app_ranking(*index, RankingHeading::TopApps, cx)),
            )
            .into_any_element()
    }

    /// One row of the Overview's top-app grid. A short last row keeps the grid's column widths.
    pub fn overview_rankings(
        &self,
        slots: &[usize],
        columns: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        assert!(slots.len() <= columns);
        div()
            .flex()
            .gap(px(12.0))
            .children(slots.iter().take(3).map(|slot| {
                assert!(*slot < OVERVIEW_SLOT_COUNT);
                match OVERVIEW_SLOTS[*slot] {
                    OverviewSlot::Ranked(index) => {
                        self.app_ranking(index, RankingHeading::Measure, cx)
                    }
                    OverviewSlot::Uncaptured(title, help) => uncaptured_ranking(title, help),
                }
            }))
            .children((slots.len()..columns).map(|_| div().flex_1().min_w_0()))
            .into_any_element()
    }

    fn app_ranking(
        &self,
        index: usize,
        heading: RankingHeading,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let metric = RANKED_METRICS[index];
        let card = ranking_card(metric_name(metric));
        if matches!(metric, Metric::Gpu) {
            return self.gpu_ranking(card, heading, cx);
        }

        let (rankings, target) = match self.ranking_reading() {
            Ok(reading) => reading,
            Err(error) => {
                return card
                    .child(ranking_title(metric, heading))
                    .child(notice(&error))
                    .into_any_element();
            }
        };
        let ranking = &rankings.metrics[index];
        let membership = format!(
            "{} Apps, membership {}, {} ungrouped processes",
            rankings.app_count,
            if rankings.membership_complete {
                "complete"
            } else {
                "partial"
            },
            rankings.ungrouped_members
        );

        card.id(SharedString::from(format!(
            "ranking-card-{}",
            metric.title()
        )))
        .role(gpui::Role::Group)
        .aria_label(format!(
            "{} top apps at Unix second {}; {membership}",
            metric_name(metric),
            target.second
        ))
        .child(ranking_title(metric, heading))
        .child(Self::app_ranking_entries(ranking, &target, cx))
        .into_any_element()
    }

    /// The GPU ranking of the selected second, from the GPU time the app sampled itself.
    fn gpu_ranking(
        &self,
        card: gpui::Div,
        heading: RankingHeading,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let card = card.child(ranking_title(Metric::Gpu, heading));
        let Some(snapshot) = self.reading_snapshot() else {
            return card
                .child(notice(
                    "GPU time by App unavailable while reading the selection",
                ))
                .into_any_element();
        };
        let selection = &snapshot.selection;
        let reading = crate::live_gpu_apps::ranking(selection).and_then(|ranking| {
            let second = selection["second"]
                .as_u64()
                .ok_or("GPU App observation time unavailable")?;
            Ok((ranking, Target::exact(second, &selection["selected"])?))
        });
        match reading {
            Ok((ranking, target)) => card
                .id(SharedString::from("ranking-card-GPU time"))
                .role(gpui::Role::Group)
                .aria_label(format!("GPU top apps at Unix second {}", target.second))
                .child(Self::app_ranking_entries(&ranking, &target, cx))
                .into_any_element(),
            Err(error) => card.child(notice(&error)).into_any_element(),
        }
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
            return div()
                .py(px(20.0))
                .text_size(px(11.0))
                .line_height(px(17.0))
                .text_color(rgb(MUTED))
                .child("No active app readings.")
                .into_any_element();
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
                    .enumerate()
                    .map(|(position, entry)| {
                        let last = position + 1 == ranking.entries.len().min(super::RANKS_MAX);
                        Self::app_ranking_entry(entry, ranking, target, last, cx)
                    }),
            )
            .into_any_element()
    }

    fn app_ranking_entry(
        entry: &Entry,
        ranking: &Ranking,
        target: &Target,
        last: bool,
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
            .when(!last, |row| {
                row.border_b_1().border_color(rgb(crate::charts::LINE))
            })
            .min_h(px(34.0))
            .px(px(2.0))
            .py(px(3.0))
            .flex()
            .items_center()
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
            .child(ranking_cell(entry, ranking, value, coverage))
            .into_any_element()
    }

    fn open_ranked_app(&mut self, key: &AppKey, target: &Target, cx: &mut Context<Self>) {
        self.request.second = Some(target.second);
        self.request.identity.clone_from(&target.identity);
        self.request.app = Some(key.clone());
        self.request.apps_offset = 0;
        self.request.members_offset = 0;
        self.request.executable = None;
        self.picker = None;
        self.navigate("apps", cx);
    }
}

fn ranking_card(name: &'static str) -> gpui::Div {
    div()
        .debug_selector(move || format!("ranking-card:{name}"))
        .flex_1()
        .min_w_0()
        .px(px(12.0))
        .pt(px(8.0))
        .pb(px(8.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(rgb(crate::charts::LINE))
        .bg(rgb(PANEL))
}

/// The Network page's ranking, which has no Apps to list because the collector records no
/// traffic per process.
pub(crate) fn network_ranking() -> AnyElement {
    ranking_card("Network")
        .child(top_apps_title("ranking-heading:Network".into(), "MB/s"))
        .child(
            div()
                .debug_selector(|| "ranking-uncaptured:Network".into())
                .py(px(8.0))
                .text_size(px(12.0))
                .text_color(rgb(MUTED))
                .child("Not recorded per app"),
        )
        .into_any_element()
}

fn ranking_title(metric: Metric, heading: RankingHeading) -> gpui::Div {
    if heading == RankingHeading::Measure {
        return ranking_heading(metric, metric_help(metric).into());
    }
    top_apps_title(
        format!("ranking-heading:{}", metric.title()),
        metric_unit(metric),
    )
}

fn top_apps_title(selector: String, unit: &'static str) -> gpui::Div {
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap(px(6.0))
        .h(px(40.0))
        .child(
            div()
                .text_size(px(14.0))
                .line_height(px(21.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Top apps"),
        )
        .child(
            div()
                .text_size(px(11.0))
                .line_height(px(16.5))
                .text_color(rgb(MUTED))
                .child(unit),
        )
}

/// The unit the prototype names beside Top apps, the unit of the chart the card ranks.
fn metric_unit(metric: Metric) -> &'static str {
    match metric {
        Metric::Cpu => "% of one core",
        Metric::Resident => "GiB",
        Metric::Power => "W",
        Metric::Disk => "MB/s",
        Metric::Gpu => "% of interval",
    }
}

fn ranking_heading(metric: Metric, help: String) -> gpui::Div {
    let selector = format!("ranking-help:{}", metric.title());
    let label = format!("About {} App rankings", metric.title());
    ranking_heading_text(metric_name(metric), selector, label, help)
}

fn ranking_heading_text(
    name: &'static str,
    selector: String,
    label: String,
    help: String,
) -> gpui::Div {
    let description = help.clone();
    div()
        .flex()
        .items_center()
        .gap(px(4.0))
        .min_h(px(44.0))
        .child(
            div()
                .text_size(px(14.0))
                .line_height(px(21.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(name),
        )
        .child(
            chart_head_button(selector.clone(), "ⓘ")
                .debug_selector(move || selector.clone())
                .px(px(8.0))
                .aria_label(label)
                .aria_description(description)
                .tooltip(move |_, cx| cx.new(|_| ChartHelp(help.clone())).into()),
        )
}

fn uncaptured_ranking(name: &'static str, help: &'static str) -> AnyElement {
    ranking_card(name)
        .child(ranking_heading_text(
            name,
            format!("ranking-help:{name}"),
            format!("About {name} App rankings"),
            help.into(),
        ))
        .child(
            div()
                .debug_selector(move || format!("ranking-uncaptured:{name}"))
                .py(px(8.0))
                .text_size(px(12.0))
                .text_color(rgb(MUTED))
                .child("Not recorded per app"),
        )
        .into_any_element()
}

/// The name the prototype gives each ranked measure.
fn metric_name(metric: Metric) -> &'static str {
    match metric {
        Metric::Cpu => "CPU",
        Metric::Resident => "Memory",
        Metric::Power => "Energy",
        Metric::Disk => "Disk",
        Metric::Gpu => "GPU",
    }
}

/// An app's name as the prototype lists it, without the bundle's `.app` extension.
fn ranking_name(name: &str) -> &str {
    name.strip_suffix(".app").unwrap_or(name)
}

fn ranking_cell(entry: &Entry, ranking: &Ranking, value: String, coverage: String) -> gpui::Div {
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

    // The prototype gives the name 1.2 parts of the row and the bar 1 part.
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .gap(px(8.0))
        .items_center()
        .text_size(px(13.0))
        .child(
            div()
                .debug_selector(move || coverage.clone())
                .w(gpui::relative(0.545))
                .min_w_0()
                .truncate()
                .child(ranking_name(&entry.app.name).to_owned()),
        )
        .child(
            div()
                .debug_selector(move || track.clone())
                .relative()
                .flex_1()
                .min_w(px(70.0))
                .h(px(24.0))
                .rounded(px(4.0))
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
                        .bg(crate::charts::alpha(color, 0.35)),
                )
                .child(
                    div()
                        .relative()
                        .size_full()
                        .px(px(5.0))
                        .flex()
                        .items_center()
                        .justify_end()
                        .text_size(px(11.0))
                        .text_color(rgb(TEXT))
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
        Metric::Disk => YELLOW,
        Metric::Gpu => PURPLE,
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
        Metric::Disk => {
            "Process-attributed read plus written bytes over the selected observation interval. \
            Only jointly measured members contribute; known subtotals exclude unavailable members. \
            Separate from physical-device driver throughput."
        }
        Metric::Gpu => {
            "GPU time each App's processes took over the interval, from the GPU driver's own \
            count, as a share of the interval. The app samples it each second while it runs."
        }
    }
}

fn notice(text: &str) -> gpui::Div {
    let text = crate::bounded::bounded(text, 512);
    div()
        .debug_selector(|| text.clone())
        .mt(px(8.0))
        .text_size(px(11.0))
        .text_color(rgb(MUTED))
        .child(text)
}

fn value(metric: Metric, entry: &Entry) -> String {
    match metric {
        Metric::Cpu | Metric::Gpu => entry
            .rate
            .map_or_else(|| "Unavailable".into(), |rate| format!("{rate:.1}%")),
        Metric::Resident => crate::live_app_views::memory(Some(entry.amount)),
        Metric::Power => crate::live_app_views::event_rate(entry.rate, "W"),
        Metric::Disk => crate::live_app_views::disk_rate(entry.rate),
    }
}

#[cfg(test)]
mod tests {
    use super::ranking_name;

    #[test]
    fn ranked_apps_drop_only_the_bundle_extension() {
        // Goal: a ranked app reads as the prototype lists it.
        // Method: name a bundle, a bare executable and a name that only contains ".app".
        assert_eq!(ranking_name("Brave Browser.app"), "Brave Browser");
        assert_eq!(ranking_name("claude"), "claude");
        assert_eq!(ranking_name("com.apple.appkit"), "com.apple.appkit");
    }
}
