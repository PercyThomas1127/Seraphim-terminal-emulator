# Copyright 2026 Gentoo Authors
# Distributed under the terms of the GNU General Public License v2

EAPI=8

CRATES="
	ab_glyph@0.2.32
	ab_glyph_rasterizer@0.1.10
	adler2@2.0.1
	ahash@0.8.12
	aho-corasick@1.1.5
	alacritty_terminal@0.26.0
	allocator-api2@0.2.21
	android-activity@0.6.1
	android-properties@0.2.2
	android_system_properties@0.1.6
	arboard@3.7.0
	arrayref@0.3.9
	arrayvec@0.7.8
	as-raw-xcb-connection@1.0.1
	ash@0.38.0+1.3.281
	atomic-waker@1.1.2
	autocfg@1.5.1
	base64@0.22.1
	bit-set@0.10.0
	bit-vec@0.9.1
	bitflags@1.3.2
	bitflags@2.13.2
	block2@0.5.1
	block2@0.6.2
	bstr@1.13.1
	bumpalo@3.20.3
	bytemuck@1.25.2
	bytemuck_derive@1.12.1
	byteorder-lite@0.1.0
	bytes@1.12.1
	calloop-wayland-source@0.3.0
	calloop@0.13.0
	cc@1.11.1
	cfg-if@1.0.5
	cfg_aliases@0.2.2
	clipboard-win@5.4.3
	codespan-reporting@0.13.1
	color_quant@1.1.0
	combine@4.6.9
	concurrent-queue@2.5.0
	core-foundation-sys@0.8.7
	core-foundation@0.9.4
	core-graphics-types@0.1.3
	core-graphics@0.23.2
	crc32fast@1.5.4
	crossbeam-utils@0.8.23
	crunchy@0.2.4
	cursor-icon@1.2.0
	dispatch2@0.3.1
	dispatch@0.2.0
	displaydoc@0.2.7
	dlib@0.5.3
	document-features@0.2.12
	downcast-rs@1.2.1
	dpi@0.1.2
	either@1.19.0
	equivalent@1.0.3
	erased-serde@0.4.11
	errno@0.3.14
	error-code@3.4.0
	fastrand@2.5.0
	fdeflate@0.3.7
	find-msvc-tools@0.1.15
	fixedbitset@0.5.7
	flate2@1.1.11
	foldhash@0.1.5
	foldhash@0.2.0
	font-types@0.12.7
	fontconfig-parser@0.5.8
	fontdb@0.24.0
	foreign-types-macros@0.2.4
	foreign-types-shared@0.3.1
	foreign-types@0.5.0
	form_urlencoded@1.2.2
	futures-core@0.3.34
	futures-io@0.3.34
	futures-task@0.3.34
	futures-util@0.3.34
	gethostname@1.1.0
	getrandom@0.2.17
	getrandom@0.3.4
	getrandom@0.4.3
	gif@0.14.2
	gl_generator@0.14.0
	glow@0.17.0
	glutin_wgl_sys@0.6.1
	gpu-allocator@0.28.0
	half@2.7.1
	hashbrown@0.15.5
	hashbrown@0.16.1
	hashbrown@0.17.1
	hermit-abi@0.5.3
	home@0.5.12
	icu_collections@2.4.0
	icu_locale_core@2.4.0
	icu_normalizer@2.4.0
	icu_normalizer_data@2.4.0
	icu_properties@2.4.0
	icu_properties_data@2.4.0
	icu_provider@2.4.0
	idna@1.1.0
	idna_adapter@1.2.2
	image-webp@0.2.4
	image@0.25.10
	indexmap@2.14.2
	itoa@1.0.18
	jni-macros@0.22.4
	jni-sys-macros@0.4.1
	jni-sys@0.3.1
	jni-sys@0.4.1
	jni@0.22.4
	jobserver@0.1.35
	js-sys@0.3.107
	khronos-egl@6.0.0
	khronos_api@3.1.0
	libc@0.2.191
	libloading@0.8.9
	libm@0.2.16
	libredox@0.1.25
	linux-raw-sys@0.12.1
	linux-raw-sys@0.4.15
	litemap@0.8.3
	litrs@1.0.1
	lock_api@0.4.14
	log@0.4.35
	lua-src@551.0.2
	luajit-src@210.7.4+c6ffc14
	memchr@2.8.3
	memmap2@0.9.12
	miniz_oxide@0.8.9
	miniz_oxide@0.9.1
	miow@0.6.1
	mlua-sys@0.13.0
	mlua@0.12.2
	moxcms@0.8.1
	naga-types@30.0.1
	naga@30.0.1
	ndk-context@0.1.1
	ndk-sys@0.6.0+11769913
	ndk@0.9.0
	nix@0.31.3
	nom@8.0.0
	num-traits@0.2.19
	num_enum@0.7.6
	num_enum_derive@0.7.6
	objc-sys@0.3.5
	objc2-app-kit@0.2.2
	objc2-app-kit@0.3.2
	objc2-cloud-kit@0.2.2
	objc2-contacts@0.2.2
	objc2-core-data@0.2.2
	objc2-core-foundation@0.3.2
	objc2-core-graphics@0.3.2
	objc2-core-image@0.2.2
	objc2-core-location@0.2.2
	objc2-encode@4.1.0
	objc2-foundation@0.2.2
	objc2-foundation@0.3.2
	objc2-io-surface@0.3.2
	objc2-link-presentation@0.2.2
	objc2-metal@0.2.2
	objc2-metal@0.3.2
	objc2-quartz-core@0.2.2
	objc2-quartz-core@0.3.2
	objc2-symbols@0.2.2
	objc2-ui-kit@0.2.2
	objc2-uniform-type-identifiers@0.2.2
	objc2-user-notifications@0.2.2
	objc2@0.5.3
	objc2@0.6.5
	once_cell@1.21.4
	orbclient@0.3.55
	ordered-float@2.10.1
	ordered-float@5.5.1
	os_pipe@1.2.3
	owned_ttf_parser@0.25.1
	parking_lot@0.12.5
	parking_lot_core@0.9.12
	percent-encoding@2.3.2
	petgraph@0.8.3
	pin-project-internal@1.1.13
	pin-project-lite@0.2.17
	pin-project@1.1.13
	piper@0.2.5
	pkg-config@0.3.34
	plain@0.2.3
	png@0.18.1
	polling@3.11.0
	pollster@1.0.1
	portable-atomic-util@0.2.8
	portable-atomic@1.15.0
	potential_utf@0.1.6
	presser@0.3.1
	proc-macro-crate@3.5.0
	proc-macro2@1.0.107
	profiling@1.0.18
	pxfm@0.1.30
	quick-error@2.0.1
	quick-xml@0.41.0
	quote@1.0.47
	r-efi@5.3.0
	r-efi@6.0.0
	range-alloc@0.1.5
	raw-window-handle@0.6.2
	raw-window-metal@1.1.0
	read-fonts@0.41.0
	redox_syscall@0.4.1
	redox_syscall@0.5.18
	redox_syscall@0.9.5
	regex-automata@0.4.18
	regex-syntax@0.8.11
	renderdoc-sys@1.1.0
	ring@0.17.14
	roxmltree@0.20.0
	rustc-hash@1.1.0
	rustc-hash@2.1.3
	rustc_version@0.4.1
	rustix-openpty@0.2.0
	rustix@0.38.44
	rustix@1.1.6
	rustls-pki-types@1.15.1
	rustls-webpki@0.103.16
	rustls@0.23.46
	rustversion@1.0.23
	same-file@1.0.6
	scoped-tls@1.0.1
	scopeguard@1.2.0
	sctk-adwaita@0.10.1
	semver@1.0.28
	serde-value@0.7.0
	serde@1.0.229
	serde_core@1.0.229
	serde_derive@1.0.229
	serde_json@1.0.154
	serde_spanned@1.1.3
	shlex@2.0.1
	signal-hook-registry@1.4.8
	signal-hook@0.4.6
	simd-adler32@0.3.10
	simd_cesu8@1.2.0
	simdutf8@0.1.5
	skrifa@0.44.0
	slab@0.4.12
	slotmap@1.1.1
	smallvec@1.16.5
	smithay-client-toolkit@0.19.2
	smol_str@0.2.2
	spirv@0.4.0+sdk-1.4.341.0
	stable_deref_trait@1.2.1
	static_assertions@1.1.0
	strict-num@0.1.1
	subtle@2.6.1
	swash@0.2.10
	syn@2.0.119
	syn@3.0.9
	synstructure@0.14.0
	termcolor@1.4.1
	thiserror-impl@1.0.69
	thiserror-impl@2.0.21
	thiserror@1.0.69
	thiserror@2.0.21
	tiny-skia-path@0.11.4
	tiny-skia@0.11.4
	tinystr@0.8.5
	tinyvec@1.13.3
	tokio@1.54.1
	toml@1.2.1+spec-1.1.0
	toml_datetime@1.1.3+spec-1.1.0
	toml_edit@0.25.19+spec-1.1.0
	toml_parser@1.1.5+spec-1.1.0
	toml_writer@1.2.0+spec-1.1.0
	tracing-core@0.1.36
	tracing@0.1.44
	tree_magic_mini@3.2.2
	ttf-parser@0.25.1
	typeid@1.0.3
	unicode-ident@1.0.27
	unicode-normalization@0.1.26
	unicode-segmentation@1.13.3
	unicode-width@0.2.3
	untrusted@0.9.0
	ureq@2.12.1
	url@2.5.8
	utf8_iter@1.1.0
	utf_types@1.0.0
	version_check@0.9.5
	vte@0.15.0
	walkdir@2.5.0
	wasi@0.11.1+wasi-snapshot-preview1
	wasip2@1.0.4+wasi-0.2.12
	wasm-bindgen-futures@0.4.80
	wasm-bindgen-macro-support@0.2.130
	wasm-bindgen-macro@0.2.130
	wasm-bindgen-shared@0.2.130
	wasm-bindgen@0.2.130
	wayland-backend@0.3.17
	wayland-client@0.31.15
	wayland-csd-frame@0.3.0
	wayland-cursor@0.31.14
	wayland-protocols-plasma@0.3.12
	wayland-protocols-wlr@0.3.12
	wayland-protocols@0.32.13
	wayland-scanner@0.31.11
	wayland-sys@0.31.11
	web-sys@0.3.107
	web-time@1.1.0
	webpki-roots@0.26.11
	webpki-roots@1.0.11
	weezl@0.1.12
	wgpu-core-deps-apple@30.0.1
	wgpu-core-deps-emscripten@30.0.1
	wgpu-core-deps-windows-linux-android@30.0.1
	wgpu-core@30.0.1
	wgpu-hal@30.0.1
	wgpu-naga-bridge@30.0.1
	wgpu-types@30.0.1
	wgpu@30.0.1
	which@8.0.6
	winapi-util@0.1.11
	windows-collections@0.3.2
	windows-core@0.62.2
	windows-future@0.3.2
	windows-implement@0.60.2
	windows-interface@0.59.3
	windows-link@0.2.1
	windows-numerics@0.3.1
	windows-result@0.4.1
	windows-strings@0.5.1
	windows-sys@0.52.0
	windows-sys@0.59.0
	windows-sys@0.61.2
	windows-targets@0.52.6
	windows-threading@0.2.1
	windows@0.62.2
	windows_aarch64_gnullvm@0.52.6
	windows_aarch64_msvc@0.52.6
	windows_i686_gnu@0.52.6
	windows_i686_gnullvm@0.52.6
	windows_i686_msvc@0.52.6
	windows_x86_64_gnu@0.52.6
	windows_x86_64_gnullvm@0.52.6
	windows_x86_64_msvc@0.52.6
	winit@0.30.13
	winnow@1.0.5
	wit-bindgen@0.57.1
	wl-clipboard-rs@0.9.5
	writeable@0.6.4
	x11-dl@2.21.0
	x11rb-protocol@0.13.2
	x11rb@0.13.2
	xcursor@0.3.11
	xkbcommon-dl@0.4.2
	xkeysym@0.2.1
	xml-rs@0.8.29
	yazi@0.2.2
	yoke-derive@0.8.4
	yoke@0.8.4
	zeno@0.3.3
	zerocopy-derive@0.8.63
	zerocopy@0.8.63
	zerofrom-derive@0.1.8
	zerofrom@0.1.8
	zeroize@1.9.1
	zerotrie@0.2.5
	zerovec-derive@0.11.7
	zerovec@0.11.9
	zlib-rs@0.6.8
	zmij@1.0.23
	zune-core@0.5.3
	zune-jpeg@0.5.15
"

# Some dependencies (ordered-float, mlua, image) need Rust 1.90.
RUST_MIN_VER="1.90.0"

inherit cargo desktop xdg optfeature

DESCRIPTION="GPU-accelerated, programmable terminal with animated, event-driven backgrounds"
HOMEPAGE="https://github.com/PercyThomas1127/Seraphim-terminal-emulator"
SRC_URI="
	${HOMEPAGE}/archive/refs/tags/v${PV}.tar.gz -> ${P}.tar.gz
	${CARGO_CRATE_URIS}
"
S="${WORKDIR}/Seraphim-terminal-emulator-${PV}"

# The built-in JetBrains Mono font is OFL-1.1.
LICENSE="MIT OFL-1.1"
# Dependent crate licenses
LICENSE+="
	Apache-2.0 BSD-2 BSD Boost-1.0 CDLA-Permissive-2.0 ISC MIT
	Unicode-3.0 ZLIB
"
SLOT="0"
KEYWORDS="~amd64 ~arm64"
IUSE="+wayland +X"
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

# cargo.eclass always builds the release profile; give it the dist
# profile's whole-program optimization (fat LTO, one codegen unit) for
# the installed binary. Tests keep the faster plain release build.
seraphim_lto() {
	CARGO_PROFILE_RELEASE_LTO=fat CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 "$@"
}

src_compile() {
	seraphim_lto cargo_src_compile --package seraphim
}

src_test() {
	# Renderer pixel tests probe for a GPU and skip without one; keep the
	# probe from tripping the sandbox.
	addpredict /dev/dri
	cargo_src_test --workspace
}

src_install() {
	seraphim_lto cargo_src_install --path crates/seraphim

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
