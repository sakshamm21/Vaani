#!/usr/bin/env sh
# Download Parakeet TDT 0.6B v3 int8 for sherpa-onnx (about 640 MB) into ./models.
set -eu
cd "$(dirname "$0")"
mkdir -p models
name=sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8
if [ -d "models/$name" ]; then echo "already have models/$name"; exit 0; fi
url="https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/$name.tar.bz2"
echo "Downloading $url (about 640 MB, takes a minute or two)"
curl -fsSL "$url" | tar -xj -C models
ls -lh "models/$name"
