/*
 * A libcuda.so that exists only to be linked against.
 *
 * media-pp reaches the CUDA driver API through `#[link(name = "cuda")]` on
 * Linux, so `-lcuda` has to resolve when a binary is produced. On Windows the
 * same extern block is `raw-dylib` and resolves at run time, which is why only
 * this side ever needs anything. A GitHub runner has no NVIDIA driver and
 * therefore no libcuda, so without this the test binary and the release binary
 * both fail to link. Type-checking does not: `cargo clippy` passes on a tree
 * that cannot be linked at all.
 *
 * NVIDIA ships stubs for exactly this in the CUDA toolkit. This is here
 * instead because it needs no apt repository, no toolkit download, and no
 * package that has to exist for whichever Ubuntu the runner happens to be.
 *
 * # Two places declare these, not one
 *
 * The list below is the union of
 *
 *   - `media_pp::platform::cuda::driver`, and
 *   - `src/engine/preview/linux.rs`, which has an extern block of its own for
 *     the external-memory calls that import a Vulkan allocation into CUDA.
 *
 * Missing the second is how this file was first written, and the link failed
 * naming `cuImportExternalMemory` and friends. That is the failure this should
 * have — loud, and pointing at exactly what to add — but both places have to
 * be looked at when adding to it. `grep -rhoE '\bfn (cu[A-Z][A-Za-z0-9_]*)'`
 * over each source tree is what produced this list.
 *
 * # Two details that would be quiet bugs
 *
 * The SONAME is set to `libcuda.so.1` where this is built, so a binary linked
 * against it records a dependency on what a real driver installs rather than
 * on the `libcuda.so` that only appears with a development package. Getting
 * that wrong produces a release binary that runs nowhere.
 *
 * Every function answers 100, `CUDA_ERROR_NO_DEVICE`. Zero would be
 * `CUDA_SUCCESS` and would tell a caller the GPU is there, which on a runner
 * is how a clean skip turns into a crash. In a release build these bodies are
 * never reached: the loader binds to the real driver.
 */

#define STUB(name)                                                             \
    int name(void);                                                            \
    int name(void) { return 100; }

STUB(cuCtxPopCurrent_v2)
STUB(cuCtxPushCurrent_v2)
STUB(cuCtxSynchronize)
STUB(cuDestroyExternalMemory)
STUB(cuDeviceGet)
STUB(cuDevicePrimaryCtxRelease_v2)
STUB(cuDevicePrimaryCtxRetain)
STUB(cuExternalMemoryGetMappedBuffer)
STUB(cuGetErrorString)
STUB(cuImportExternalMemory)
STUB(cuInit)
STUB(cuLaunchKernel)
STUB(cuMemAlloc_v2)
STUB(cuMemFree_v2)
STUB(cuMemcpy2D_v2)
STUB(cuMemcpyHtoD_v2)
STUB(cuMemsetD2D16_v2)
STUB(cuMemsetD2D8_v2)
STUB(cuModuleGetFunction)
STUB(cuModuleLoadData)
STUB(cuModuleUnload)
