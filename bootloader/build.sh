#!/bin/sh
# Rebuilds bootloader.bin: ESP-IDF's own second-stage bootloader, unchanged, with app rollback
# turned on (sdkconfig.defaults). The CMake files and the empty app_main only exist because
# idf.py builds the bootloader as part of a project. Needs Docker; the image is pinned by digest.
set -e
cd "$(dirname "$0")"
IMG=espressif/idf@sha256:dfa2d076c796769c07c155eba6c672b9f395aec943b2ba3701b73379b5f9e884 # v5.5.1
docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$PWD:/project" -w /project "$IMG" \
    idf.py bootloader
cp build/bootloader/bootloader.bin bootloader.bin
rm -rf build sdkconfig
echo "bootloader.bin rebuilt"
