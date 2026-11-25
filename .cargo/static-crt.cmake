# Toolchain file for libopus (built by audiopus_sys through the cmake crate):
# static C runtime (/MT), matching `+crt-static` in .cargo/config.toml.
# Needs policy CMP0091 NEW, which CMAKE_POLICY_VERSION_MINIMUM >= 3.15 gives.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded")
