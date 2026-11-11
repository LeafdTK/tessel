use crate::{Style, Svg, finish, hex};

// far beyond anything typeset by hand; the layout recurses once per level.
const MAX_NESTING: usize = 200;
const MAX_TEX_BYTES: usize = 16 * 1024;

pub(crate) fn render(tex: &str, display: bool, style: &Style) -> Result<Svg, String> {
    let tex = tex.trim();
    if tex.is_empty() {
        return Err("the formula is empty".into());
    }
    if tex.len() > MAX_TEX_BYTES {
        return Err(format!("the formula is longer than {MAX_TEX_BYTES} bytes"));
    }
    if let Some(column) = too_deep(tex) {
        return Err(format!(
            "column {column}: nested more than {MAX_NESTING} levels deep"
        ));
    }

    let options = mathra::Options {
        display,
        // without this, unknown commands and unbalanced braces are patched up
        // and drawn anyway, and the reader never learns the formula was wrong.
        strict: true,
        ..Default::default()
    };
    let output = mathra::convert(tex, &options).map_err(|error| error.to_string())?;
    finish(&restyle(&output.svg, style)?)
}

/// the 1-based column where the nesting of groups first exceeds the limit.
fn too_deep(tex: &str) -> Option<usize> {
    let bytes = tex.as_bytes();
    let (mut depth, mut at) = (0usize, 0usize);
    while at < bytes.len() {
        let rest = &bytes[at..];
        if command(rest, b"\\begin") || command(rest, b"\\left") {
            depth += 1;
        } else if command(rest, b"\\end") || command(rest, b"\\right") {
            depth = depth.saturating_sub(1);
        } else if rest[0] == b'\\' {
            // an escaped brace is a character, not a group.
            at += 2;
            continue;
        } else if rest[0] == b'{' {
            depth += 1;
        } else if rest[0] == b'}' {
            depth = depth.saturating_sub(1);
        }
        if depth > MAX_NESTING {
            return Some(String::from_utf8_lossy(&bytes[..at]).chars().count() + 1);
        }
        at += 1;
    }
    None
}

/// whether `rest` starts with exactly this command, not a longer one such as
/// `\leftarrow`.
fn command(rest: &[u8], name: &[u8]) -> bool {
    rest.starts_with(name) && !rest.get(name.len()).is_some_and(u8::is_ascii_alphabetic)
}

/// gives the drawing a size in px and the surrounding text colour.
///
/// the renderer sizes its output in em and leaves the colour to the page
/// (`currentColor`), neither of which means anything to a standalone image.
fn restyle(svg: &str, style: &Style) -> Result<String, String> {
    let malformed = || "the math renderer produced a drawing in an unexpected shape".to_owned();
    let root_end = svg.find('>').ok_or_else(malformed)?;
    let (root, body) = svg.split_at(root_end);
    let view_box = attribute(root, "viewBox").ok_or_else(malformed)?;
    let em = |name| {
        attribute(root, name)
            .and_then(|value| value.strip_suffix("em"))
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(malformed)
    };
    let (width, height) = (em("width")? * style.font_px, em("height")? * style.font_px);

    // speech, braille and the source tree for assistive tools in a browser;
    // dead weight in an image, and in a namespace the rasteriser rejects.
    let body = without(body, "<metadata>", "</metadata>");
    // an invisible copy of the formula as text, meant for selection in a
    // browser. here it would only be shaped and turned into empty paths.
    let body = without(&body, "<text ", "</text>");
    let body = body
        .replace("<style>svg:root{color:#111}</style>", "")
        .replace("currentColor", &hex(style.text));

    Ok(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view_box}" width="{width}" height="{height}"{body}"#
    ))
}

/// `text` with the last `open`..`close` element cut out.
fn without(text: &str, open: &str, close: &str) -> String {
    match (text.rfind(open), text.rfind(close)) {
        (Some(start), Some(end)) if start < end => {
            [&text[..start], &text[end + close.len()..]].concat()
        }
        _ => text.to_owned(),
    }
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let start = tag.find(&format!(" {name}=\""))? + name.len() + 3;
    let end = tag[start..].find('"')?;
    Some(&tag[start..start + end])
}
