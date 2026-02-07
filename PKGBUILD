# Maintainer: flumpsi <flumpsi@outlook.com>
pkgname=deltatune
pkgver=0.1.0.r3.ge07b09d
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
  cd "$srcdir/deltatune/"
  cargo build --release --locked
}

package() {
  cd "$srcdir/deltatune/"
  install -Dm755 "target/release/deltatune_layershell" "$pkgdir/usr/bin/deltatune"
  install -Dm644 "$srcdir/deltatune/LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -d "$pkgdir/usr/share/deltatune"
  install -Dm644 "assets/MusicTitleFont.fnt" "$pkgdir/usr/share/deltatune/MusicTitleFont.fnt"
  install -Dm644 "assets/MusicTitleFont.png" "$pkgdir/usr/share/deltatune/MusicTitleFont.png"
}
