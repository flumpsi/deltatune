# deltatune rewritten in rust and also linux only

## if you know rust read this!
i cant really maintain this project too much, so if anyone can take over ownership that would be great.

## what is this??
deltatune shows you what is currently playing using mpris in the same fashion as DELTARUNE did once in chapter 1 when the field of hopes and dreams started playing
this is really just a rewrite of an already existing program https://github.com/ToadsworthLP/deltatune which is more maintained than this.

## how to install the package
there is currently only a package for arch-based systems only.
to install the package git clone the repo, then run
```
makepkg -si
```
in the root of the repo.
if install fails please make an issue and send me the error log.

## how to compile (NOT RECOMMENDED, ASSETS BREAK)

to compile just git clone the repo, then run
```
cargo build --release
```
in the root.
the executables are now in target/release
