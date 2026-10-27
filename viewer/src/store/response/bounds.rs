use super::QueryContext;

pub(super) const RESIDENT_POINT_BYTES: usize = 512 * size_of::<serde_json::Value>();
pub(super) const ATTRIBUTION_POINT_BYTES: usize = 768 * size_of::<serde_json::Value>();
pub(super) const TRACE_POINT_BYTES: usize = 768 * size_of::<serde_json::Value>();

/// Includes the largest possible eight cohort identities and the fixed header/coverage shape.
pub(super) fn history_result_bytes(context: &QueryContext, point_bytes: usize) -> usize {
    let mut names = 0;
    let mut name_bytes = 0;
    for second in context.seconds() {
        for app in &second.apps().apps {
            names = (names + 1).min(8);
            name_bytes =
                name_bytes.max(12 * app.name.bytes.len() + 2 * app.name.display_name.len());
        }
    }

    512 * size_of::<serde_json::Value>()
        + context.seconds().len() * point_bytes
        + names * name_bytes
}
