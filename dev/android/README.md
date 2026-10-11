# dev/android/

Mobile (Android / iOS) build steps. Grouped here so every reproducible
build/package step lives under `dev/`. The authoritative Android flow is
documented in [`apps/acowork-mobile/README.md`](../../apps/acowork-mobile/README.md);
this folder adds one automation on top of it: [`sign_release.ps1`](sign_release.ps1)
drives the full release-signing + APK build interactively.

## sign_release.ps1 (Windows)

One-stop flow: locate `keytool` → create/reuse the keystore → write the
plaintext `gradle.properties` → build the signed release APK. Every location is
prompted with the manual-flow defaults (Enter = default):

- keystore: `$HOME/acowork-mobile.keystore`
- gradle.properties: `$HOME/.gradle/gradle.properties`

```powershell
# interactive (recommended first run)
.\dev\android\sign_release.ps1

# non-interactive overrides + auto-build
.\dev\android\sign_release.ps1 -Build
```

The script writes `acowork.release.{storeFile,storePassword,keyAlias,keyPassword}`
into `gradle.properties` (preserving unrelated lines) and the empty-safe signing
block in `src-tauri/gen/android/app/build.gradle.kts` reads them via
`findProperty`. Passwords are typed by the operator and stored in plaintext in
that local file only — never printed, never committed. If you point
`gradle.properties` inside the repo, the script warns and asks to confirm.

Flags: `-StorePath`, `-GradlePropsPath`, `-KeytoolPath`, `-Alias`, `-KeySize`,
`-Validity`, `-Build` / `-NoBuild`.

## Prerequisites (per machine)

- Android Studio (SDK + NDK), JDK 17+
- `rustup target add aarch64-linux-android`
- `ANDROID_HOME` / `NDK_HOME` exported
- `@tauri-apps/api` and the Rust `tauri` crate aligned on the same minor
- On Windows: **Developer Mode** must be on. The Tauri CLI symlinks the `.so`
  into `jniLibs`, and non-admin processes can't create symlinks without it
  (`Creation symbolic link is not allowed for this system`). `sign_release.ps1`
  probes this before building and offers to enable it via a UAC-elevated
  `reg add`. To do it manually:
  ```powershell
  Start-Process pwsh -Verb RunAs -Wait -ArgumentList '-NoProfile','-Command',
    'reg add "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock" /t REG_DWORD /f /v AllowDevelopmentWithoutDevLicense /d 1'
  ```
  The authoritative Android flow + the full pitfall list live in
  [`apps/acowork-mobile/README.md`](../../apps/acowork-mobile/README.md).
