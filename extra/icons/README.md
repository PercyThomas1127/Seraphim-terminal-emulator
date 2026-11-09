# Seraphim's icon

`seraphim.png` is the 32×32 original. It is pixel art and meant to look
blocky, so every size in `hicolor/` is an enlargement by whole pixels
(2×, 4×, 8×, 16×). 48 px is 1.5× by nearest pixel, which repeats every
other row and column. Nothing is smoothed.

There is no 16 px version: shrinking the art breaks the wings and the
halo, so desktops scale the 32 px one down instead. Draw a 16 px version by
hand if a crisp one is wanted (system tray, small lists).

Regenerate the sizes after changing `seraphim.png`:

```bash
for n in 32 48 64 128 256 512; do
  magick seraphim.png -filter point -resize ${n}x${n} -strip hicolor/${n}x${n}/apps/seraphim.png
done
```

`crates/seraphim/src/desktop.rs` embeds these files: the 64 px one is the
window icon, and `seraphim --install-desktop` installs all of them.
