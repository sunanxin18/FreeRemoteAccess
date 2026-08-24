cmake_minimum_required(VERSION 3.14)

project(freeremote_native LANGUAGES NONE)

get_filename_component(FRD_REPOSITORY_ROOT
  "${CMAKE_CURRENT_LIST_DIR}/../../../.." REALPATH)
set(FRD_CARGO_TARGET_DIR "${CMAKE_CURRENT_BINARY_DIR}/cargo")
set(FRD_RUST_LIBRARY
  "${FRD_CARGO_TARGET_DIR}/release/freeremote_native.dll")

add_custom_command(
  OUTPUT "${FRD_RUST_LIBRARY}"
  COMMAND cargo build -p freeremote_ffi --release
          --target-dir "${FRD_CARGO_TARGET_DIR}"
  WORKING_DIRECTORY "${FRD_REPOSITORY_ROOT}"
  DEPENDS
    "${FRD_REPOSITORY_ROOT}/Cargo.toml"
    "${FRD_REPOSITORY_ROOT}/Cargo.lock"
    "${FRD_REPOSITORY_ROOT}/src/app/connection.rs"
    "${FRD_REPOSITORY_ROOT}/native/freeremote_ffi/Cargo.toml"
    "${FRD_REPOSITORY_ROOT}/native/freeremote_ffi/src/lib.rs"
  COMMENT "Building FreeRemoteAccess Rust bridge"
  VERBATIM
)
add_custom_target(freeremote_native_build ALL DEPENDS "${FRD_RUST_LIBRARY}")

set(freeremote_native_bundled_libraries
  "${FRD_RUST_LIBRARY}"
  PARENT_SCOPE
)
