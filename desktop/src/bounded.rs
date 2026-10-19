/// The first `maximum` characters of `text`, marked with an ellipsis where it was cut.
pub fn bounded(text: &str, maximum: usize) -> String {
    assert!(maximum <= 4096);
    let mut characters = text.chars();
    let mut output: String = characters.by_ref().take(maximum).collect();
    if characters.next().is_some() {
        output.push('…');
    }

    output
}
