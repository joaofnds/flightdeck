//! The Recording section of Settings, where the user sets the retention period. The recording holds
//! the period, so this page reads it from there and writes it back, and keeps no copy elsewhere.

use std::io;
use std::path::Path;

use collector::recording::private::PrivateDirectory;
use gpui::{Context, IntoElement, Stateful, div, prelude::*, px, rgb};
use persistence::retention::Period;

use crate::charts::{self, BLUE, MUTED, ORANGE, PANEL, TEXT};
use crate::live_dashboard::LiveDashboard;

/// What the page shows of the period, and what a save refused.
pub(crate) struct RetentionChoice {
    shown: Retention,
    notice: Option<String>,
}

impl Default for RetentionChoice {
    fn default() -> Self {
        Self {
            shown: Retention::from_period(Period::DEFAULT),
            notice: None,
        }
    }
}

/// A retention period as the page states it.
#[derive(Clone, Copy)]
struct Retention {
    amount: u64,
    unit: RetentionUnit,
}

impl Retention {
    /// States `period` in the largest unit that fits it exactly. Only a file edited by hand holds
    /// a period no unit fits, which rounds up to whole minutes.
    fn from_period(period: Period) -> Self {
        let seconds = period.seconds();
        let unit = if seconds.is_multiple_of(RetentionUnit::Days.seconds()) {
            RetentionUnit::Days
        } else if seconds.is_multiple_of(RetentionUnit::Hours.seconds()) {
            RetentionUnit::Hours
        } else {
            RetentionUnit::Minutes
        };

        Self {
            amount: seconds.div_ceil(unit.seconds()),
            unit,
        }
    }

    fn seconds(self) -> u64 {
        self.amount * self.unit.seconds()
    }

    /// The same period in `unit`, rounded up, so changing the unit never shortens it.
    fn in_unit(self, unit: RetentionUnit) -> Self {
        Self {
            amount: self.seconds().div_ceil(unit.seconds()),
            unit,
        }
    }

    fn with_amount(self, amount: u64) -> Self {
        Self { amount, ..self }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RetentionUnit {
    Minutes,
    Hours,
    Days,
}

impl RetentionUnit {
    const ALL: [Self; 3] = [Self::Minutes, Self::Hours, Self::Days];

    const fn seconds(self) -> u64 {
        match self {
            Self::Minutes => 60,
            Self::Hours => 3_600,
            Self::Days => 86_400,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Minutes => "Minutes",
            Self::Hours => "Hours",
            Self::Days => "Days",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::Minutes => "retention-unit:Minutes",
            Self::Hours => "retention-unit:Hours",
            Self::Days => "retention-unit:Days",
        }
    }
}

impl LiveDashboard {
    /// Reads the period the recording holds, keeping the unit the user chose while it states the
    /// same period.
    pub(crate) fn open_settings_retention(&mut self) {
        match open_settings_retention_period(self.worker.root()) {
            Ok(period) => {
                let choice = &mut self.settings.retention;
                if choice.shown.seconds() != period.seconds() {
                    choice.shown = Retention::from_period(period);
                }
                choice.notice = None;
            }
            Err(error) => {
                self.settings.retention.notice = Some(format!(
                    "The retention period did not read, so nothing is deleted until you choose \
                    one here: {error}"
                ));
            }
        }
    }

    /// Saves `retention` unless it lies outside what a period may be. The page goes on showing the
    /// period the recording keeps when the save fails, and says so.
    fn set_retention(&mut self, retention: Retention, cx: &mut Context<Self>) {
        let Some(period) = Period::from_seconds(retention.seconds()) else {
            return;
        };

        let saved = set_retention_save(self.worker.root(), period);
        let choice = &mut self.settings.retention;
        match saved {
            Ok(()) => {
                choice.shown = retention;
                choice.notice = None;
            }
            Err(error) => {
                choice.notice = Some(format!("The retention period was not saved: {error}"));
            }
        }
        cx.notify();
    }

    pub(crate) fn settings_view_retention(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let choice = &self.settings.retention;

        div()
            .w_full()
            .max_w(px(760.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Recording"),
            )
            .child(
                div()
                    .rounded(px(7.0))
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(charts::alpha(TEXT, 0.12))
                    .child(self.settings_view_retention_row(cx)),
            )
            .children(choice.notice.as_deref().map(settings_view_retention_notice))
    }

    fn settings_view_retention_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .p(px(12.0))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(16.0))
            .child(
                div()
                    .flex_grow_1()
                    .flex_basis(px(240.0))
                    .min_w(px(0.0))
                    .max_w_full()
                    .child(
                        div()
                            .min_h(px(24.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Keep readings for"),
                    )
                    .child(
                        div()
                            .debug_selector(|| "retention-description".into())
                            .mt(px(4.0))
                            .text_size(px(13.0))
                            .line_height(px(20.0))
                            .text_color(rgb(MUTED))
                            .child(
                                "Readings older than this stop showing, then are deleted \
                                oldest first and cannot be brought back.",
                            ),
                    ),
            )
            .child(self.settings_view_retention_controls(cx))
    }

    fn settings_view_retention_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.settings.retention.shown;
        let amount_label = format!("{} {}", shown.amount, shown.unit.label());

        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .child(
                settings_retention_control("retention-decrease", "Keep readings for less")
                    .child("−")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_retention(shown.with_amount(shown.amount - 1), cx);
                    })),
            )
            .child(
                div()
                    .id("retention-amount")
                    .debug_selector(move || {
                        format!("retention:{}:{}", shown.amount, shown.unit.label())
                    })
                    .role(gpui::Role::Status)
                    .aria_label(amount_label)
                    .min_w(px(32.0))
                    .flex()
                    .justify_center()
                    .child(shown.amount.to_string()),
            )
            .child(
                settings_retention_control("retention-increase", "Keep readings for longer")
                    .child("+")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_retention(shown.with_amount(shown.amount + 1), cx);
                    })),
            )
            .children(
                RetentionUnit::ALL.map(|unit| Self::settings_view_retention_unit(unit, shown, cx)),
            )
    }

    fn settings_view_retention_unit(
        unit: RetentionUnit,
        shown: Retention,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = shown.unit == unit;

        settings_retention_control(unit.selector(), unit.label())
            .aria_selected(selected)
            .when(selected, |control| {
                control
                    .bg(rgb(BLUE))
                    .text_color(rgb(PANEL))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
            })
            .child(unit.label())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_retention(shown.in_unit(unit), cx);
            }))
    }
}

/// The period `root` holds. A recording that does not exist yet keeps the default.
fn open_settings_retention_period(root: &Path) -> io::Result<Period> {
    match PrivateDirectory::open(root) {
        Ok(directory) => Period::read(&directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Period::DEFAULT),
        Err(error) => Err(error),
    }
}

fn set_retention_save(root: &Path, period: Period) -> io::Result<()> {
    period.write(&PrivateDirectory::open(root)?)
}

fn settings_retention_control(id: &'static str, label: &'static str) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(gpui::Role::Button)
        .aria_label(label)
        .focusable()
        .tab_stop(true)
        .focus(charts::focus_ring)
        .cursor_pointer()
        .min_w(px(44.0))
        .min_h(px(44.0))
        .px(px(8.0))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(charts::alpha(TEXT, 0.24))
        .hover(|style| style.border_color(rgb(TEXT)))
}

fn settings_view_retention_notice(notice: &str) -> impl IntoElement {
    div()
        .id("retention-notice")
        .debug_selector(|| "retention-notice".into())
        .role(gpui::Role::Status)
        .aria_label(notice.to_owned())
        .text_size(px(13.0))
        .text_color(rgb(ORANGE))
        .child(notice.to_owned())
}
