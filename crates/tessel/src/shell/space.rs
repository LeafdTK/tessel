//! the space backdrop: a photograph from NASA's image library behind the panes.
//!
//! NASA's own images aren't under copyright, and its library's api needs no
//! key. a photo is fetched once (curl, off the ui thread), kept beside
//! ui.json in `backdrops/` with a `current` file naming it, so a restart shows
//! the same one without the network. it's decoded and scaled to the screen
//! off the ui thread too, handed to the gpu once, and painted every frame as
//! one sprite, under a scrim so whatever floats over it stays readable.

use super::*;
use serde_json::Value;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// what the library is searched for: the kind of picture that sits well behind work.
const QUERIES: [&str; 7] = ["nebula", "galaxy", "hubble nebula", "webb telescope", "spiral galaxy", "star cluster", "planetary nebula"];

/// most of the library is launches, labs and press events. a photo is kept
/// only if it comes from the observatories' science centres and is of the
/// sky: it names a telescope or the kind of thing telescopes look at.
fn is_sky(center: &str, title: &str, id: &str, keywords: &[&str]) -> bool {
    let from_science = ["JPL", "GSFC", "STScI"].iter().any(|c| center.starts_with(c));
    let words = format!("{title} {}", keywords.join(" ")).to_lowercase();
    let of_sky = ["hubble", "webb", "spitzer", "chandra", "nebula", "galaxy", "cluster", "supernova"].iter().any(|w| words.contains(w));
    let event = ["hangout", "briefing", "conference", "media", "teleconference"].iter().any(|w| words.contains(w));
    from_science && of_sky && !event && !title.is_empty() && title != id
}

/// the widest a photo is kept at: bigger than any window, smaller than the originals.
const MAX_W: u32 = 3200;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Photo {
    pub id: String,
    pub title: String,
    pub path: PathBuf,
}

#[derive(Default)]
pub(crate) struct State {
    pub photo: Option<Photo>,
    image: Option<(Arc<RenderImage>, u32, u32)>,
    /// decoded and waiting for a frame to hand it to the gpu.
    ready: Option<(Photo, Vec<u8>, u32, u32)>,
    pub loading: bool,
    pub error: Option<String>,
    /// the next fetch skips what's shown now.
    pub want_next: bool,
}

pub(crate) type Shared = Rc<RefCell<State>>;

fn get(url: &str) -> Result<Vec<u8>, String> {
    let out = std::process::Command::new("curl")
        .args(["-sfL", "--max-time", "30", "-A", "tessel", url])
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("NASA's library didn't answer ({})", out.status));
    }
    Ok(out.stdout)
}

/// a number that's different each call: which query, which result.
fn roll() -> usize {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    ((t ^ (t >> 17)) as usize).wrapping_mul(2_654_435_761)
}

/// the photo named in `current`, if it's still on disk.
fn current(dir: &Path) -> Option<Photo> {
    let id = std::fs::read_to_string(dir.join("current")).ok()?.trim().to_owned();
    let path = dir.join(format!("{id}.jpg"));
    let title = std::fs::read_to_string(dir.join(format!("{id}.txt"))).unwrap_or_default().trim().to_owned();
    path.exists().then_some(Photo { id, title, path })
}

/// pick a photo from the library, fetch it, and make it the current one.
fn fetch(dir: &Path, avoid: Option<&str>) -> Result<Photo, String> {
    let r = roll();
    let query = QUERIES[r % QUERIES.len()];
    let url = format!("https://images-api.nasa.gov/search?media_type=image&q={}", query.replace(' ', "%20"));
    let found: Value = serde_json::from_slice(&get(&url)?).map_err(|e| format!("NASA's library answered oddly: {e}"))?;
    let items: Vec<(String, String)> = found["collection"]["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|it| {
                    let d = it["data"].get(0)?;
                    let id = d["nasa_id"].as_str()?.to_owned();
                    let title = d["title"].as_str().unwrap_or_default().to_owned();
                    let keywords: Vec<&str> = d["keywords"].as_array().map(|k| k.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                    is_sky(d["center"].as_str().unwrap_or_default(), &title, &id, &keywords).then_some((id, title))
                })
                .filter(|(id, _)| Some(id.as_str()) != avoid)
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        return Err(format!("nothing came back for \"{query}\""));
    }
    let (id, title) = items[(r / QUERIES.len()) % items.len()].clone();
    // the asset manifest lists the sizes; a large one is plenty.
    let manifest: Value = serde_json::from_slice(&get(&format!("https://images-api.nasa.gov/asset/{}", id.replace(' ', "%20")))?).map_err(|e| e.to_string())?;
    let hrefs: Vec<String> = manifest["collection"]["items"].as_array().map(|a| a.iter().filter_map(|i| i["href"].as_str().map(str::to_owned)).collect()).unwrap_or_default();
    let href = ["~large.jpg", "~medium.jpg", "~orig.jpg"]
        .iter()
        .find_map(|size| hrefs.iter().find(|h| h.ends_with(size)))
        .ok_or("that photo has no jpeg")?
        .replacen("http://", "https://", 1);
    let bytes = get(&href.replace(' ', "%20"))?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // a file name of its own, whatever characters the id has.
    let safe: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let path = dir.join(format!("{safe}.jpg"));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    let _ = std::fs::write(dir.join(format!("{safe}.txt")), &title);
    let _ = std::fs::write(dir.join("current"), &safe);
    Ok(Photo { id: safe, title, path })
}

/// a photo's pixels, bgra, no wider than `MAX_W`.
fn decode(path: &Path) -> Result<(Vec<u8>, u32, u32), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("couldn't read the photo: {e}"))?;
    let img = if img.width() > MAX_W { img.resize(MAX_W, u32::MAX, image::imageops::FilterType::Triangle) } else { img };
    let mut rgba = img.to_rgba8();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let (w, h) = rgba.dimensions();
    Ok((rgba.into_raw(), w, h))
}

/// the rectangle a `w`×`h` picture fills `bounds` with, cropping what's over.
fn cover(bounds: Bounds<Pixels>, w: u32, h: u32) -> Bounds<Pixels> {
    let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let s = (bw / w as f32).max(bh / h as f32);
    let (iw, ih) = (w as f32 * s, h as f32 * s);
    Bounds { origin: point(bounds.origin.x + px((bw - iw) / 2.0), bounds.origin.y + px((bh - ih) / 2.0)), size: size(px(iw), px(ih)) }
}

impl Shell {
    fn backdrops_dir(&self) -> Option<PathBuf> {
        self.ui_path.as_ref().and_then(|p| p.parent()).map(|d| d.join("backdrops"))
    }

    /// another photo, in place of the one shown.
    pub(crate) fn next_space_photo(&self, cx: &mut Context<Self>) {
        {
            let mut st = self.space.borrow_mut();
            st.want_next = true;
            st.error = None;
        }
        self.load_space(cx);
    }

    /// make sure there's a photo, or one on its way: the one from last time if
    /// it's on disk, otherwise one from the library.
    fn load_space(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.backdrops_dir() else { return };
        // tests never reach for the network.
        if cfg!(test) {
            return;
        }
        let state = self.space.clone();
        let (next, avoid) = {
            let mut st = state.borrow_mut();
            if st.loading || (st.photo.is_some() && !st.want_next) || (st.error.is_some() && !st.want_next) {
                return;
            }
            st.loading = true;
            let next = std::mem::take(&mut st.want_next);
            (next, st.photo.as_ref().map(|p| p.id.clone()))
        };
        let work = cx.background_executor().spawn(async move {
            let photo = match (next, current(&dir)) {
                (false, Some(p)) => p,
                _ => fetch(&dir, avoid.as_deref())?,
            };
            let (bytes, w, h) = decode(&photo.path)?;
            Ok::<_, String>((photo, bytes, w, h))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            {
                let mut st = state.borrow_mut();
                st.loading = false;
                match result {
                    Ok((photo, bytes, w, h)) => st.ready = Some((photo, bytes, w, h)),
                    Err(e) => st.error = Some(e),
                }
            }
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    /// the photo, filling its parent, under a scrim, with its credit.
    pub(crate) fn space_el(&self, cx: &mut Context<Self>) -> AnyElement {
        self.load_space(cx);
        let p = self.palette.get();
        let state = self.space.clone();
        let credit = state.borrow().photo.as_ref().map(|ph| if ph.title.is_empty() { "NASA".to_owned() } else { format!("NASA · {}", ph.title) });
        let picture = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let mut st = state.borrow_mut();
                if let Some((photo, bytes, w, h)) = st.ready.take()
                    && let Some(buf) = image::RgbaImage::from_raw(w, h, bytes)
                {
                    let fresh = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
                    if let Some((old, ..)) = st.image.replace((fresh, w, h)) {
                        let _ = window.drop_image(old);
                    }
                    st.photo = Some(photo);
                }
                if let Some((img, w, h)) = st.image.clone() {
                    let _ = window.paint_image(bounds, cover(bounds, w, h), Corners::all(px(theme::R_PANEL)), img, 0, false);
                }
            },
        )
        .absolute()
        .inset_0();
        div()
            .absolute()
            .inset_0()
            .child(picture)
            // the scrim: the photo is a mood, never louder than what's over it. always
            // a darkening: space is black, and a white veil turns it to grey haze. the
            // cards over it are solid, so in light mode a lighter touch is enough.
            .child(div().absolute().inset_0().rounded(px(theme::R_PANEL)).bg(gpui_kit::black().opacity(if p.dark { 0.55 } else { 0.25 })))
            .children(credit.map(|c| {
                div()
                    .absolute()
                    .right(px(12.))
                    .bottom(px(10.))
                    .max_w(px(360.))
                    .truncate()
                    .px(px(8.))
                    .py(px(3.))
                    .rounded_full()
                    .bg(gpui_kit::black().opacity(0.35))
                    .set(Type::Subheadline)
                    .text_color(gpui_kit::white().opacity(0.8))
                    .child(c)
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{cover, is_sky};

    #[test]
    fn only_pictures_of_the_sky_are_kept() {
        assert!(is_sky("JPL", "Ant Nebula", "PIA03000", &["Hubble Space Telescope"]));
        assert!(is_sky("STScI (Webb)", "Pillars of Creation", "webb-1", &["James Webb Space Telescope"]));
        assert!(!is_sky("KSC", "KSC-2009-1046", "KSC-2009-1046", &[]), "a lab at Kennedy, named only by its number");
        assert!(!is_sky("GSFC", "NASA Google+ Hangout: Earthrise", "x", &["galaxy"]), "a press event");
        assert!(!is_sky("JSC", "View of Skylab in Earth orbit", "s1", &["EARTH OBSERVATIONS"]));
    }
    use gpui_kit::{Bounds, point, px, size};

    #[test]
    fn a_photo_covers_its_room_centred_and_cropped() {
        let room = Bounds { origin: point(px(0.), px(0.)), size: size(px(1000.), px(500.)) };
        // a square photo: as wide as the room, taller, cut top and bottom evenly.
        let r = cover(room, 800, 800);
        assert_eq!(r.size.width, px(1000.));
        assert_eq!(r.size.height, px(1000.));
        assert_eq!(r.origin.y, px(-250.));
    }
}
