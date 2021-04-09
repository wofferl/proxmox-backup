This is a clone of the repository https://git.proxmox.com/git/proxmox-backup.git with small changes used to compile for armbian64.

Use debian buster arm64 as base image.

## Install build essentials
```
 apt-get -y install \
   build-essential llvm clang git-core \
   lintian pkg-config quilt patch cargo \
   nodejs node-colors node-commander \
   libudev-dev libapt-pkg-dev \
   libacl1-dev libpam0g-dev libfuse3-dev \
   libsystemd-dev uuid-dev libssl-dev \
   libclang-dev libjson-perl libcurl4-openssl-dev \
   dh-exec liblocale-po-perl sudo
```

## Install ``rustup``
```
curl -sSf https://static.rust-lang.org/rustup.sh | sh -s 
source .cargo/env
```

## Install build dependencies
```
sudo apt -y build-dep $PWD/proxmox-backup
```

## Compile and install pve eslint
```
git clone https://git.proxmox.com/git/pve-eslint.git
cd pve-eslint 
make deb
sudo apt install ./pve-eslint_7.18.0-1_all.deb
```

## Compile and install dh-cargo 24
```
git clone https://git.proxmox.com/git/dh-cargo.git
git -C dh-cargo/ checkout -b v24 fc51977a114458e8214582d1410e5cbc95df6eee
cd dh-cargo
dpkg-buildpackage -us -uc -b
sudo apt install ../dh-cargo_24~bpo10+pve1_all.deb
```

## Checkout proxmox backup build dependencies
```
git clone https://git.proxmox.com/git/proxmox.git
git -C proxmox checkout -b v0.11.0 1fce0ff41ddeb177f92874bf4e95a775cfd99c69
git clone https://git.proxmox.com/git/proxmox-fuse.git
git -C proxmox-fuse checkout -b 0.1.1 0e0966af8886c176d8decfe18cb7ead4db5a83a6
git clone https://git.proxmox.com/git/pxar.git
git -C pxar checkout -b 0.10.1 82608859c8f949f9f527eeb891b42897bc2675a0
git clone https://git.proxmox.com/git/pathpatterns.git
git -C pathpatterns checkout -b 0.1.2 916e41c50e75a718ab7b1b95dc770eed9cd7a403
```

## Download rust crates
```
cargo vendor
```

## Build debian package
```
dpkg-buildpackage -b -us -uc
```


## Build other needed packages

```
###### pve-xtermjs
git clone https://github.com/wofferl/pve-xtermjs.git
cd pve-xtermjs && make deb

###### proxmox-mini-journalreader
git clone https://git.proxmox.com/git/proxmox-mini-journalreader.git
git -C proxmox-mini-journalreader checkout -b 1.1 7d75c26107561aa6108c0487875051bca6f85452
cd proxmox-mini-journalreader/ && make deb

###### proxmox-widget-toolkit
git clone https://git.proxmox.com/git/proxmox-widget-toolkit.git
git -C proxmox-widget-toolkit checkout -b 2.4 2d99e60eea3b59f2928854150c94989263e8d40b
cd proxmox-widget-toolkit/ && make deb

###### pbs-i18n
git clone https://git.proxmox.com/git/proxmox-i18n.git
cd proxmox-i18n/ && make deb

###### libjs-extjs
git clone https://git.proxmox.com/git/extjs.git
git -C extjs checkout -b 6.0.1 7e289e3bd34ee1078ecfe39f5fd52601c9faf90a
cd extjs/ && make deb

###### libjs-qrcodejs
git clone https://git.proxmox.com/git/libjs-qrcodejs.git
cd libjs-qrcodejs && make deb
```
