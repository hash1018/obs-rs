//! A Browser Source: a web page, drawn by the engine in `crate::browser` and
//! composited like any other layer.
//!
//! # Nothing is copied out to the CPU
//!
//! The browser renders on its own GPU device and hands each picture over as a
//! shared texture handle. `D3d11SharedTextureSource` opens that handle on the
//! compositor's device and copies it into a texture of the pipeline's own —
//! GPU to GPU, inside the callback, because the handle is the browser's and
//! is only promised for the length of the call.
//!
//! # Its alpha is already in its colour
//!
//! A browser composites its page before handing it over, so what arrives is
//! colour multiplied by alpha. The layer says so — see `layer_for` — and the
//! compositor blends it by what it already holds rather than applying that
//! alpha a second time.
//!
//! # Changing anything reopens it
//!
//! A page is told its address, its size and its rate when the browser is
//! created. So the Properties dock's three fields each end this Source and
//! start another, which is why they commit when a control is let go rather
//! than while it is being used.
//!
//! # Windows only, so far
//!
//! There is one browser engine here and it is CEF on Windows — see
//! `crate::browser`. Everywhere else this kind opens as absent, with that as
//! the reason, rather than being missing from the Sources list on one
//! platform.

use std::sync::Arc;

use crate::domain::SourceSettings;
use crate::snapshots::SceneItemSnapshot;

use super::super::backend::BackendError;
use super::OpenOutcome;

/// What this Source shows, or why it shows nothing yet.
fn settings(
    item: &SceneItemSnapshot,
) -> Result<Result<&crate::domain::BrowserSourceSettings, String>, BackendError> {
    let SourceSettings::Browser(settings) = &item.settings else {
        return Err("scene item is not a browser source".into());
    };
    // A Source added and not yet pointed anywhere. Absent rather than an
    // error: it is the state every Browser Source starts in, and the Sources
    // dock is where the user is told to fill the address in.
    let url = settings.url.trim();
    if url.is_empty() {
        return Ok(Err("no address yet".to_owned()));
    }
    if !is_address(url) {
        return Ok(Err(format!(
            "\"{url}\" is not an address — it needs a scheme, like https://"
        )));
    }
    Ok(Ok(settings))
}

/// Whether this is an address to hand a browser, rather than something it
/// would go looking for.
///
/// A browser given `obs` or `hello` does what a browser does with what you
/// type in its bar: it searches for it, and a Chromium asked for its search
/// page has no picture to hand over off-screen — it puts a window on the
/// screen instead, over whatever is being recorded. So a Source says it is
/// not pointed anywhere rather than letting that happen, and a typo is a
/// sentence in the Sources dock instead of a browser window nobody asked
/// for.
fn is_address(url: &str) -> bool {
    url.contains("://") || url.starts_with("data:")
}

/// A Browser Source's page, and what it takes to open one again.
///
/// A page is a browser: a renderer process, its GPU allocations, and whatever
/// the page itself is running. Hiding the Source normally only stops it
/// drawing, which is what a page kept alive is for — a clock is right and a
/// chat is still connected the moment it comes back. A Source set to shut
/// down when it is hidden gives all of that up instead, and gets it back by
/// opening the page again: the same address at the same size, pushing through
/// the same callbacks into the pipeline that never went away.
pub(in crate::engine) struct OpenPage {
    /// The browser, while there is one. `None` only where the Source shut it
    /// down because nothing was looking at it.
    page: Option<crate::browser::Page>,
    /// What it was opened with, kept to open it again.
    options: crate::browser::PageOptions,
    /// The Source's name, for the one thing here that is worth a log line.
    name: String,
    /// Whether anything is looking at the Source.
    shown: bool,
    /// Whether being hidden closes the browser rather than pausing its
    /// drawing.
    shut_down_when_hidden: bool,
    /// Whether being shown again loads the page a second time.
    refresh_when_shown: bool,
    /// Whether a failure to open the page again has already been said. The
    /// browser engine stopping would otherwise be a line every frame.
    complained: bool,
}

impl OpenPage {
    // Called where a page is opened, which is Windows alone so far.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    fn new(page: crate::browser::Page, options: crate::browser::PageOptions, name: String) -> Self {
        Self {
            page: Some(page),
            options,
            name,
            shown: true,
            shut_down_when_hidden: false,
            refresh_when_shown: false,
            complained: false,
        }
    }

    /// Whether anything is looking at this page.
    ///
    /// A page that is kept is told, and stops painting — which is the whole
    /// point: a Source whose Scene is not the one being shown has a paused
    /// pipeline, and every picture drawn for it is a texture copied into a
    /// queue nothing is emptying. Its own timers and scripts keep running, so
    /// a clock is right again the moment it comes back.
    ///
    /// A page that is not kept is closed, and opened again when this turns
    /// back on — see [`Self::set_shut_down_when_hidden`].
    pub(in crate::engine) fn set_shown(&mut self, shown: bool) {
        if shown == self.shown {
            return;
        }
        self.shown = shown;
        // A page that was shut down is about to be opened, which loads it
        // from its address anyway — so this is only for one that was kept.
        let kept = self.page.is_some();
        self.apply();
        if shown && kept && self.refresh_when_shown {
            self.refresh();
        }
    }

    /// Whether coming back into view loads the page again.
    ///
    /// For a page that is only right when it has just been fetched — a
    /// scoreboard, a queue, anything a Scene is switched to in order to look
    /// at. Off by default: a page that was already correct is one that
    /// flashes through being blank for no reason.
    pub(in crate::engine) fn set_refresh_when_shown(&mut self, refresh: bool) {
        self.refresh_when_shown = refresh;
    }

    /// Loads the page again now, ignoring what was cached for it.
    ///
    /// Nothing for a page that has been shut down: there is no browser to
    /// tell, and the one opened when the Source is shown again fetches the
    /// address itself.
    pub(in crate::engine) fn refresh(&self) {
        if let Some(page) = &self.page {
            page.reload();
        }
    }

    /// Whether hiding the Source closes the browser rather than pausing it.
    ///
    /// What it buys is everything a page costs while nobody is looking:
    /// Chromium's processes, its memory, and whatever the page is fetching in
    /// the background. What it costs is the page starting from nothing when it
    /// comes back — a video from its first frame, a login asked for again —
    /// which is why it is a switch rather than what a Browser Source does.
    ///
    /// Applied at once rather than at the next change: turning it on while the
    /// Source is already hidden is a page to close now.
    pub(in crate::engine) fn set_shut_down_when_hidden(&mut self, shut_down: bool) {
        if shut_down != self.shut_down_when_hidden {
            self.shut_down_when_hidden = shut_down;
            self.apply();
        }
    }

    /// Does something to the page — a click, a wheel, a key.
    ///
    /// Nothing at all where the page has been shut down, which is the right
    /// answer: a Source nothing is showing is not one being clicked either.
    pub(in crate::engine) fn send(&self, input: crate::browser::PageInput) {
        if let Some(page) = &self.page {
            page.send(input);
        }
    }

    /// Brings the browser in line with the two switches above.
    fn apply(&mut self) {
        match (self.shown, self.shut_down_when_hidden) {
            (true, _) => match &self.page {
                Some(page) => page.set_shown(true),
                // Shown again after being shut down, so this is a new
                // browser for the same page.
                None => match crate::browser::open_page(self.options.clone()) {
                    Ok(page) => {
                        self.page = Some(page);
                        self.complained = false;
                    }
                    Err(error) => {
                        if !std::mem::replace(&mut self.complained, true) {
                            tracing::warn!(
                                "\"{}\": could not open the page again: {error}",
                                self.name
                            );
                        }
                    }
                },
            },
            // Dropping it is what closes the browser.
            (false, true) => self.page = None,
            (false, false) => {
                if let Some(page) = &self.page {
                    page.set_shown(false);
                }
            }
        }
    }
}

/// Blocks of a page's sound held between the browser and the mixer.
///
/// CEF hands them over in hundredths of a second, so this is about a second:
/// far more than the mixer will ever be behind by, and nothing next to a
/// file's read-ahead. A block that will not fit is dropped rather than
/// waited on — see where it is pushed.
#[cfg(target_os = "windows")]
const AUDIO_QUEUE_DEPTH: usize = 100;

/// Where a page's sound has got to, in samples.
///
/// A page's own timestamps are wall-clock milliseconds and its stream stops
/// and starts as it plays one thing after another; what the branch below
/// wants is a timeline that only goes forwards, at the rate the samples
/// arrive. So this counts them, which is also what the mixer does with what
/// it is given.
#[cfg(target_os = "windows")]
#[derive(Default)]
struct SampleClock {
    samples: i64,
}

#[cfg(target_os = "windows")]
impl SampleClock {
    /// One block of planar samples as a frame, stamped where it falls, or
    /// `None` for a block there is nothing to make one of.
    ///
    /// The two refusals are what the copy below would otherwise panic on,
    /// and a panic here is the browser engine's callback aborting the
    /// application — see `browser::guarded`. Neither has been seen: CEF
    /// hands over one equal-length plane per channel it announced.
    fn frame(
        &mut self,
        heard: &crate::browser::Heard<'_>,
    ) -> Option<media_pp::buffer::MediaBuffer> {
        use media_pp::ffmpeg::format::{Sample, sample::Type};

        let frames = heard.planes.first()?.len();
        if frames == 0 || heard.planes.iter().any(|plane| plane.len() != frames) {
            return None;
        }
        let mut audio = media_pp::ffmpeg::frame::Audio::new(
            Sample::F32(Type::Planar),
            frames,
            channel_layout(heard.planes.len()),
        );
        // What FFmpeg actually allocated, which is what the copy below is
        // allowed to write into: a frame it could not get a buffer for
        // reports no planes at all.
        if audio.planes() != heard.planes.len() {
            return None;
        }
        audio.set_rate(crate::browser::AUDIO_RATE);
        audio.set_pts(Some(self.samples));
        for (index, plane) in heard.planes.iter().enumerate() {
            // Plane by plane and sample by sample rather than as bytes:
            // `data_mut` reads `linesize[index]`, and planar audio sets only
            // `linesize[0]` — every plane after the first came back empty,
            // which is a panic inside a callback CEF cannot unwind through.
            audio.plane_mut::<f32>(index).copy_from_slice(plane);
        }
        self.samples += frames as i64;
        Some(media_pp::buffer::MediaBuffer::Audio(Arc::new(audio)))
    }
}

/// What FFmpeg calls the layout a page handed over.
#[cfg(target_os = "windows")]
fn channel_layout(channels: usize) -> media_pp::ffmpeg::ChannelLayout {
    match channels {
        0 | 1 => media_pp::ffmpeg::ChannelLayout::MONO,
        2 => media_pp::ffmpeg::ChannelLayout::STEREO,
        // Whatever else a page was mixed into, by count alone: FFmpeg's
        // default layout for that many channels is what every other Source
        // here is described by.
        other => media_pp::ffmpeg::ChannelLayout::default(other as i32),
    }
}

/// The page's size as the pipeline takes it: whole, even pixels.
///
/// Even because everything downstream of the compositor is NV12 — a
/// recording, a stream — and an odd dimension has no whole chroma pixel to
/// carry. Rounding here means the page is *told* the size that will be
/// drawn, rather than laid out for one size and scaled to another.
///
/// Read where a page is opened, which is Windows alone so far.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn page_size(settings: &crate::domain::BrowserSourceSettings) -> [u32; 2] {
    [settings.size[0].max(2) & !1, settings.size[1].max(2) & !1]
}

#[cfg(target_os = "windows")]
#[allow(clippy::too_many_arguments)]
pub(in crate::engine) fn open(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: Arc<std::sync::Mutex<windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext>>,
    handle: &media_pp::elements::D3d11VideoCompositorHandle,
    mixer: Option<&media_pp::elements::MixerHandle>,
    meter_wake: &crate::engine::audio::MeterWake,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<OpenOutcome, BackendError> {
    use std::sync::atomic::{AtomicBool, Ordering};

    use media_pp::elements::{D3d11SharedTextureSource, D3d11VideoCompositorInput};
    use media_pp::pipeline::PipelineBuilder;

    use super::{FilledRack, MediaFile, MediaMeters, OpenSource, filters, input_name, sound};

    let settings = match settings(item)? {
        Ok(settings) => settings,
        Err(absent) => return Ok(OpenOutcome::Absent(absent)),
    };
    let size = page_size(settings);
    let name = input_name(item);

    // Two frames of slack. The browser paints on its own thread and the
    // compositor takes what it is given, so a deeper queue would only hold
    // pictures the compositor has already replaced — and a shallower one
    // would make the browser wait on a compositor tick.
    let (source, pusher) = D3d11SharedTextureSource::new(
        name.clone(),
        device,
        Arc::clone(&context),
        size[0],
        size[1],
        2,
    )?;
    let FilledRack { rack, filters } = super::filled_rack(
        &name,
        device,
        context,
        filters::ChainFormat::Bgra,
        size,
        item,
    )?;

    // Whatever the page plays, as a channel in the mixer. Built before it
    // has played anything, and for a page that never does: CEF says nothing
    // about a page's sound until it makes one, and a Source that grew a
    // fader the moment a video started would be a mixer nobody could set up
    // in advance. What it costs meanwhile is an idle channel at silence.
    let meters = Arc::new(MediaMeters::default());
    let (audio_source, heard_pusher) = media_pp::elements::AppSource::new(
        format!("{name}-audio"),
        // Blocks of a hundredth of a second: a second of them, which is far
        // more than the mixer will ever be behind by and still nothing next
        // to a file's read-ahead.
        AUDIO_QUEUE_DEPTH,
    );
    let sound = sound::build_pushed(
        &name,
        media_pp::elements::AudioFormat::new(
            media_pp::ffmpeg::format::Sample::F32(media_pp::ffmpeg::format::sample::Type::Planar),
            crate::browser::AUDIO_RATE,
            crate::browser::AUDIO_CHANNELS,
        ),
        mixer,
        sound::SoundSettings {
            gain_db: settings.gain_db,
            muted: super::muted(settings.muted, item.visible),
            filters: &item.audio_filters,
        },
        &meters,
        meter_wake,
    )?;
    let volume = sound.as_ref().map(|sound| sound.volume.clone());

    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    let sound_name = name.clone();
    let mut routing = None;
    // By `&mut` rather than by value, as a stream's is: the closure has to
    // be `move` for what it consumes, and the routing has to come back out
    // to the engine loop that decides which mixes this page is in.
    let routing_out = &mut routing;
    let mut builder =
        PipelineBuilder::new(name.clone()).add_source(source, move |source, context| {
            let branch = context.branch().pipe(rack).to(sink)?;
            context.attach(source, 0, branch)?;
            Ok(())
        })?;
    if let Some(sound) = sound {
        builder = builder.add_source(audio_source, move |source, context| {
            *routing_out = Some(sound::attach(context, source, sound, &sound_name)?);
            Ok(())
        })?;
    }
    let pipeline = builder.build();
    pipeline.run()?;

    // Opened after the pipeline is running, so the first picture the page
    // paints has somewhere to go.
    let complained = AtomicBool::new(false);
    let complained_about = name.clone();
    let options = crate::browser::PageOptions {
        url: settings.url.clone(),
        size,
        fps: settings.fps,
        paint: Arc::new(move |painted| {
            // The browser was told this size, so a different one means it
            // drew something else — a device change, a page that resized
            // itself. Refused rather than stretched, and said once: this
            // runs at the page's frame rate.
            let pushed = if painted.size != size {
                Err(format!(
                    "the page painted {}x{} where it was told {}x{}",
                    painted.size[0], painted.size[1], size[0], size[1]
                ))
            } else {
                // Dropped rather than waited on. This is the browser
                // engine's own thread and every page shares it, so a Source
                // that is not being drained — its Scene is not the one being
                // shown, and its pipeline is paused — must cost this page a
                // picture rather than costing every page its engine.
                pusher
                    .try_push(painted.handle, None)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            };
            if let Err(error) = pushed
                && !complained.swap(true, Ordering::Relaxed)
            {
                tracing::warn!("\"{complained_about}\": {error}");
            }
        }),
        // `None` where the mixer never started: with no audio handler the
        // page's sound stays Chromium's, which plays it on this machine's
        // own output — the only place left for it to go.
        audio: routing.is_some().then(|| {
            let clock = std::sync::Mutex::new(SampleClock::default());
            let complained = AtomicBool::new(false);
            let complained_about = name.clone();
            Arc::new(move |heard: crate::browser::Heard<'_>| {
                let Some(frame) = clock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .frame(&heard)
                else {
                    return;
                };
                if let Err(error) = heard_pusher.try_push(frame)
                    && !complained.swap(true, Ordering::Relaxed)
                {
                    tracing::warn!("\"{complained_about}\": {error}");
                }
            }) as crate::browser::OnAudio
        }),
    };
    let mut page = match crate::browser::open_page(options.clone()) {
        Ok(page) => OpenPage::new(page, options, name.clone()),
        // Whatever the browser engine could not do, the Sources dock says.
        // Absent rather than a failure, for the reason an unmounted drive
        // is: an engine that is not there now may be next time.
        Err(absent) => return Ok(OpenOutcome::Absent(absent)),
    };
    // Told here as well as in the engine loop: a Source whose item is hidden
    // is closed on the next pass rather than kept open until something moves.
    page.set_shut_down_when_hidden(settings.shut_down_when_hidden);
    page.set_refresh_when_shown(settings.refresh_when_shown);

    Ok(OpenOutcome::Open(OpenSource {
        // Named for the file it was written for; what a page shares with one
        // is that it carries its own sound — see [`MediaFile`].
        media_file: Some(MediaFile {
            // Nothing to loop: a page is not playing a timeline.
            looping: None,
            volume,
            meters,
            pipeline: Arc::clone(&pipeline),
            sound: routing,
        }),
        // Told to the page rather than negotiated with it, so there is
        // nothing to write back.
        negotiated_size: None,
        source: super::super::backend::RunningSource::Owned(pipeline),
        layer,
        name,
        refreshed_token: None,
        filters: filters.open,
        filter_rack: filters.filter_rack,
        showing: true,
        running: true,
        pushed: None,
        // Held for as long as the Source is: dropping it closes the browser.
        page: Some(page),
    }))
}

#[cfg(target_os = "linux")]
pub(in crate::engine) fn open(
    _device: &Arc<media_pp::elements::CudaDevice>,
    _handle: &media_pp::elements::CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    _layer: media_pp::elements::VideoLayer,
) -> Result<OpenOutcome, BackendError> {
    // The settings are still read, so a Source pointed nowhere says that
    // rather than blaming the platform for it.
    match settings(item)? {
        Ok(_) => Ok(OpenOutcome::Absent(
            "there is no browser engine on this platform yet".to_owned(),
        )),
        Err(absent) => Ok(OpenOutcome::Absent(absent)),
    }
}

#[cfg(test)]
mod tests {
    use super::is_address;

    /// What separates an address from a search term, which is what a browser
    /// makes of anything else — and a Chromium asked for its search page puts
    /// a window on the screen, over whatever is being recorded.
    #[test]
    fn an_address_is_one_with_a_scheme() {
        for address in [
            "https://example.com",
            "http://192.168.0.2:8080/overlay?x=1",
            "file:///C:/pages/alert.html",
            "data:text/html,<b>hi</b>",
        ] {
            assert!(is_address(address), "{address} is an address");
        }
        for not in [
            "hello",
            "example.com",
            "obs rs",
            "/pages/alert.html",
            r"C:p",
        ] {
            assert!(!is_address(not), "{not} is not an address");
        }
    }
}
