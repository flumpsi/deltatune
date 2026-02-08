# deltatune rewritten in rust and also linux only

## how to install the package
there is currently only a package for arch-based systems only.
to install the package git clone the repo, then run
```makepkg -si``` in the root of the repo.
if install fails please make an issue and send me the error log.

## how to compile (NOT RECOMMENDED, ASSETS BREAK)

to compile just git clone the repo, then run
```cargo build --release``` in the root.
the executables are now in target/release
