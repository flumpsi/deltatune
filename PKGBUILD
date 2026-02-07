# Maintainer: flumpsi <flumpsi@outlook.com>
pkgname=deltatune
pkgver=0.1.0.r0.g0000000
pkgrel=1
pkgdesc="deltatune shows you what is playing using mpris in similar fashion to what DELTARUNE did once in chapter 1 when hopes and dreams started playing"
arch=('x86_64')
license=('MIT')
depends=('gtk3' 'libappindicator-gtk3' 'wayland' 'libxkbcommon')
makedepends=('cargo' 'pkgconf' 'git')
provides=('deltatune')
conflicts=('deltatune-bin' 'deltatune-git')
source=("deltatune::git+https://github.com/flumpsi/deltatune.git")
md5sums=('SKIP')

pkgver() {
  cd "$srcdir/deltatune"
  local count=$(git rev-list --count HEAD)
  local commit=$(git rev-parse --short HEAD)
  printf "0.1.0.r%s.g%s" "$count" "$commit"
}

build() {
  cd "$srcdir/deltatune/deltatune-layershell"
  cargo build --release --locked
}

package() {
  cd "$srcdir/deltatune/deltatune-layershell"
  install -Dm755 "target/release/deltatune_layershell" "$pkgdir/usr/bin/deltatune"
  install -Dm644 "$srcdir/deltatune/LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
