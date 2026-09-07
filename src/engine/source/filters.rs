//! Turning a Source's stored filter list into elements in its chain.
//!
//! Each Source kind that runs filters puts a [`Rack`] in its branch, just
//! before it terminates at the compositor, and hands the [`FilterRack`] back
//! for [`OpenSource`](super::OpenSource) to keep. A Source with no filters
//! gets an empty rack, which is a wire: the buffer that arrives is the buffer
//! that leaves, not a copy of it.
//!
//! # Why a rack and not a chain
//!
//! A branch is assembled once, inside the closure `Pipeline::new` takes, and
//! keeps its shape for as long as it exists. Adding a filter therefore used
//! to mean building the branch again, which means reopening the Source — a
//! camera going dark and coming back, and on Wayland a portal dialog for a
//! checkbox. A rack is the one place in a media-pp graph whose contents can
//! be exchanged while frames flow, so adding, removing and reordering are all
//! [`FilterRack::refill`] now, and the picture never stops.
//!
//! What a rack cannot absorb is what the filters cannot do without: a device,
//! and the picture size they were built for. Those are what [`FilterRack`]
//! holds beside the handle, so a refill can build the new elements without
//! the engine loop having to know which backend it is on.
//!
//! # The bridge
//!
//! Every filter here works in BGRA, and half the Sources hand over NV12 — a
//! camera on either platform, and anything hardware-decoded. So a rack that
//! is filled and does not already carry BGRA gets one conversion at the head
//! of it, and the compositor takes the keyed BGRA layer directly: both
//! backends accept a layer of either format and read which from the frame.
//! An emptied rack drops the conversion with everything else, and the layer
//! is NV12 again on the next frame.
//!
//! Nothing converts back. On Linux that makes a filtered Color Source
//! *cheaper* than an unfiltered one, which converts to NV12 today for no
//! reason this still holds — see `color::open`.

use media_pp::contract::{InputContract, MediaKind, MemoryDomain, OutputContract, PortContract};
use media_pp::element::Filter as PpFilter;
use media_pp::elements::{Rack, RackHandle};

use crate::domain::{Filter, FilterId, FilterSettings};

use super::super::backend::BackendError;

/// What the chain carries where the rack sits.
///
/// The caller knows this and the rack does not: it is whatever the last
/// element before it produces, and that differs per Source kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::engine) enum ChainFormat {
    /// What every filter here wants, so no bridge is inserted.
    ///
    /// Only the camera is wired to this module so far and a camera is NV12,
    /// so nothing names this yet. The Sources that will are the ones already
    /// producing BGRA — a capture, a Color, an Image — and each needs one
    /// thing more than a name: on the CUDA backend they convert to NV12 on
    /// the way to the compositor today, and a keyed frame cannot make that
    /// trip. NV12 has no alpha, so converting one throws away exactly what
    /// the key just wrote.
    #[allow(dead_code)]
    Bgra,
    /// A camera, or anything decoded. Bridged to BGRA when the rack has
    /// filters to run, and left alone when it does not.
    Nv12,
}

/// One running filter, and the way back to it.
///
/// The id is the database row's, which is what a command names when it
/// retunes one — see `SourceCommand::SetChromaKeySettings`.
pub(in crate::engine) struct OpenFilter {
    pub(in crate::engine) id: FilterId,
    pub(in crate::engine) handle: FilterHandle,
}

/// What a backend answers a build with: the elements to put in the rack, in
/// order, and the handles for the ones the project can retune afterwards.
///
/// Two vectors of different lengths, which is why they are not one. A bridge
/// is an element with nothing to control, so it is in the first and not the
/// second — and the second is what `OpenSource::filters` becomes, which must
/// line up with the stored list rather than with the chain.
type Built = Result<(Vec<Box<dyn PpFilter>>, Vec<OpenFilter>), BackendError>;

/// The runtime control for one filter, by kind.
pub(in crate::engine) enum FilterHandle {
    ChromaKey(media_pp::elements::ChromaKeyHandle),
}

impl OpenFilter {
    /// Which kind this is, read from the handle it holds.
    fn kind(&self) -> crate::domain::FilterKind {
        match self.handle {
            FilterHandle::ChromaKey(_) => crate::domain::FilterKind::ChromaKey,
        }
    }

    /// Applies what the project now says, without building anything.
    ///
    /// This is what the handles are kept for. Everything a filter carries
    /// except its identity and its place in the chain can be changed through
    /// one of these; the two that cannot are what makes the engine refill the
    /// rack instead — see [`shape`].
    pub(in crate::engine) fn apply(&self, filter: &Filter) {
        match &self.handle {
            FilterHandle::ChromaKey(handle) => handle.set_enabled(filter.enabled),
        }
        self.retune(&filter.settings);
    }

    /// The settings alone, for a slider still under the pointer.
    ///
    /// The project is not told until the gesture ends — see the mixer's own
    /// fader, which splits the same way and for the same reason — so this is
    /// how the picture follows the pointer meanwhile.
    pub(in crate::engine) fn retune(&self, settings: &FilterSettings) {
        match (&self.handle, settings) {
            (FilterHandle::ChromaKey(handle), FilterSettings::ChromaKey(settings)) => {
                handle.set_options(chroma_key_options(settings));
            }
        }
    }
}

/// A Source's rack, and everything needed to fill it again.
///
/// Held by the running Source rather than by its branch: what is inside the
/// rack belongs to the rack, and this is only the way back to it. Dropping
/// this stops nothing — the rack keeps running whatever it was last given.
pub(in crate::engine) struct FilterRack {
    handle: RackHandle,
    backend: backend::Backend,
}

impl FilterRack {
    /// Builds the elements for `filters` and swaps them in, answering the
    /// handles that reach the ones now running.
    ///
    /// Atomic from the caller's side: nothing is swapped in until every
    /// element has been built, so a filter that fails to construct leaves the
    /// rack running exactly what it was. The swap itself takes effect on the
    /// next frame.
    ///
    /// An empty `filters` empties the rack, and with it the bridge — a Source
    /// whose last filter was removed costs what it did before any were added.
    pub(in crate::engine) fn refill(
        &self,
        filters: &[Filter],
    ) -> Result<Vec<OpenFilter>, BackendError> {
        let (elements, open) = backend::build(&self.backend, filters)?;
        self.handle
            .replace(elements)
            .map_err(|error| BackendError::from(error.to_string()))?;
        Ok(open)
    }
}

/// The rack every Source with filters puts in its branch.
///
/// Its contracts are declared rather than derived — what is in one changes,
/// so nothing could derive them — and what they declare is the one thing true
/// of this rack whatever it holds: decoded video frames, in the backend's own
/// memory. The pixel format varies within that (NV12 in, BGRA out once a
/// bridge is there), and a contract naming one would refuse the other.
fn new_rack(name: &str, memory: MemoryDomain) -> (Rack, RackHandle) {
    let port = PortContract::frame(MediaKind::VideoFrame, memory);
    Rack::new(
        format!("{name}-filters"),
        InputContract::Fixed(port),
        OutputContract::Fixed(port),
    )
}

/// What identifies a chain as the one that is running.
///
/// Which filters, of which kinds, in which order — and nothing else. Change
/// any of it and the rack is refilled; change anything else and
/// [`OpenFilter::apply`] is enough.
pub(in crate::engine) fn shape(filters: &[Filter]) -> Vec<(FilterId, crate::domain::FilterKind)> {
    filters
        .iter()
        .map(|filter| {
            (
                filter.id,
                match filter.settings {
                    FilterSettings::ChromaKey(_) => crate::domain::FilterKind::ChromaKey,
                },
            )
        })
        .collect()
}

/// The same, for the chain that is actually running.
pub(in crate::engine) fn running_shape(
    open: &[OpenFilter],
) -> Vec<(FilterId, crate::domain::FilterKind)> {
    open.iter()
        .map(|filter| (filter.id, filter.kind()))
        .collect()
}

#[cfg(target_os = "windows")]
use windows as backend;
#[cfg(target_os = "windows")]
pub(in crate::engine) use windows::rack;

#[cfg(target_os = "linux")]
use linux as backend;
#[cfg(target_os = "linux")]
pub(in crate::engine) use linux::rack;

#[cfg(target_os = "windows")]
mod windows {
    use std::sync::{Arc, Mutex};

    use media_pp::contract::MemoryDomain;
    use media_pp::element::Filter as PpFilter;
    use media_pp::elements::{D3d11ChromaKey, D3d11Scaler, D3d11ScalerFormat, Rack};
    use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};

    use super::{
        BackendError, ChainFormat, Filter, FilterHandle, FilterRack, FilterSettings, OpenFilter,
    };

    /// What a refill needs and the rack cannot hold for it.
    ///
    /// The device is owned rather than borrowed: this outlives the call that
    /// opened the Source, and a `windows-rs` interface is a refcounted
    /// pointer, so keeping one is a clone rather than a lifetime.
    pub(super) struct Backend {
        name: String,
        device: ID3D11Device,
        context: Arc<Mutex<ID3D11DeviceContext>>,
        incoming: ChainFormat,
        width: u32,
        height: u32,
    }

    /// Creates a Source's rack, and the way back to it.
    ///
    /// `width`/`height` are the picture the filters will see, which is the
    /// Source's own — every filter here is a per-pixel transform and none of
    /// them resizes.
    pub(in crate::engine) fn rack(
        name: &str,
        device: &ID3D11Device,
        context: Arc<Mutex<ID3D11DeviceContext>>,
        incoming: ChainFormat,
        width: u32,
        height: u32,
    ) -> (Rack, FilterRack) {
        let (rack, handle) = super::new_rack(name, MemoryDomain::D3d11);
        let backend = Backend {
            name: name.to_owned(),
            device: device.clone(),
            context,
            incoming,
            width,
            height,
        };
        (rack, FilterRack { handle, backend })
    }

    pub(super) fn build(backend: &Backend, filters: &[Filter]) -> super::Built {
        if filters.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let Backend {
            name,
            device,
            context,
            incoming,
            width,
            height,
        } = backend;

        let mut elements: Vec<Box<dyn PpFilter>> = Vec::with_capacity(filters.len() + 1);
        // `D3d11ScalerFormat::Bgra` names the chroma key among the things it
        // is for, so this is the conversion the library already intended
        // rather than one invented here.
        if *incoming == ChainFormat::Nv12 {
            elements.push(Box::new(
                D3d11Scaler::new(
                    format!("{name}-to-bgra"),
                    device,
                    context.clone(),
                    D3d11ScalerFormat::Bgra,
                    *width,
                    *height,
                )
                .map_err(|error| BackendError::from(error.to_string()))?,
            ));
        }

        let mut open = Vec::with_capacity(filters.len());
        for filter in filters {
            match &filter.settings {
                FilterSettings::ChromaKey(settings) => {
                    let (element, handle) = D3d11ChromaKey::new(
                        format!("{name}-key-{}", filter.id.0),
                        device,
                        context.clone(),
                        super::chroma_key_options(settings),
                    )
                    .map_err(|error| BackendError::from(error.to_string()))?;
                    handle.set_enabled(filter.enabled);
                    elements.push(Box::new(element));
                    open.push(OpenFilter {
                        id: filter.id,
                        handle: FilterHandle::ChromaKey(handle),
                    });
                }
            }
        }
        Ok((elements, open))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::sync::Arc;

    use media_pp::contract::MemoryDomain;
    use media_pp::element::Filter as PpFilter;
    use media_pp::elements::{CudaChromaKey, CudaConverter, CudaDevice, CudaFrameFormat, Rack};

    use super::{
        BackendError, ChainFormat, Filter, FilterHandle, FilterRack, FilterSettings, OpenFilter,
    };

    /// What a refill needs and the rack cannot hold for it — see the Windows
    /// twin, whose device is a refcounted interface and needs no wrapper.
    /// A `CudaDevice` is not `Clone` — media-pp hands it out by reference and
    /// documents that it need only outlive the constructor calls — so the
    /// backend keeps the one it opened in an `Arc` and this shares it.
    pub(super) struct Backend {
        name: String,
        device: Arc<CudaDevice>,
        incoming: ChainFormat,
        width: u32,
        height: u32,
    }

    pub(in crate::engine) fn rack(
        name: &str,
        device: &Arc<CudaDevice>,
        incoming: ChainFormat,
        width: u32,
        height: u32,
    ) -> (Rack, FilterRack) {
        let (rack, handle) = super::new_rack(name, MemoryDomain::Cuda);
        let backend = Backend {
            name: name.to_owned(),
            device: device.clone(),
            incoming,
            width,
            height,
        };
        (rack, FilterRack { handle, backend })
    }

    pub(super) fn build(backend: &Backend, filters: &[Filter]) -> super::Built {
        if filters.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let Backend {
            name,
            device,
            incoming,
            width,
            height,
        } = backend;

        let mut elements: Vec<Box<dyn PpFilter>> = Vec::with_capacity(filters.len() + 1);
        // `CudaScaler` cannot do this — it refuses a YUV/RGB pair either way
        // — which is why `CudaConverter` grew the direction.
        if *incoming == ChainFormat::Nv12 {
            elements.push(Box::new(
                CudaConverter::new(
                    format!("{name}-to-bgra"),
                    device,
                    CudaFrameFormat::Bgra,
                    *width,
                    *height,
                )
                .map_err(|error| BackendError::from(error.to_string()))?,
            ));
        }

        let mut open = Vec::with_capacity(filters.len());
        for filter in filters {
            match &filter.settings {
                FilterSettings::ChromaKey(settings) => {
                    let (element, handle) = CudaChromaKey::new(
                        format!("{name}-key-{}", filter.id.0),
                        device,
                        *width,
                        *height,
                        super::chroma_key_options(settings),
                    )
                    .map_err(|error| BackendError::from(error.to_string()))?;
                    handle.set_enabled(filter.enabled);
                    elements.push(Box::new(element));
                    open.push(OpenFilter {
                        id: filter.id,
                        handle: FilterHandle::ChromaKey(handle),
                    });
                }
            }
        }
        Ok((elements, open))
    }
}

/// The library's options for what the project stored.
///
/// One place rather than one per backend: both chroma keys take the same
/// type, and the mapping is the only thing that knows a custom colour lives
/// beside the method here and inside it there.
fn chroma_key_options(
    settings: &crate::domain::ChromaKeySettings,
) -> media_pp::elements::ChromaKeyOptions {
    media_pp::elements::ChromaKeyOptions {
        method: match settings.method {
            crate::domain::ChromaKeyMethod::Green => media_pp::elements::ChromaKeyMethod::Green,
            crate::domain::ChromaKeyMethod::Blue => media_pp::elements::ChromaKeyMethod::Blue,
            crate::domain::ChromaKeyMethod::Custom => {
                let [red, green, blue] = settings.custom_rgb;
                media_pp::elements::ChromaKeyMethod::Custom(media_pp::color::Color::new(
                    red, green, blue,
                ))
            }
        },
        threshold: settings.threshold,
        smoothing: settings.smoothing,
    }
}
