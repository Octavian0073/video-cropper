# video-cropper

Small Rust app for trimming and cropping a video. I got annoyed with the options on Arch (heavy editors, clunky GUIs, ffmpeg commands I had to look up every time), so I threw this together for myself. It's quick and a bit rough, but it does the one thing I need.

Open a video, drag the crop box, drag the trim handles, hit export.

## What it does

- Live preview with a scrubbable timeline (drag the white line to pick where playback starts)
- Drag the green handles to trim the start and end
- Drag the crop box edges and corners right on the video, or use the sliders
- Built-in file browser that starts in `~/Downloads`, with search-as-you-type
- Drag and drop a file onto the window
- Exports an H.264/AAC mp4 to `~/newpoker/<name>_cropped.mp4`

The preview is silent. The export keeps the audio.

## Requirements

- Rust (`sudo pacman -S rust`)
- ffmpeg (`sudo pacman -S ffmpeg`), used for decoding the preview and for the export

## Build and run

```bash
git clone https://github.com/YOUR_USERNAME/video-cropper.git
cd video-cropper
cargo run --release
```

## Run it from anywhere

Build it, then link the binary somewhere on your `PATH`:

```bash
cargo build --release
sudo ln -sf "$PWD/target/release/video-cropper" /usr/local/bin/cropper
```

Then just run `cropper &` from any folder. The symlink follows rebuilds, so after changing the code you only need to run `cargo build --release` again.

## Keys

| Key | Action |
| --- | --- |
| Space | Play / pause |
| I / O | Set trim start / end at the playhead |
| Left / Right | Step 1s (5s with Shift) |
| Esc | Quit |

## Notes

- Personal tool, so no promises. It works on my Arch setup.
- The output folder is hardcoded to `~/newpoker`. Change `OUT_SUBDIR` in `src/main.rs` if you want somewhere else.
- The window uses its own title bar. If that misbehaves on your WM, set `CUSTOM_TITLEBAR = false` in `src/main.rs`.
