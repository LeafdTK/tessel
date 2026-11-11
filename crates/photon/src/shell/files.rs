//! more than text in the files module: what changed, marked in the gutter;
//! images, pdfs and media shown rather than refused; markdown read as a
//! document, with the source a click away. the bytes come from the daemon, so
//! a remote daemon shows the same.

use super::*;
use gpui_kit::component::input::GutterDecoration;
use gpui_kit::{Image, ImageFormat, ObjectFit, img};
use photon_proto::{Request, Response};
use std::sync::Arc;

/// a run of lines a diff changed, in the file as it is now (0-based, end exclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Added(u32, u32),
    Modified(u32, u32),
    /// lines were removed just before this one.
    Deleted(u32),
}

/// the marks for a unified diff of one file.
pub(crate) fn line_marks(unified: &str) -> Vec<Mark> {
    let mut marks = Vec::new();
    let mut new_line: u32 = 0;
    // the run being read: lines removed, then lines added.
    let (mut removed, mut added, mut start) = (0u32, 0u32, 0u32);
    let flush = |marks: &mut Vec<Mark>, removed: &mut u32, added: &mut u32, start: u32| {
        match (*removed, *added) {
            (0, 0) => {}
            (0, a) => marks.push(Mark::Added(start, start + a)),
            (_, 0) => marks.push(Mark::Deleted(start)),
            (_, a) => marks.push(Mark::Modified(start, start + a)),
        }
        (*removed, *added) = (0, 0);
    };
    for line in unified.lines() {
        if let Some(rest) = line.strip_prefix("@@ ") {
            flush(&mut marks, &mut removed, &mut added, start);
            // @@ -a,b +c,d @@: new lines start at c (1-based).
            let c = rest.split_whitespace().nth(1).and_then(|n| n.trim_start_matches('+').split(',').next()?.parse::<u32>().ok()).unwrap_or(1);
            new_line = c.saturating_sub(1);
            continue;
        }
        if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff ") || line.starts_with("index ") {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => {
                if removed == 0 && added == 0 {
                    start = new_line;
                }
                added += 1;
                new_line += 1;
            }
            Some(b'-') => {
                if added > 0 {
                    flush(&mut marks, &mut removed, &mut added, start);
                }
                if removed == 0 {
                    start = new_line;
                }
                removed += 1;
            }
            Some(b'\\') => {}
            _ => {
                flush(&mut marks, &mut removed, &mut added, start);
                new_line += 1;
            }
        }
    }
    flush(&mut marks, &mut removed, &mut added, start);
    marks
}

/// byte offsets where each line of `text` starts.
fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect()
}

fn format_of(mime: &str) -> Option<ImageFormat> {
    Some(match mime {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        "image/svg+xml" => ImageFormat::Svg,
        "image/bmp" => ImageFormat::Bmp,
        "image/tiff" => ImageFormat::Tiff,
        "image/x-icon" => ImageFormat::Ico,
        _ => return None,
    })
}

pub(crate) fn is_markdown(path: &str) -> bool {
    matches!(path.rsplit('.').next().map(str::to_ascii_lowercase).as_deref(), Some("md" | "markdown" | "mdx"))
}

/// what kind of thing to show instead of an editor, by name: these are looked
/// at even when the bytes would pass as text (an svg is text).
fn shown_not_edited(path: &str) -> bool {
    matches!(
        path.rsplit('.').next().map(str::to_ascii_lowercase).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "pdf" | "mov" | "mp4" | "m4v" | "webm" | "mp3" | "m4a" | "wav")
    )
}

impl Shell {
    /// what to fetch for a file that just opened: a preview, or its change marks.
    pub(crate) fn file_opened(&mut self, binary: bool) {
        let (Some(session), Some(file)) = (self.ws_session, self.ws.file.as_ref()) else { return };
        let path = file.path.clone();
        if binary || shown_not_edited(&path) {
            self.cmd(Cmd::Ask { tag: "file.preview", req: Request::FsPreview { session, path } });
        } else {
            self.cmd(Cmd::Ask { tag: "file.marks", req: Request::GitDiff { session, path, at: None, index: None } });
        }
    }

    pub(crate) fn file_answer(&mut self, tag: &'static str, result: Result<Response, String>, cx: &mut Context<Self>) {
        let p = self.palette.get();
        let Some(file) = self.ws.file.as_mut() else { return };
        match (tag, result) {
            ("file.preview", Ok(Response::Preview { mime, data, note })) => {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD.decode(data).unwrap_or_default();
                match format_of(&mime) {
                    Some(format) => {
                        file.preview = Some((Arc::new(Image::from_bytes(format, bytes)), note));
                        file.note = None;
                    }
                    None => file.note = Some("There's nothing to show for this file.".into()),
                }
            }
            ("file.preview", Err(e)) => file.note = Some(format!("{e}.")),
            ("file.marks", Ok(Response::Diff { diff })) => {
                let Some(editor) = file.editor.clone() else { return };
                let marks = line_marks(&diff.unified);
                let text = editor.read(cx).value().to_string();
                let starts = line_starts(&text);
                let at = |line: u32| starts.get(line as usize).copied().unwrap_or(text.len());
                let decorations: Vec<GutterDecoration> = marks
                    .iter()
                    .map(|m| match *m {
                        Mark::Added(a, b) => GutterDecoration::new(at(a)..at(b).saturating_sub(1).max(at(a))).with_color(p.finished),
                        Mark::Modified(a, b) => GutterDecoration::new(at(a)..at(b).saturating_sub(1).max(at(a))).with_color(p.accent),
                        Mark::Deleted(a) => GutterDecoration::new(at(a)..at(a)).with_color(p.failed),
                    })
                    .collect();
                match &file.marks {
                    Some(c) => c.set(decorations, cx),
                    None => file.marks = Some(editor.update(cx, |s, cx| s.create_gutter_decorations_collection(decorations, cx))),
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// a file shown rather than edited: its preview, fitted.
    pub(crate) fn preview_el(&self, image: &Arc<Image>, note: Option<&str>) -> AnyElement {
        let p = self.palette.get();
        v_flex()
            .size_full()
            .p(theme::z(16.))
            .gap(theme::z(8.))
            .items_center()
            .justify_center()
            .bg(p.fill.opacity(0.4))
            .child(img(image.clone()).max_w_full().flex_1().min_h_0().object_fit(ObjectFit::Contain))
            .children(note.map(|n| div().set(Type::Subheadline).text_color(p.tertiary).child(n.to_owned())))
            .into_any_element()
    }

    /// a to-do ticked in the document: the source's `[ ]` becomes `[x]` (or
    /// back), and the file is saved, as if it had been typed.
    pub(crate) fn toggle_task(&mut self, marker: std::ops::Range<usize>, checked: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.ws.file.as_ref().and_then(|f| f.editor.clone()) else { return };
        let mut text = editor.read(cx).value().to_string();
        let Some(was) = text.get(marker.clone()) else { return };
        // the marker as written: "[ ]", "[x]" or "[X]", or just its middle.
        let mark = if checked { "x" } else { " " };
        let fixed = match was.len() {
            3 if was.starts_with('[') && was.ends_with(']') => format!("[{mark}]"),
            1 => mark.to_owned(),
            _ => return,
        };
        text.replace_range(marker, &fixed);
        editor.update(cx, |e, cx| e.set_value(text, window, cx));
        if let Some(f) = self.ws.file.as_mut() {
            f.dirty = true;
        }
        self.save_file(cx);
        cx.notify();
    }

    /// markdown, as a document. its to-dos tick.
    pub(crate) fn document_el(&self, text: String, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let me = cx.entity().downgrade();
        div()
            .id("file-document")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .max_w(px(theme::READING_W))
                    .mx_auto()
                    .px(theme::z(24.))
                    .py(theme::z(20.))
                    .set(Type::Prose)
                    .child(TextView::markdown("file-document-text", text).selectable(true).style(theme::prose(&p)).on_task_toggle(move |ev, window, cx| {
                        let (marker, checked) = (ev.marker_range(), ev.checked());
                        let _ = me.update(cx, |this, cx| this.toggle_task(marker, checked, window, cx));
                    })),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Mark, line_marks};

    #[test]
    fn a_diff_becomes_marks_on_the_lines_as_they_are_now() {
        let unified = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,5 +1,6 @@\n one\n-two\n+TWO\n three\n+new\n four\n-five\n";
        assert_eq!(line_marks(unified), [Mark::Modified(1, 2), Mark::Added(3, 4), Mark::Deleted(5)]);
    }

    #[test]
    fn a_new_file_is_all_added_and_hunks_place_themselves() {
        assert_eq!(line_marks("@@ -0,0 +1,3 @@\n+a\n+b\n+c\n"), [Mark::Added(0, 3)]);
        assert_eq!(line_marks("@@ -10,2 +10,3 @@\n ten\n+x\n eleven\n"), [Mark::Added(10, 11)]);
    }
}
