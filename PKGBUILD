# Maintainer: flumpsi <flumpsi@outlook.com>
pkgname=deltatune
pkgver=0.1.0
pkgrel=1
pkgdesc="deltatune shows you what is playing using mpris in similar fashion to what DELTARUNE did once in chapter 1 when hopes and dreams started playing"
arch=('x86_64')
license=('MIT')
depends=('gtk3' 'libappindicator-gtk3' 'wayland' 'libxkbcommon')
makedepends=('cargo' 'pkgconf')
provides=('deltatune')
conflicts=('deltatune-bin' 'deltatune-git')
source=()
md5sums=()

build() {
  cd "$startdir"
  cargo build --release --locked
}

package() {
  cd "$startdir"
  install -Dm755 "target/release/deltatune_layershell" "$pkgdir/usr/bin/deltatune"
  install -Dm644 "../LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
