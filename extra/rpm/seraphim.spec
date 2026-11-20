Name:           seraphim
Version:        0.2.1
Release:        2%{?dist}
Summary:        GPU-accelerated, programmable terminal with animated, event-driven backgrounds

# Seraphim is MIT and its built-in JetBrains Mono font OFL-1.1. The rest are
# the crates compiled into the binary
# (cargo tree -p seraphim -e normal,build --format '{l}' on Linux targets).
License:        MIT AND OFL-1.1 AND Apache-2.0 AND BSD-2-Clause AND BSD-3-Clause AND CDLA-Permissive-2.0 AND ISC AND Unicode-3.0 AND Zlib AND (0BSD OR MIT OR Apache-2.0) AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR MIT) AND (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT) AND (BSD-2-Clause OR Apache-2.0 OR MIT) AND (BSD-3-Clause OR Apache-2.0) AND (MIT OR Apache-2.0 OR Zlib) AND (Unlicense OR MIT)
URL:            https://github.com/PercyThomas1127/Seraphim-terminal-emulator
Source0:        %{url}/archive/refs/tags/v%{version}/Seraphim-terminal-emulator-%{version}.tar.gz
# Every crate from Cargo.lock, so the build runs offline.
# Made by extra/rpm/make-vendor-tarball.sh and attached to the GitHub release.
Source1:        %{url}/releases/download/v%{version}/%{name}-%{version}-vendor.tar.xz

ExclusiveArch:  x86_64 aarch64

# Some dependencies (ordered-float, mlua, image) need Rust 1.90.
BuildRequires:  cargo >= 1.90
BuildRequires:  rust >= 1.90
# ring and the bundled Lua are C.
BuildRequires:  gcc
# tic compiles the xterm-seraphim terminfo entry.
BuildRequires:  ncurses
BuildRequires:  desktop-file-utils

# winit, wgpu and the clipboard load these with dlopen at run time, so
# rpm's automatic dependency scan cannot see them.
Requires:       vulkan-loader
Requires:       libglvnd-egl
Requires:       libxkbcommon
Requires:       libwayland-client
Requires:       libwayland-cursor
Requires:       libxkbcommon-x11
Requires:       libX11
Requires:       libXcursor
Requires:       libXi
Requires:       libXrandr
Requires:       hicolor-icon-theme

# Optional helpers Seraphim runs when a feature needs them.
Suggests:       /usr/bin/ffmpeg
Suggests:       /usr/bin/parec
Suggests:       /usr/bin/notify-send

%description
Seraphim is a GPU-accelerated terminal emulator with animated,
event-driven backgrounds: shader, image and video layers that react to
what happens in the terminal, such as entering a root shell or
installing a package. It is scriptable in Lua and includes shell
integration for bash, zsh and fish.

%prep
%autosetup -n Seraphim-terminal-emulator-%{version} -a 1
mkdir -p .cargo
cat > .cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

%build
export RUSTFLAGS="%{?build_rustflags}"
# The dist profile: release with whole-program optimization (fat LTO).
# Cargo.toml says so from the next release on; the 0.2.1 sources need it
# set here.
export CARGO_PROFILE_DIST_LTO=fat CARGO_PROFILE_DIST_CODEGEN_UNITS=1
cargo build --frozen --profile dist --package seraphim

%install
install -Dpm0755 target/dist/seraphim %{buildroot}%{_bindir}/seraphim
install -Dpm0644 extra/linux/seraphim.desktop %{buildroot}%{_datadir}/applications/seraphim.desktop
for size in 32 48 64 128 256 512; do
  install -Dpm0644 extra/icons/hicolor/${size}x${size}/apps/seraphim.png \
    %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/seraphim.png
done
mkdir -p %{buildroot}%{_datadir}/terminfo
tic -x -o %{buildroot}%{_datadir}/terminfo extra/terminfo/xterm-seraphim.terminfo
install -Dpm0644 -t %{buildroot}%{_datadir}/seraphim/shell-integration \
  shell-integration/seraphim.bash shell-integration/seraphim.fish shell-integration/seraphim.zsh

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/seraphim.desktop
export RUSTFLAGS="%{?build_rustflags}"
cargo test --frozen --release --workspace

%files
%license LICENSE
%license crates/seraphim-render/fonts/OFL.txt
%doc README.md docs
%{_bindir}/seraphim
%{_datadir}/applications/seraphim.desktop
%{_datadir}/icons/hicolor/*/apps/seraphim.png
%{_datadir}/terminfo/x/xterm-seraphim
%{_datadir}/seraphim/

%changelog
* Thu Nov 19 2026 Seraphim Dev <yc9941@stanford.edu> - 0.2.1-2
- Build with the dist profile (fat LTO from the next release on)

* Wed Nov 18 2026 Seraphim Dev <yc9941@stanford.edu> - 0.2.1-1
- Update to 0.2.1: built-in JetBrains Mono font, faster startup

* Sun Nov 15 2026 Seraphim Dev <yc9941@stanford.edu> - 0.1.1-1
- First RPM package
