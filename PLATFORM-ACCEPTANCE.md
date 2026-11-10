# Native platform release candidate acceptance

The target matrix is Linux x64/ARM64 GNU (glibc 2.36+), macOS ARM64,
and Windows x64/ARM64 MSVC. Windows ARM64 is a native executable, without an
x64-emulation fallback. Containers and systemd examples remain Linux targets.

The GitHub-hosted runner matrix builds each executable on its native CPU:

| Target | Runner |
| --- | --- |
| Linux x64 | `ubuntu-24.04` (Bookworm build container) |
| Linux ARM64 | `ubuntu-24.04-arm` (Bookworm build container) |
| macOS ARM64 | `macos-15` |
| Windows x64 | `windows-2025` |
| Windows ARM64 | `windows-11-arm` |

PR validation produces executable/checksum/notice fragments for each target.
The native distribution validation run combines all five fragments after every
build passes, uploading `native-release-candidate`. Download that bundle or the individual artifact matching the test machine's OS and CPU.
These are candidate artifacts from the checked-out source; they are not an npm
publication or a deployment. To build additional candidates without publication,
once that workflow is available on main, run **Native macOS and Windows candidates**
with `workflow_dispatch` and select the candidate branch. The existing separately approved release workflow publishes all five
verified fragments from an exact committed version on main.

## Offline checks on each respective machine

Record OS version, CPU, `wx --json version` output and source commit. Use an empty
scratch directory, never an existing official profile or deployed daemon.

1. Run `node scripts/native-platform-smoke.mjs /absolute/path/to/wx` (use `wx.exe`
   on Windows). It checks authenticated lifecycle, stale generation rejection,
   duplicate start, graceful stop and restart identity without contacting WeChat.
2. Run `node scripts/native-package-smoke.mjs /absolute/path/to/artifact-directory`
   after installing repository dependencies. This stages an npm tarball, verifies
   offline installation and runs actual managed/external OpenClaw hosting.
3. Confirm installation and invocation from paths containing spaces. On Windows,
   check both PowerShell and cmd npm shims with an exact published candidate or a
   locally staged tarball. Check native ARM64 Node when selecting ARM64 artifacts.
4. Inspect Windows data/token/key ACLs: only the current user and SYSTEM may have
   allow entries. Deliberately widened ACLs and links/reparse points must fail
   admission. On Unix, verify owner-only directories and token/key files.
5. Check foreground Ctrl+C, background start/status/stop/logs and external mode.
   Confirm a foreign occupied listener and stale ownership record cannot cause
   another process to be adopted or stopped. Confirm data survives restart.

CI covers the automated offline checks. Keep native-machine results attached to
the PR before promotion; a successful cross-target compile alone is insufficient.

## Media decoder checks

WXGF still previews require FFmpeg on the daemon's PATH. Without it, decoding
reports `wxgf_decoder_unavailable`; retained original bytes are preserved. Check
the synthetic WXGF fixture with `cargo test -p wechat-media --locked` both with
and without FFmpeg installed on each target. Temporary decoder directories and
files use the shared private filesystem protections, including Windows ACLs.
All targets enforce the decoder deadline, output bounds and FFmpeg allocation/
thread limits. Unix additionally applies process resource limits; Windows does
not currently provide equivalent kernel-enforced memory/CPU limits.

## Isolated account acceptance

Use a dedicated test account and a disposable supported-format profile. Confirm
recipient and destination explicitly before any send. Record login, cold restart,
incoming text/media, durable events, read history and selected outgoing playback.
Install FFmpeg/FFprobe on the daemon host for media operations that need them.

Official-app profile takeover (`--official`) remains restricted to Linux. The
macOS and Windows adapters do not assert that those official apps' profile layouts
or saved credentials match the existing Linux-format adapter. Use a provisioned
supported-format profile for candidate runtime testing; never point an unvalidated
adapter at the only copy of an official macOS/Windows profile. Windows copy rejects
symbolic links, and non-Linux initialization refuses replacing an existing empty
profile directory because atomic directory exchange is not implemented there.
