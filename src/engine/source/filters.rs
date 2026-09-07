//! Turning a Source's stored filter list into elements in its chain.
//!
//! Each Source kind builds its own branch, so each calls in here just before
//! it terminates at the compositor. A Source with no filters gets exactly the
//! chain it had before this existed — [`FilterChain::append`] hands the
//! builder straight back, no element and no cost.
//!
//! # Two calls, because the handles have to escape
//!
//! A chain is assembled inside the closure `Pipeline::new` takes, and that
//! closure is where the elements have to end up. But a chroma key hands out
//! its [`ChromaKeyHandle`](media_pp::elements::ChromaKeyHandle) at
//! construction, and the handle has to reach [`OpenSource`] so a slider can
//! reach the element afterwards.
//!
//! So [`build`] runs outside the closure and answers with both halves: the
//! elements, boxed into a `FilterChain` that moves in, and the handles, which
//! stay behind. `Box<dyn Filter>` would have avoided the split, and
//! `ChainBuilder::pipe` does not take one — which is why the elements travel
//! as an enum per kind rather than as trait objects.
//!
//! # The bridge
//!
//! Every filter here works in BGRA, and half the Sources hand over NV12 — a
//! camera on either platform, and anything hardware-decoded. So a chain that
//! has filters and does not already carry BGRA gets one conversion in front
//! of them, and the compositor takes the keyed BGRA layer directly: both
//! backends accept a layer of either format and read which from the frame.
//!
//! Nothing converts back. On Linux that makes a filtered Color Source
//! *cheaper* than an unfiltered one, which converts to NV12 today for no
//! reason this still holds — see `color::open`.

use crate::domain::{Filter, FilterId, FilterSettings};

use super::super::backend::BackendError;

/// What the chain carries where the filters are appended.
///
/// The caller knows this and the chain does not: it is whatever the last
/// element before this point produces, and that differs per Source kind.
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
    /// A camera, or anything decoded. Bridged to BGRA when there are filters
    /// to run, and left alone when there are not.
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

    /// Applies what the project now says, without rebuilding anything.
    ///
    /// This is the whole reason the handles are kept. Everything a filter
    /// carries except its identity and its place in the chain can be changed
    /// through one of these, and the two that cannot are exactly the two
    /// that make [`crate::engine`] reopen the Source instead.
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

/// What identifies a chain as the one that is running.
///
/// Which filters, of which kinds, in which order — and nothing else. Change
/// any of it and the branch has to be rebuilt, which means reopening the
/// Source; change anything else and [`OpenFilter::apply`] is enough.
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
pub(in crate::engine) use windows::build;

#[cfg(target_os = "linux")]
pub(in crate::engine) use linux::build;

#[cfg(target_os = "windows")]
mod windows {
    use std::sync::{Arc, Mutex};

    use media_pp::elements::{D3d11ChromaKey, D3d11Scaler, D3d11ScalerFormat};
    use media_pp::pipeline::ChainBuilder;
    use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};

    use super::{BackendError, ChainFormat, Filter, FilterHandle, FilterSettings, OpenFilter};

    /// One filter built and waiting to be piped.
    ///
    /// An enum rather than `Box<dyn Filter>` because `ChainBuilder::pipe`
    /// takes its element by value and by type — see this module's own docs.
    enum Built {
        ChromaKey(D3d11ChromaKey),
    }

    /// The elements a Source's filters become, ready to move into the
    /// closure that assembles its chain.
    pub(in crate::engine) struct FilterChain {
        /// The conversion in front of them, for a Source that does not
        /// already hand over BGRA. `None` when it does, and when there are
        /// no filters at all.
        bridge: Option<D3d11Scaler>,
        built: Vec<Built>,
    }

    impl FilterChain {
        /// Appends everything onto `chain`, in order.
        ///
        /// A Source with no filters gets its builder back untouched, which
        /// is what keeps this out of the way of every Source that has none.
        pub(in crate::engine) fn append(self, chain: ChainBuilder) -> ChainBuilder {
            let mut chain = match self.bridge {
                Some(bridge) => chain.pipe(bridge),
                None => chain,
            };
            for built in self.built {
                chain = match built {
                    Built::ChromaKey(key) => chain.pipe(key),
                };
            }
            chain
        }
    }

    /// Builds a Source's filters, and the handles that reach them afterwards.
    ///
    /// `width`/`height` are the picture the filters will see, which is the
    /// Source's own — every filter here is a per-pixel transform and none of
    /// them resizes.
    pub(in crate::engine) fn build(
        name: &str,
        device: &ID3D11Device,
        context: Arc<Mutex<ID3D11DeviceContext>>,
        incoming: ChainFormat,
        filters: &[Filter],
        width: u32,
        height: u32,
    ) -> Result<(FilterChain, Vec<OpenFilter>), BackendError> {
        if filters.is_empty() {
            return Ok((
                FilterChain {
                    bridge: None,
                    built: Vec::new(),
                },
                Vec::new(),
            ));
        }

        // `D3d11ScalerFormat::Bgra` names the chroma key among the things it
        // is for, so this is the conversion the library already intended
        // rather than one invented here.
        let bridge = match incoming {
            ChainFormat::Bgra => None,
            ChainFormat::Nv12 => Some(
                D3d11Scaler::new(
                    format!("{name}-to-bgra"),
                    device,
                    context.clone(),
                    D3d11ScalerFormat::Bgra,
                    width,
                    height,
                )
                .map_err(|error| BackendError::from(error.to_string()))?,
            ),
        };

        let mut built = Vec::with_capacity(filters.len());
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
                    built.push(Built::ChromaKey(element));
                    open.push(OpenFilter {
                        id: filter.id,
                        handle: FilterHandle::ChromaKey(handle),
                    });
                }
            }
        }
        Ok((FilterChain { bridge, built }, open))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use media_pp::elements::{CudaChromaKey, CudaConverter, CudaDevice, CudaFrameFormat};
    use media_pp::pipeline::ChainBuilder;

    use super::{BackendError, ChainFormat, Filter, FilterHandle, FilterSettings, OpenFilter};

    /// One filter built and waiting to be piped — see the Windows twin.
    enum Built {
        ChromaKey(CudaChromaKey),
    }

    /// The elements a Source's filters become, ready to move into the
    /// closure that assembles its chain.
    pub(in crate::engine) struct FilterChain {
        /// The conversion in front of them, for a Source that does not
        /// already hand over BGRA.
        bridge: Option<CudaConverter>,
        built: Vec<Built>,
    }

    impl FilterChain {
        pub(in crate::engine) fn append(self, chain: ChainBuilder) -> ChainBuilder {
            let mut chain = match self.bridge {
                Some(bridge) => chain.pipe(bridge),
                None => chain,
            };
            for built in self.built {
                chain = match built {
                    Built::ChromaKey(key) => chain.pipe(key),
                };
            }
            chain
        }
    }

    pub(in crate::engine) fn build(
        name: &str,
        device: &CudaDevice,
        incoming: ChainFormat,
        filters: &[Filter],
        width: u32,
        height: u32,
    ) -> Result<(FilterChain, Vec<OpenFilter>), BackendError> {
        if filters.is_empty() {
            return Ok((
                FilterChain {
                    bridge: None,
                    built: Vec::new(),
                },
                Vec::new(),
            ));
        }

        // `CudaScaler` cannot do this — it refuses a YUV/RGB pair either way
        // — which is why `CudaConverter` grew the direction.
        let bridge = match incoming {
            ChainFormat::Bgra => None,
            ChainFormat::Nv12 => Some(
                CudaConverter::new(
                    format!("{name}-to-bgra"),
                    device,
                    CudaFrameFormat::Bgra,
                    width,
                    height,
                )
                .map_err(|error| BackendError::from(error.to_string()))?,
            ),
        };

        let mut built = Vec::with_capacity(filters.len());
        let mut open = Vec::with_capacity(filters.len());
        for filter in filters {
            match &filter.settings {
                FilterSettings::ChromaKey(settings) => {
                    let (element, handle) = CudaChromaKey::new(
                        format!("{name}-key-{}", filter.id.0),
                        device,
                        width,
                        height,
                        super::chroma_key_options(settings),
                    )
                    .map_err(|error| BackendError::from(error.to_string()))?;
                    handle.set_enabled(filter.enabled);
                    built.push(Built::ChromaKey(element));
                    open.push(OpenFilter {
                        id: filter.id,
                        handle: FilterHandle::ChromaKey(handle),
                    });
                }
            }
        }
        Ok((FilterChain { bridge, built }, open))
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
