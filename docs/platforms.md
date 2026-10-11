# Platforms

RustingEngine 1.0 ships games for Windows and Linux desktops. macOS
through MoltenVK and Android are planned next (roadmap Milestone 21). Web,
iOS and game consoles are not supported before 1.0. This page records
why, and what would have to change first.

| Platform | Status | Main blocker |
| --- | --- | --- |
| Windows, Linux | Supported | |
| macOS | Planned (Milestone 21) | Vulkan through MoltenVK; fallbacks for features it lacks |
| Android | Planned (Milestone 21) | Touch input, app lifecycle, mobile quality profiles |
| Web | After 1.0 | No Vulkan, sockets or files in a browser |
| iOS | After 1.0 | Needs the macOS and Android work first, plus App Store packaging |
| Consoles | After 1.0 | Closed SDKs and graphics APIs under NDA |

## Web

A browser export is the largest change of the three, because several
layers assume a desktop operating system:

- **Graphics.** The renderer, the GPU physics solvers and the readback
  path are written against Vulkan through `vulkano`, with 26 GLSL shader
  files compiled to SPIR-V. Browsers offer WebGPU and WebGL 2, not
  Vulkan. A web build needs a second GPU backend, and every shader needs a
  WGSL version that passes WebGPU validation. WebGL 2 has no compute
  shaders, so the GPU physics would need WebGPU.
- **Threads.** `bevy_ecs` runs systems on threads (`multi_threaded`), and
  the CPU physics uses `rayon`. WebAssembly threads need
  `SharedArrayBuffer`, which needs a server that sends the COOP and COEP
  headers. Without them the game runs on one thread.
- **Networking.** `net` uses `std::net` TCP and UDP sockets. A browser has
  neither: it needs WebSocket (left open in the transport item) or
  WebRTC data channels, and a server that accepts them.
- **Files.** Games read cooked scenes and `assets/` from the folder beside
  the executable, and write saves and crash reports to a user data
  folder. A browser has no file system, so loading must become
  asynchronous fetches and saves must go to browser storage.
- **Tooling.** `rusting test`, `capture` and `export` start the game as a
  native process. Testing a web build needs a headless browser runner.

**Unblocks it:** a GPU backend that also targets WebGPU (for example a
`wgpu` renderer behind the same interfaces), WGSL versions of the shaders,
and WebSocket transport. Each needs new dependencies and the owner's
approval.

## iOS

iOS has no Vulkan driver either, but MoltenVK runs on it, so the renderer
could be kept. What blocks it:

- **macOS first.** MoltenVK lacks some Vulkan features the renderer may
  use. The capability fallbacks planned for macOS are the same ones iOS
  needs, and there is no macOS runner to check them on yet.
- **Touch and lifecycle.** Input is keyboard, mouse and gamepad. iOS
  needs touch input, and it needs the game to pause, release GPU memory
  and resume when the app goes to the background. That is the same work
  as the Android item.
- **Memory.** iOS ends an app that uses too much memory, with no warning
  to the game. The engine has no mobile quality profile or memory budget
  yet.
- **Packaging.** An App Store build needs an Xcode project, code signing
  and Apple's review. `rusting export` builds a plain folder, and there is
  no macOS machine in CI to build or sign on.

**Unblocks it:** the macOS and Android items in Milestone 21, then an
Xcode export template.

## Consoles

Console work cannot start in this repository:

- **Closed SDKs.** Nintendo, Sony and Microsoft give their SDKs, tools and
  documents only to registered developers, under NDA. Code that uses them
  cannot be published in an open-source engine. It would have to live in a
  separate, private port.
- **Graphics.** PlayStation uses its own graphics API, and Xbox uses
  Direct3D 12. Neither offers Vulkan, so both need a second GPU backend,
  as the web does. Switch has Vulkan, but its native API is the one
  usually expected.
- **Toolchains and dependencies.** Rust's console targets are not public.
  The window, gamepad and audio crates (`winit`, `gilrs`, `kira`) have no
  console backends.
- **Certification.** Each platform holder checks saves, suspend and
  resume, user accounts and error messages before release. The engine
  does not meet those rules yet.

**Unblocks it:** a studio with platform-holder access, a GPU backend
abstraction (shared with the web work), and a private port layer for the
platform's window, input, audio and storage.
