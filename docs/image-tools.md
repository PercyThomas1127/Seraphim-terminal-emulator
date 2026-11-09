# Image tools tested in Seraphim

Run in a real Seraphim window (Xvfb, llvmpipe), November 2026.

| Tool | Version | Protocol | Result |
|---|---|---|---|
| `kitten icat` | kitty 0.32.2 | Kitty graphics, PNG in chunks | Works (after a fix, below) |
| `kitten icat` animated GIF | kitty 0.32.2 | Kitty animation frames | Plays |
| `kitten icat` in tmux 3.4 | kitty 0.32.2 | Kitty via passthrough + Unicode placeholders | Works with `TERM=xterm-seraphim` (below) |
| `chafa -f kitty` | chafa 1.14 | Kitty graphics | Works |
| `chafa -f sixel` | chafa 1.14 | Sixel | Works; decodes like ImageMagick |
| `img2sixel` | libsixel 1.10 | Sixel | Works |
| `timg -pk` / `timg -ps` | timg 1.6 | Kitty / Sixel, with transparency | Works |

![The tools in Seraphim](images/image-tools.png)

## What testing found

* **Chunked uploads from `kitten icat` were dropped.** icat repeats
  `a=T,q=2` on every continuation chunk. Seraphim took each of those for a
  new image, so any picture large enough to need chunks vanished. A
  transmit that names no other image now continues the upload, as kitty
  does. The recorded stream is a regression test
  (`tests/fixtures/kitten-icat-two-images.bin`).
* **Images inside tmux didn't show.** With Unicode placeholders, the
  image id travels in the placeholder's foreground color. tmux only passes
  24-bit color through when the outer terminal's terminfo declares it, and
  `xterm-256color` doesn't, so tmux turned the color into a 256-color
  approximation and the id was lost. Seraphim now ships an
  `xterm-seraphim` terminfo entry (24-bit color, undercurl, underline
  color, cursor shapes, OSC 52, synchronized output, Kitty keyboard).
  * `seraphim --install-terminfo` installs it, and new sessions then use
    it (`terminal.term = "auto"`).
  * With it, tmux detects RGB on its own and images work through tmux
    with only `set -g allow-passthrough on`.
  * Without installing it, `set -as terminal-features ",xterm-256color:RGB"`
    in tmux.conf does the same.
* chafa pads its Sixel images with an edge column of its own; ImageMagick
  decodes the same column, so that is chafa's output, not Seraphim's. The
  recorded stream checks Seraphim's decoder against ImageMagick's pixels.
