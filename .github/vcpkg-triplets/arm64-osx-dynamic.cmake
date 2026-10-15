# vcpkg's own arm64-osx-dynamic triplet, with the macOS FFmpeg and what it
# links built for the oldest macOS obs-rs.app runs on — LSMinimumSystemVersion
# in assets/macos/Info.plist, which the two must agree with. Without it each
# library is built for the runner's own SDK, and a bundle that says it runs
# on 13.0 would not load on anything older than the runner.
set(VCPKG_TARGET_ARCHITECTURE arm64)
set(VCPKG_CRT_LINKAGE dynamic)
set(VCPKG_LIBRARY_LINKAGE dynamic)
set(VCPKG_CMAKE_SYSTEM_NAME Darwin)
set(VCPKG_OSX_ARCHITECTURES arm64)
set(VCPKG_OSX_DEPLOYMENT_TARGET 13.0)
