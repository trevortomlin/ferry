# Ferry

[![CI](https://github.com/trevortomlin/ferry/actions/workflows/ci.yml/badge.svg)](https://github.com/trevortomlin/ferry/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/trevortomlin/ferry)](https://github.com/trevortomlin/ferry/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A fast SFTP and Amazon S3 browser for Linux, built on
[GPUI Fast](https://github.com/trevortomlin/gpui-fast). Ferry carries files
back and forth between your machine and your servers.

## Features

- **Bookmarks** in the sidebar, with a status dot per connection. Click to
  connect; right-click to edit, duplicate, disconnect or delete.
- **Tabs**: one per open connection, each with its own history (Alt+← / →).
- **SFTP**: SSH agent, password or key file, with known_hosts checking.
  **S3**: AWS profiles or access keys, plus custom endpoints (MinIO, R2,
  Wasabi…).
- **Secrets stay in the system keychain**, never in the bookmarks file. If one
  is missing, you're asked for it and can choose to remember it.
- **Fast listings**: 100,000 entries arrive in under 0.4 s over a local
  link, draw as they stream in, and scroll at the same frame cost as twenty.
- **Pipelined transfers**: up to 2 MiB in flight each way, so speed is set
  by the link, not by round trips.
- **Transfers drawer** with progress, speed, time left, cancel, retry and
  "Show in folder". Folders transfer whole, in both directions.
- **Conflicts** ask once: skip, keep both, continue an interrupted transfer,
  or replace, optionally for the rest of the batch.
- **Drag and drop**: drop files from your desktop onto the window, a folder
  row or a breadcrumb to upload there. Drag rows onto a folder or breadcrumb
  to move them.
- **Double-click a file** to download it to a cache and open it with the
  default app. Symbolic links to folders open as folders.
- Filter as you type (matches are highlighted), type-ahead to jump to a
  name, Ctrl+L to type a path (`~` is the server's home).
- Rename, new folder, delete (confirmed), Copy URL.
- Light and dark themes that follow the system; local time or UTC.

## Building

```sh
cargo build --release
./target/release/ferry
```

Bookmarks live in `~/.config/ferry/connections.toml`, settings beside them.
`FERRY_OPEN="<bookmark name>"` connects at startup;
`FERRY_LOG=ferry=debug` turns logging up.

## Keyboard

Press **F1** in the app for the full list.

| Key | Action |
| --- | --- |
| `Enter` / double-click | Open folder, or open file with its default app |
| `Backspace`, `Alt+↑` | Enclosing folder |
| `Alt+←` / `Alt+→` | Back / forward |
| `Ctrl+D`, `F3` / `Ctrl+Shift+D` | Download / download to… |
| `Ctrl+U` / `Ctrl+Shift+U` | Upload files / a folder |
| `F2` · `Delete` · `Ctrl+Shift+N` | Rename · delete · new folder |
| `Ctrl+F` · `Ctrl+L` | Filter · type a path |
| `Ctrl+N` · `Ctrl+W` · `Ctrl+Tab` | New connection · disconnect · next tab |
| `Ctrl+H` · `Ctrl+J` · `Ctrl+,` | Hidden files · transfers · settings |

## Performance

Measured on Linux with an NVIDIA GPU under Wayland, release build, against a
local OpenSSH `sftp-server`, with a 100,000-file directory and a 256 MiB
file. "Before" is the first working version, measured the same way before
any tuning.

| | Before | After | Change |
| --- | ---: | ---: | ---: |
| **Real window** (`script/measure.py`) | | | |
| Start → first frame | 138 ms | 138 ms | no change |
| Idle CPU, nothing open | 0.2–0.4 % | 0–0.2 % | ≈ 0 (one clock tick) |
| Threads, idle | 27 | 23 | −4 |
| Resident memory, idle | 108.3 MB | 106.4 MB | −1.9 MB |
| Heap and other anonymous memory, idle | 22.5 MB | 22.4 MB | no change |
| Open a 100k-entry folder, end to end | 1,027 ms | 376 ms | **2.7× faster** |
| Process CPU, start until the folder is loaded | 0.44 s | 0.33 s | −25 % |
| Resident memory with the folder open | 121.9 MB | 118.1 MB | −3.8 MB |
| Anonymous memory with the folder open | 34.2 MB | 29.8 MB | −4.4 MB (−38 % of what the listing costs) |
| UI-thread CPU per frame, scrolling 100k entries | 1,302 µs | 1,064 µs | −18 % |
| …95th percentile | 1,445 µs | 1,185 µs | −18 % |
| Idle CPU with the folder open | 0–0.2 % | 0–0.2 % | no change |
| **Protocol** (`examples/sftp_bench.rs`) | | | |
| List 100k entries | 1,050 ms | 354 ms | **3.0× faster** |
| Download 256 MiB | 21 MB/s | 580 MB/s | **28× faster** |
| CPU to download it | 754 ms | 690 ms | −8 % |
| Upload 256 MiB | 166 MB/s | 481 MB/s | **2.9× faster** |
| CPU to upload it | 684 ms | 557 ms | −19 % |
| Peak memory through both transfers | 14.4 MB | 14.1 MB | no change |

What changed:

- **Pipelined SFTP.** Listing had one `READDIR` in flight, so 100k entries
  cost about 1,000 sequential round trips. It now keeps 8 in flight; batches
  can return in any order because the list is sorted on arrival. Downloads
  and uploads moved one 32 KiB block per round trip; they now keep 64 in
  flight (2 MiB), write in order, and fill short reads before moving on.
  Even over loopback that was a 28× difference for downloads. Over a real
  link with latency, the gap is larger.
- **Cached panels.** The sidebar, tab strip, toolbar, status bar and
  transfers drawer are separate GPUI views, reused from the previous frame
  unless a signature of what they show changes. Scrolling and selecting
  rebuild only the list.
- **Targeted redraws.** Transfer progress (about 20 Hz) and listing chunks
  that don't change the visible rows refresh only the drawer and status bar,
  instead of the whole window and the file list.
- **Nothing starts before it's needed.** The engine thread and its tokio
  runtime start with the first connection. The keychain probe (a D-Bus
  connection with threads of its own) and the SSH agent query run the first
  time a secret or the connection editor needs them.
- **Leaner listings.** Timestamps are stored as `u32` and modes as `u16`
  (−6 bytes per entry). A finished listing drops the slack from growing
  while chunks arrived, and freed heap is returned to the system after big
  listings and after startup (`malloc_trim`).
- **Fewer per-row allocations.** A row's name is allocated once and shared
  between the painted row and its drag payload. Full paths are built only
  for folders (drop targets); a dragged row's path is resolved when it's
  dropped.

What's left:

- The rest of a scroll frame is GPUI's per-glyph and per-primitive work.
  Removing all row interactivity as an experiment saved only 8%, and the
  four secondary columns' text costs 15%. At about 1 ms the list uses
  roughly 6% of a 60 Hz frame.
- The idle heap is mostly the framework's: the font index for every
  installed face (1,016 here), shaders and glyph caches. Resident memory is
  dominated by the GPU driver and shared libraries.

To reproduce:

```sh
script/measure.py target/release/ferry HOST:PORT:USER:KEY:/path/to/100k/dir
script/threads.py target/release/ferry HOST:PORT:USER:KEY:/path/to/100k/dir   # CPU per thread
cargo run --release --example sftp_bench -- HOST:PORT:USER:KEY BIG_DIR BIG_FILE
FERRY_OPEN=<bookmark> FERRY_SCROLL_PROBE=600 ./target/release/ferry          # frame cost
cargo test --release --test large_directory -- --nocapture                    # data layer
```

The measurement scripts run the app with a throwaway `HOME` and config, so
they never touch your bookmarks, known_hosts or keychain.

## Layout

One package: a library for everything that isn't drawing, and the GPUI app
on top of it.

```
src/model/     plain data: bookmarks, listings, sorting, selection, transfer queue
src/remote/    SFTP and S3 providers
src/secrets/   system keychain, SSH agent, turning a bookmark into credentials
src/engine/    the background thread that owns every socket, and folder walks
src/ui/        the interface (binary)
  workspace/   the window: list, panels, dialogs, menus
tests/         data-layer benchmark and live server tests
examples/      protocol benchmark
script/        real-window measurements
```

The UI never awaits the network. The engine and keychain threads wake the UI
through a channel, and a foreground task drains their answers. Each listing
request carries a generation, so a slow reply for a folder you've left is
dropped instead of drawn.

`src/ui/text_input.rs` is adapted from GPUI's `examples/input.rs` (Zed
Industries, Apache-2.0), with a masked mode for passwords.

## Testing

```sh
cargo test
```

Screenshots of made-up state, without opening a window:

```sh
cargo run --release --features snapshot -- --snapshot out.png list [dark|light] [size=WxH]
# scenarios: welcome list menu filter conflict big empty failed editor editor-s3
#            secret settings help delete
```

An end-to-end check against a real SFTP server. It connects, lists, makes,
renames and deletes a folder, downloads a file and a folder, uploads,
follows a symlink and filters, then compares the results on disk:

```sh
FERRY_LIVE=host:port:user:/path/to/key:/remote/dir:/local/view/of/dir \
  cargo run --features snapshot -- --snapshot out.png live
```

`testing/` has Docker Compose files for an SFTP server and MinIO. Any local
`sshd` started with its own config works too.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Report security issues privately, as
described in [SECURITY.md](SECURITY.md).

## License

MIT
