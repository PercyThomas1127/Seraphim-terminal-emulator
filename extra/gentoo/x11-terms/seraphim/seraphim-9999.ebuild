# Copyright 2026 Gentoo Authors
# Distributed under the terms of the GNU General Public License v2

EAPI=8

# Some dependencies (ordered-float, mlua, image) need Rust 1.90.
RUST_MIN_VER="1.90.0"

inherit cargo desktop git-r3 xdg optfeature

DESCRIPTION="GPU-accelerated, programmable terminal with animated, event-driven backgrounds"
HOMEPAGE="https://github.com/PercyThomas1127/Seraphim-terminal-emulator"
EGIT_REPO_URI="https://github.com/PercyThomas1127/Seraphim-terminal-emulator.git"

LICENSE="|| ( Apache-2.0 MIT )"
# Dependent crate licenses (keep in sync with the versioned ebuild)
LICENSE+="
	Apache-2.0 BSD-2 BSD Boost-1.0 CDLA-Permissive-2.0 ISC MIT
	Unicode-3.0 ZLIB
"
SLOT="0"
IUSE="wayland X"
REQUIRED_USE="|| ( wayland X )"

# winit, wgpu and the clipboard load every system library with dlopen at
# run time, so nothing below is linked and nothing is needed to build.
RDEPEND="
	media-libs/vulkan-loader
	media-libs/libglvnd
	x11-libs/libxkbcommon
	wayland? ( dev-libs/wayland )
	X? (
		x11-libs/libX11
		x11-libs/libXcursor
		x11-libs/libXi
		x11-libs/libXrandr
		x11-libs/libxkbcommon[X]
	)
"
# tic compiles the xterm-seraphim terminfo entry.
BDEPEND="sys-libs/ncurses"

DOCS=( README.md )

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

src_compile() {
	cargo_src_compile --package seraphim
}

src_test() {
	# Renderer pixel tests probe for a GPU and skip without one; keep the
	# probe from tripping the sandbox.
	addpredict /dev/dri
	cargo_src_test --workspace
}

src_install() {
	cargo_src_install --path crates/seraphim

	domenu extra/linux/seraphim.desktop
	local size
	for size in 32 48 64 128 256 512; do
		doicon -s ${size} extra/icons/hicolor/${size}x${size}/apps/seraphim.png
	done

	dodir /usr/share/terminfo
	tic -x -o "${ED}"/usr/share/terminfo extra/terminfo/xterm-seraphim.terminfo || die

	insinto /usr/share/seraphim/shell-integration
	doins shell-integration/seraphim.{bash,fish,zsh}

	einstalldocs
	dodoc -r docs
}

pkg_postinst() {
	xdg_pkg_postinst

	elog "For prompt marks, exit codes and command events, source the shell"
	elog "integration from your shell's rc file, e.g. for bash:"
	elog "  [[ \$TERM_PROGRAM == seraphim ]] && source ${EPREFIX}/usr/share/seraphim/shell-integration/seraphim.bash"

	optfeature "video backgrounds and audio-reactive visuals" media-video/ffmpeg
	optfeature "audio capture for audio-reactive visuals" media-libs/libpulse
	optfeature "desktop notifications" x11-libs/libnotify dev-libs/glib
}
