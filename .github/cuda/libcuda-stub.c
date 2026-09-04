/*
 * A libcuda.so that exists only to be linked against.
 *
 * media-pp reaches the CUDA driver API through `#[link(name = "cuda")]` on
 * Linux, so `-lcuda` has to resolve at link time — on Windows the same block
 * is `raw-dylib` and resolves at run time instead, which is why only this side
 * needs anything. A GitHub runner has no NVIDIA driver and therefore no
 * libcuda, so without this the test binary and the release binary both fail to
 * link with "unable to find library -lcuda". Type-checking does not: `cargo
 * clippy` passed on the same tree that could not link.
 *
 * NVIDIA ships stubs for exactly this, in the CUDA toolkit. This is here
 * instead because it needs no apt repository, no toolkit download, and no
 * package that has to exist for whichever Ubuntu the runner happens to be.
 *
 * The list below is every symbol media-pp's `platform::cuda::driver` declares.
 * When it grows one, the link fails naming the missing symbol, which is the
 * failure this file should have: loud, and pointing at what to add.
 *
 * The SONAME matters as much as the symbols. It is set to `libcuda.so.1` when
 * this is built, so a binary linked against it records a dependency on
 * `libcuda.so.1` — what a real driver installs — rather than on `libcuda.so`,
 * which only appears with a development package. Getting that wrong would
 * produce a release binary that runs nowhere.
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
STUB(cuDeviceGet)
STUB(cuDevicePrimaryCtxRelease_v2)
STUB(cuDevicePrimaryCtxRetain)
STUB(cuGetErrorString)
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
