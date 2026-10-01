# Maintainer: DevInBlack001 <53265844+DevInBlack001@users.noreply.github.com>
#
# This is one install path among several (see scripts/install.sh for the
# general, non-Arch-specific route); the project targets Linux broadly,
# not Arch exclusively.
pkgname=lofi-launcher-terminal
pkgver=0.1.0
pkgrel=1
pkgdesc="Play mood-based lofi music in the background whenever a terminal or TTY session is open"
arch=('x86_64' 'aarch64')
url="https://github.com/DevInBlack001/lofi-launcher-terminal"
license=('MIT')
depends=('mpv')
optdepends=('mpv-mpris: expose now-playing track info over MPRIS for widgets like quickshell or playerctl')
makedepends=('cargo')
source=("$pkgname-$pkgver.tar.gz::https://github.com/DevInBlack001/lofi-launcher-terminal/archive/v$pkgver.tar.gz")
sha256sums=('SKIP')

build() {
    cd "$pkgname-$pkgver"
    cargo build --release --locked
}

package() {
    cd "$pkgname-$pkgver"
    install -Dm755 target/release/lofi "$pkgdir/usr/bin/lofi"
    install -Dm755 target/release/lofi-daemon "$pkgdir/usr/bin/lofi-daemon"
    install -Dm644 config.default.toml "$pkgdir/usr/share/lofi-launcher-terminal/config.default.toml"
    install -Dm644 scripts/lofi-launcher.sh.in "$pkgdir/usr/share/lofi-launcher-terminal/lofi-launcher.sh.in"
}
