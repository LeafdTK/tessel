//! a pdf's pages as pictures, drawn here by the mac's own pdf renderer
//! (CoreGraphics), from its bytes: the same wherever the file is (a dev box
//! has no previewer of its own).

use std::sync::Arc;

/// at most this many pages are drawn.
pub const MAX_PAGES: usize = 60;

/// the pages of the pdf in `bytes`, each `width` pixels wide, as images ready
/// for the gpu (bgra). none if it isn't a pdf the mac can read.
pub fn pages(bytes: &[u8], width: u32) -> Option<Vec<Arc<gpui_kit::RenderImage>>> {
    #[cfg(target_os = "macos")]
    {
        mac::pages(bytes, width)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (bytes, width);
        None
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::MAX_PAGES;
    use std::ffi::c_void;
    use std::sync::Arc;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Affine {
        a: f64,
        b: f64,
        c: f64,
        d: f64,
        tx: f64,
        ty: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDataProviderCreateWithData(info: *mut c_void, data: *const c_void, size: usize, release: *const c_void) -> *mut c_void;
        fn CGDataProviderRelease(p: *mut c_void);
        fn CGPDFDocumentCreateWithProvider(p: *mut c_void) -> *mut c_void;
        fn CGPDFDocumentRelease(d: *mut c_void);
        fn CGPDFDocumentGetNumberOfPages(d: *mut c_void) -> usize;
        fn CGPDFDocumentGetPage(d: *mut c_void, n: usize) -> *mut c_void;
        fn CGPDFPageGetBoxRect(p: *mut c_void, r#box: i32) -> Rect;
        fn CGPDFPageGetRotationAngle(p: *mut c_void) -> i32;
        fn CGPDFPageGetDrawingTransform(p: *mut c_void, r#box: i32, rect: Rect, rotate: i32, preserve: bool) -> Affine;
        fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
        fn CGColorSpaceRelease(s: *mut c_void);
        fn CGBitmapContextCreate(data: *mut c_void, w: usize, h: usize, bpc: usize, bpr: usize, space: *mut c_void, info: u32) -> *mut c_void;
        fn CGContextRelease(c: *mut c_void);
        fn CGContextSetRGBFillColor(c: *mut c_void, r: f64, g: f64, b: f64, a: f64);
        fn CGContextFillRect(c: *mut c_void, r: Rect);
        fn CGContextConcatCTM(c: *mut c_void, t: Affine);
        fn CGContextDrawPDFPage(c: *mut c_void, p: *mut c_void);
    }

    /// the crop box: what a reader shows of the page.
    const CROP_BOX: i32 = 1;
    /// bgra in memory, premultiplied: kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little.
    const BGRA: u32 = 2 | (2 << 12);

    pub(super) fn pages(bytes: &[u8], width: u32) -> Option<Vec<Arc<gpui_kit::RenderImage>>> {
        unsafe {
            let provider = CGDataProviderCreateWithData(std::ptr::null_mut(), bytes.as_ptr().cast(), bytes.len(), std::ptr::null());
            if provider.is_null() {
                return None;
            }
            let doc = CGPDFDocumentCreateWithProvider(provider);
            CGDataProviderRelease(provider);
            if doc.is_null() {
                return None;
            }
            let space = CGColorSpaceCreateDeviceRGB();
            let n = CGPDFDocumentGetNumberOfPages(doc).min(MAX_PAGES);
            let mut out = Vec::with_capacity(n);
            for i in 1..=n {
                let page = CGPDFDocumentGetPage(doc, i);
                if page.is_null() {
                    continue;
                }
                let r = CGPDFPageGetBoxRect(page, CROP_BOX);
                let turned = CGPDFPageGetRotationAngle(page).rem_euclid(180) == 90;
                let (pw, ph) = if turned { (r.h, r.w) } else { (r.w, r.h) };
                if pw <= 0.0 || ph <= 0.0 {
                    continue;
                }
                let w = width as usize;
                let h = ((ph / pw) * width as f64).round().max(1.0) as usize;
                let mut buf = vec![0u8; w * h * 4];
                let ctx = CGBitmapContextCreate(buf.as_mut_ptr().cast(), w, h, 8, w * 4, space, BGRA);
                if ctx.is_null() {
                    continue;
                }
                let target = Rect { x: 0.0, y: 0.0, w: w as f64, h: h as f64 };
                CGContextSetRGBFillColor(ctx, 1.0, 1.0, 1.0, 1.0);
                CGContextFillRect(ctx, target);
                // scaled to fill the width: the drawing transform only ever shrinks, so scale first.
                let s = w as f64 / pw;
                CGContextConcatCTM(ctx, Affine { a: s, b: 0.0, c: 0.0, d: s, tx: 0.0, ty: 0.0 });
                let fit = CGPDFPageGetDrawingTransform(page, CROP_BOX, Rect { x: 0.0, y: 0.0, w: pw, h: ph }, 0, true);
                CGContextConcatCTM(ctx, fit);
                CGContextDrawPDFPage(ctx, page);
                CGContextRelease(ctx);
                if let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, buf) {
                    out.push(Arc::new(gpui_kit::RenderImage::new(vec![image::Frame::new(img)])));
                }
            }
            CGColorSpaceRelease(space);
            CGPDFDocumentRelease(doc);
            (!out.is_empty()).then_some(out)
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    /// a one-page pdf, 200 × 100 points, a black square in its lower left.
    fn tiny() -> Vec<u8> {
        let stream = "0 0 0 rg 10 10 50 50 re f";
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for o in offs {
            out.extend(format!("{o:010} 00000 n \n").bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
        out
    }

    #[test]
    fn a_pdf_page_is_drawn_to_the_width_asked_with_its_ink() {
        let pages = super::pages(&tiny(), 400).expect("it reads");
        assert_eq!(pages.len(), 1);
        let size = pages[0].size(0);
        assert_eq!((size.width.0, size.height.0), (400, 200), "the page's shape, scaled to the width");
        let bytes = pages[0].as_bytes(0).expect("one frame");
        // bgra: the lower left (the square) is dark, the upper right white.
        let at = |x: usize, y: usize| bytes[(y * 400 + x) * 4];
        assert!(at(60, 160) < 40, "the square is drawn");
        assert!(at(350, 20) > 240, "the rest is paper");
        assert!(super::pages(b"not a pdf", 400).is_none());
    }
}
