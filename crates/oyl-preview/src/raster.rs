//! What to rasterize, at which resolution, and whether a finished job still
//! matters.
//!
//! A page is rasterized at `zoom × scale factor` pixels per point, capped so
//! that one bitmap stays a reasonable GPU texture. Jobs run in the
//! background, a few at a time; a job is only ever started for a page that
//! is wanted *now*, so superseded work never starts, and a result that a
//! newer document or zoom made pointless is dropped when it arrives.

use std::ops::Range;

use gpui_kit::Size;

/// Longest side of one page bitmap, in pixels.
pub const MAX_BITMAP_SIDE: f32 = 8192.;
/// Largest area of one page bitmap (32 megapixels, 128 MB).
pub const MAX_BITMAP_PIXELS: f32 = 32. * 1024. * 1024.;
/// Lowest resolution ever requested.
pub const MIN_PIXELS_PER_PT: f32 = 0.1;

/// The resolution, in pixels per point, to rasterize a page of `page_pt`
/// for `zoom` on a display with `scale_factor`.
pub fn raster_scale(zoom: f32, scale_factor: f32, page_pt: Size<f32>) -> f32 {
    let desired = zoom * scale_factor;
    if !desired.is_finite() || desired <= MIN_PIXELS_PER_PT {
        return MIN_PIXELS_PER_PT;
    }
    let longest = page_pt.width.max(page_pt.height).max(1.);
    let area = (page_pt.width * page_pt.height).max(1.);
    desired
        .min(MAX_BITMAP_SIDE / longest)
        .min((MAX_BITMAP_PIXELS / area).sqrt())
        .max(MIN_PIXELS_PER_PT)
}

/// Whether two resolutions are the same for display purposes (within 1%).
pub fn scales_match(a: f32, b: f32) -> bool {
    (a - b).abs() <= a.max(b) * 0.01
}

/// How far a resolution is from a target, as a ratio (0 = exact).
fn scale_error(scale: f32, target: f32) -> f32 {
    if scale > 0. && target > 0. {
        (scale / target).ln().abs()
    } else {
        f32::INFINITY
    }
}

/// One rasterization: which page, of which content, at which resolution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterKey {
    /// Page index.
    pub page: usize,
    /// The page's fingerprint when the job was planned.
    pub fingerprint: u64,
    /// Resolution in pixels per point.
    pub pixels_per_pt: f32,
}

impl RasterKey {
    /// Whether two keys describe the same work (resolutions within 1%).
    pub fn same_job(&self, other: &RasterKey) -> bool {
        self.page == other.page
            && self.fingerprint == other.fingerprint
            && scales_match(self.pixels_per_pt, other.pixels_per_pt)
    }
}

/// What the cache holds for a page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeldBitmap {
    /// Fingerprint of the content it shows.
    pub fingerprint: u64,
    /// Its resolution.
    pub pixels_per_pt: f32,
}

/// Whether a page holding `held` needs the rasterization `want`.
///
/// A page that shows nothing, or older content, always does. A page that
/// shows its current content at another resolution does only once the zoom
/// has `settled`: while a zoom is moving, its bitmap is shown scaled.
pub fn needs_raster(held: Option<HeldBitmap>, want: &RasterKey, settled: bool) -> bool {
    match held {
        None => true,
        Some(held) if held.fingerprint != want.fingerprint => true,
        Some(held) => settled && !scales_match(held.pixels_per_pt, want.pixels_per_pt),
    }
}

/// Whether a finished job's bitmap should replace what the page holds.
///
/// - Content superseded by a newer compile (`current_fingerprint` differs,
///   or the page is gone): dropped.
/// - At the wanted resolution: taken.
/// - At an outdated resolution (the zoom moved on while it ran): taken only
///   when it is still better than what is shown — the page shows nothing or
///   older content, or a resolution even further from the target.
pub fn accept_result(
    job: &RasterKey,
    current_fingerprint: Option<u64>,
    held: Option<HeldBitmap>,
    target_pixels_per_pt: f32,
) -> bool {
    if current_fingerprint != Some(job.fingerprint) {
        return false;
    }
    if scales_match(job.pixels_per_pt, target_pixels_per_pt) {
        return true;
    }
    match held {
        None => true,
        Some(held) if held.fingerprint != job.fingerprint => true,
        Some(held) => {
            scale_error(job.pixels_per_pt, target_pixels_per_pt)
                < scale_error(held.pixels_per_pt, target_pixels_per_pt)
        }
    }
}

/// Identifies one running job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JobId(u64);

/// The jobs running in the background, bounded in number.
#[derive(Debug)]
pub struct JobTracker {
    running: Vec<(JobId, RasterKey)>,
    next_id: u64,
    limit: usize,
}

impl JobTracker {
    /// A tracker that allows `limit` jobs at once (at least one).
    pub fn new(limit: usize) -> Self {
        Self {
            running: Vec::new(),
            next_id: 0,
            limit: limit.max(1),
        }
    }

    /// How many jobs may run at once.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// How many jobs are running.
    pub fn running(&self) -> usize {
        self.running.len()
    }

    /// Whether another job may start.
    pub fn has_capacity(&self) -> bool {
        self.running.len() < self.limit
    }

    /// Whether some job is running for a page (any content or resolution).
    /// One page never has two jobs at once: while a zoom gesture or fast
    /// typing keeps changing what is wanted, the next job for that page is
    /// planned only when the running one reports back.
    pub fn is_running_page(&self, page: usize) -> bool {
        self.running.iter().any(|(_, k)| k.page == page)
    }

    /// Records a started job.
    pub fn start(&mut self, key: RasterKey) -> JobId {
        let id = JobId(self.next_id);
        self.next_id += 1;
        self.running.push((id, key));
        id
    }

    /// Records a finished job and returns what it was.
    pub fn finish(&mut self, id: JobId) -> Option<RasterKey> {
        let index = self.running.iter().position(|(i, _)| *i == id)?;
        Some(self.running.swap_remove(index).1)
    }
}

/// How many pages to rasterize in parallel on a machine with `cpus` cores:
/// half of them, between two and four.
pub fn concurrency_limit(cpus: usize) -> usize {
    (cpus / 2).clamp(2, 4)
}

/// The order in which to rasterize: the visible pages from `focus` outwards,
/// then the page before and the page after them.
pub fn raster_order(visible: Range<usize>, focus: usize, page_count: usize) -> Vec<usize> {
    let visible = visible.start.min(page_count)..visible.end.min(page_count);
    if visible.is_empty() {
        return Vec::new();
    }
    let focus = focus.clamp(visible.start, visible.end - 1);
    let mut order: Vec<usize> = visible.clone().collect();
    order.sort_by_key(|&page| (page.abs_diff(focus), page));
    if visible.start > 0 {
        order.push(visible.start - 1);
    }
    if visible.end < page_count {
        order.push(visible.end);
    }
    order
}

/// Turns RGBA pixels into the BGRA order GPU textures expect, in place.
pub fn rgba_to_bgra(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::size;

    const A4: Size<f32> = Size {
        width: 595.,
        height: 842.,
    };

    fn key(page: usize, fingerprint: u64, pixels_per_pt: f32) -> RasterKey {
        RasterKey {
            page,
            fingerprint,
            pixels_per_pt,
        }
    }

    fn held(fingerprint: u64, pixels_per_pt: f32) -> Option<HeldBitmap> {
        Some(HeldBitmap {
            fingerprint,
            pixels_per_pt,
        })
    }

    #[test]
    fn raster_scale_is_zoom_times_scale_factor_until_capped() {
        assert_eq!(raster_scale(1., 2., A4), 2.);
        assert_eq!(raster_scale(1.5, 1., A4), 1.5);
        let capped = raster_scale(5., 2., A4);
        assert!(capped < 10.);
        assert!(A4.height * capped <= MAX_BITMAP_SIDE + 0.5);
        assert!(A4.width * A4.height * capped * capped <= MAX_BITMAP_PIXELS * 1.001);
        let tall = size(100., 20_000.);
        assert!(tall.height * raster_scale(5., 2., tall) <= MAX_BITMAP_SIDE + 0.5);
        assert_eq!(raster_scale(0., 2., A4), MIN_PIXELS_PER_PT);
        assert_eq!(raster_scale(f32::NAN, 2., A4), MIN_PIXELS_PER_PT);
    }

    #[test]
    fn needs_raster_when_missing_changed_or_at_another_scale() {
        let want = key(3, 7, 2.);
        assert!(needs_raster(None, &want, true));
        assert!(needs_raster(held(8, 2.), &want, true));
        assert!(needs_raster(held(7, 1.), &want, true));
        assert!(!needs_raster(held(7, 2.005), &want, true));
    }

    #[test]
    fn a_moving_zoom_rescales_nothing_but_still_fills_and_updates_pages() {
        let want = key(3, 7, 2.);
        assert!(!needs_raster(held(7, 1.), &want, false));
        assert!(needs_raster(None, &want, false));
        assert!(needs_raster(held(8, 1.), &want, false));
    }

    #[test]
    fn results_for_superseded_content_are_dropped() {
        let job = key(0, 1, 2.);
        assert!(!accept_result(&job, Some(2), None, 2.));
        assert!(!accept_result(&job, None, None, 2.));
        assert!(accept_result(&job, Some(1), held(1, 1.), 2.));
    }

    #[test]
    fn results_at_an_outdated_zoom_are_kept_only_when_better() {
        let job = key(0, 1, 2.);
        // Nothing shown yet, or older content shown: better than that.
        assert!(accept_result(&job, Some(1), None, 3.));
        assert!(accept_result(&job, Some(1), held(9, 3.), 3.));
        // Already sharp at the target: ignore the outdated result.
        assert!(!accept_result(&job, Some(1), held(1, 3.), 3.));
        // Shown bitmap even further off than the result: take the result.
        assert!(accept_result(&job, Some(1), held(1, 1.), 3.));
    }

    #[test]
    fn tracker_bounds_running_jobs_and_spots_duplicates() {
        let mut jobs = JobTracker::new(2);
        let a = jobs.start(key(0, 1, 2.));
        assert!(jobs.has_capacity());
        let b = jobs.start(key(1, 1, 2.));
        assert!(!jobs.has_capacity());
        assert!(jobs.is_running_page(0));
        assert!(jobs.is_running_page(1));
        assert!(!jobs.is_running_page(2));
        assert!(key(0, 1, 2.).same_job(&key(0, 1, 2.001)));
        assert!(!key(0, 1, 2.).same_job(&key(0, 2, 2.)));
        assert_eq!(jobs.finish(a), Some(key(0, 1, 2.)));
        assert_eq!(jobs.finish(a), None);
        assert!(jobs.has_capacity());
        assert_eq!(jobs.running(), 1);
        jobs.finish(b);
        assert_eq!(jobs.running(), 0);
        assert_eq!(JobTracker::new(0).limit(), 1);
    }

    #[test]
    fn concurrency_is_bounded() {
        assert_eq!(concurrency_limit(1), 2);
        assert_eq!(concurrency_limit(6), 3);
        assert_eq!(concurrency_limit(32), 4);
    }

    #[test]
    fn raster_order_starts_at_the_focus_then_adds_the_margin() {
        assert_eq!(raster_order(3..6, 4, 10), vec![4, 3, 5, 2, 6]);
        assert_eq!(raster_order(0..2, 0, 2), vec![0, 1]);
        assert_eq!(raster_order(0..3, 9, 3), vec![2, 1, 0]);
        assert!(raster_order(2..2, 2, 5).is_empty());
        assert_eq!(raster_order(4..9, 4, 5), vec![4, 3]);
    }

    #[test]
    fn rgba_becomes_bgra() {
        let mut pixels = vec![1, 2, 3, 4, 5, 6, 7, 8];
        rgba_to_bgra(&mut pixels);
        assert_eq!(pixels, vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }
}
