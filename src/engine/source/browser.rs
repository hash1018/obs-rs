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

/// The page's size as the pipeline takes it: whole, even pixels.
///
/// Even because everything downstream of the compositor is NV12 — a
/// recording, a stream — and an odd dimension has no whole chroma pixel to
/// carry. Rounding here means the page is *told* the size that will be
/// drawn, rather than laid out for one size and scaled to another.
fn page_size(settings: &crate::domain::BrowserSourceSettings) -> [u32; 2] {
    [settings.size[0].max(2) & !1, settings.size[1].max(2) & !1]
}

#[cfg(target_os = "windows")]
pub(in crate::engine) fn open(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: Arc<std::sync::Mutex<windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext>>,
    handle: &media_pp::elements::D3d11VideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<OpenOutcome, BackendError> {
    use std::sync::atomic::{AtomicBool, Ordering};

    use media_pp::elements::{D3d11SharedTextureSource, D3d11VideoCompositorInput};
    use media_pp::pipeline::Pipeline;

    use super::{FilledRack, OpenSource, filters, input_name};

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

    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(rack).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    // Opened after the pipeline is running, so the first picture the page
    // paints has somewhere to go.
    let complained = AtomicBool::new(false);
    let complained_about = name.clone();
    let page = crate::browser::open_page(
        &settings.url,
        size,
        settings.fps,
        Box::new(move |painted| {
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
    );
    let page = match page {
        Ok(page) => page,
        // Whatever the browser engine could not do, the Sources dock says.
        // Absent rather than a failure, for the reason an unmounted drive
        // is: an engine that is not there now may be next time.
        Err(absent) => return Ok(OpenOutcome::Absent(absent)),
    };

    Ok(OpenOutcome::Open(OpenSource {
        media_file: None,
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
