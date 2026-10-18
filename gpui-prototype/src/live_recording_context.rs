use serde_json::Value;

pub const ALTERNATIVES_MAX: usize = 4096;
const PAGE_SIZE: usize = 64;

#[derive(Clone, Copy)]
pub struct RecordingContext<'a> {
    pub catalog: &'a Value,
    pub selection: &'a Value,
}

pub struct AlternativeChoice<'a> {
    pub context: RecordingContext<'a>,
    pub identity: &'a Value,
}

pub struct AlternativePage {
    pub choices: Vec<Alternative>,
    pub index: u16,
    pub count: u16,
    pub total: u16,
    pub previous: Option<u16>,
    pub next: Option<u16>,
    pub token: SelectionToken,
}

pub struct Alternative {
    pub identity: Value,
    pub label: String,
    pub selected: bool,
}

#[derive(Clone)]
pub struct SelectionToken {
    recording: String,
    pub second: u64,
    selected: Value,
}

impl AlternativePage {
    pub fn read(context: RecordingContext<'_>, index: u16) -> Result<Self, String> {
        let alternatives = context.selection["alternatives"]
            .as_array()
            .ok_or("Observation alternatives unavailable")?;
        if alternatives.len() > ALTERNATIVES_MAX {
            return Err("Observation alternatives exceed 4096".into());
        }

        let token = SelectionToken::read(context)?;
        let count = u16::try_from(alternatives.len().div_ceil(PAGE_SIZE))
            .map_err(|error| error.to_string())?;
        let index = index.min(count.saturating_sub(1));
        let first = usize::from(index) * PAGE_SIZE;
        let mut choices = Vec::with_capacity(PAGE_SIZE);
        for identity in alternatives.iter().skip(first).take(PAGE_SIZE) {
            choices.push(Alternative {
                identity: identity.clone(),
                label: alternative_label(identity)?,
                selected: *identity == context.selection["selected"],
            });
        }

        assert!(choices.len() <= PAGE_SIZE);
        assert!(count <= 64);

        Ok(Self {
            choices,
            index,
            count,
            total: u16::try_from(alternatives.len()).map_err(|error| error.to_string())?,
            previous: index.checked_sub(1),
            next: (index + 1 < count).then_some(index + 1),
            token,
        })
    }
}

impl SelectionToken {
    fn read(context: RecordingContext<'_>) -> Result<Self, String> {
        let recording = context.catalog["path"]
            .as_str()
            .ok_or("Recording path unavailable")?;
        if recording.len() > 4096 {
            return Err("Recording path exceeds 4096 bytes".into());
        }

        Ok(Self {
            recording: recording.into(),
            second: context.selection["second"]
                .as_u64()
                .ok_or("Selected second unavailable")?,
            selected: context.selection["selected"].clone(),
        })
    }

    pub fn matches(&self, context: RecordingContext<'_>) -> bool {
        if context.catalog["path"].as_str() != Some(self.recording.as_str()) {
            return false;
        }

        if context.selection["second"].as_u64() != Some(self.second) {
            return false;
        }

        context.selection["selected"] == self.selected
    }

    pub fn choose(&self, choice: &AlternativeChoice<'_>) -> Option<Value> {
        if !self.matches(choice.context) {
            return None;
        }

        let alternatives = choice.context.selection["alternatives"].as_array()?;
        if alternatives.len() > ALTERNATIVES_MAX {
            return None;
        }

        alternatives
            .iter()
            .find(|candidate| *candidate == choice.identity)
            .cloned()
    }
}

fn alternative_label(identity: &Value) -> Result<String, String> {
    let run = identity["run"]
        .as_str()
        .ok_or("Observation run unavailable")?;
    let boot = identity["boot"]
        .as_str()
        .ok_or("Observation boot unavailable")?;
    if run.len() != 32 {
        return Err("Invalid observation run identity".into());
    }

    if boot.len() != 32 {
        return Err("Invalid observation boot identity".into());
    }

    if !run.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid observation run identity".into());
    }

    if !boot.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid observation boot identity".into());
    }

    let publication = identity["publication"]
        .as_u64()
        .ok_or("Publication unavailable")?;

    Ok(format!(
        "Run {run} · boot {boot} · publication {publication}"
    ))
}

pub fn source(context: RecordingContext<'_>) -> Vec<String> {
    let path = context.catalog["path"].as_str().map_or_else(
        || "Recording path unavailable".into(),
        |path| format!("Recording: {}", bounded(path, 4096)),
    );
    let extent = match (
        context.catalog["first_second"].as_u64(),
        context.catalog["last_second"].as_u64(),
    ) {
        (Some(first), Some(last)) => format!("Known extent: {first}…{last} Unix seconds"),
        _ => "Known extent unavailable: no recorded wall-time bounds".into(),
    };
    let windows = context.catalog["windows"].as_u64().map_or_else(
        || "sealed-window count unavailable".into(),
        |count| format!("{count} sealed windows"),
    );
    let zone = context.selection["second"].as_u64().map_or_else(
        || "Viewer timezone unavailable: no selected second".into(),
        |second| format!("Viewer timezone at selected second: {}", timezone(second)),
    );

    vec![path, format!("{extent} · {windows}"), zone]
}

pub fn bounded(text: &str, maximum: usize) -> String {
    assert!(maximum <= 4096);
    let mut characters = text.chars();
    let mut output: String = characters.by_ref().take(maximum).collect();
    if characters.next().is_some() {
        output.push('…');
    }

    output
}

fn timezone(second: u64) -> String {
    let Ok(timestamp) = libc::time_t::try_from(second) else {
        return "Unavailable".into();
    };
    let mut parts = std::mem::MaybeUninit::<libc::tm>::uninit();
    // localtime_r initializes the supplied output only when it succeeds.
    if unsafe { libc::localtime_r(&raw const timestamp, parts.as_mut_ptr()) }.is_null() {
        return "Unavailable".into();
    }

    let mut buffer = [0_i8; 128];
    // The successful conversion and bounded buffer satisfy strftime's pointer requirements.
    let count = unsafe {
        libc::strftime(
            buffer.as_mut_ptr(),
            buffer.len(),
            c"%Z %z".as_ptr(),
            parts.as_ptr(),
        )
    };
    if count == 0 {
        return "Unavailable".into();
    }

    let bytes: Vec<_> = buffer[..count]
        .iter()
        .map(|byte| byte.to_ne_bytes()[0])
        .collect();
    match String::from_utf8(bytes) {
        Ok(zone) => zone,
        Err(_) => "Unavailable".into(),
    }
}

#[cfg(test)]
#[path = "live_recording_context_tests.rs"]
mod tests;
