# Third-party notices

Optional **Moondream Parakeet Ultra** weights are a post-trained version of NVIDIA Parakeet TDT 0.6B v3, distributed under CC BY 4.0. Model and attribution: https://huggingface.co/moondream/parakeet-ultra. On request, Echo downloads pinned embedded Python (PSF license), PyTorch and its bundled CUDA libraries, and Photon/Kestrel wheels directly from their upstream distribution sites into a private GPU runtime. Echo does not host or redistribute a prebuilt runtime bundle for new installations. Package notices and supplied licenses are preserved in that runtime's `THIRD_PARTY_NOTICES.txt`, `LICENSE.txt`, and `lib/` package directories; applicable upstream license terms still apply. Neither the weights nor the GPU runtime is included in the main installer.

Echo can download an ONNX conversion of **NVIDIA Parakeet TDT 0.6B v2** on the user's request. The model is not included in the installer. It is licensed under CC BY 4.0. Original model: `nvidia/parakeet-tdt-0.6b-v2`. ONNX conversion and runtime integration: `k2-fsa/sherpa-onnx`.

The local runtime also contains sherpa-onnx (Apache License 2.0), ONNX Runtime and its transitive native components, and uses Silero VAD (MIT) when its separately downloaded model is enabled. The corresponding license texts are installed alongside this notice.

Optional post-processing downloads the INT8 **Edge-Punct-Casing English punctuation and capitalization model**, distributed by k2-fsa/sherpa-onnx (2024-08-06 package). It is not included in the installer. Upstream model and Apache License 2.0: https://github.com/frankyoujian/Edge-Punct-Casing; conversion: https://github.com/k2-fsa/sherpa-onnx/releases/tag/punctuation-models. The Apache 2.0 license text is included in `licenses/sherpa-onnx-Apache-2.0.txt`. English written-form rules run locally in Rust using text2num 2.8.0 (MIT).

`RUST_THIRD_PARTY_NOTICES.txt` contains the locked Rust dependency inventory and the license or notice files shipped by those package sources.

Pitch-preserving speech tempo processing embeds **Signalsmith Stretch** and
**Signalsmith Linear** (MIT), through the `signalsmith-stretch` Rust wrapper.
The native libraries are compiled into Echo and need no separate installation.
Their license texts are included as `signalsmith-stretch-MIT.txt` and
`signalsmith-linear-MIT.txt`. Upstream: https://github.com/Signalsmith-Audio/signalsmith-stretch .

The bundled **Pocket TTS Alba preset** (`assets/pocket-tts-alba.safetensors`) is
Kyutai's precomputed embedding of Alba MacKenna's Casual recording. Attribution:
Alba MacKenna and Kyutai. Licensed under CC BY 4.0:
https://creativecommons.org/licenses/by/4.0/ . Distributed unchanged from
https://huggingface.co/kyutai/pocket-tts-without-voice-cloning/blob/d4fdd22ae8c8e1cb3634e150ebeff1dab2d16df3/embeddings/alba.safetensors .
SHA-256: `ad234695323e4030336b6afc8a050c97e3110603e11ecd8226d9562488300a50`.
Voice source and licence: https://huggingface.co/kyutai/tts-voices#alba-mackenna .
The preset is embedded in Echo; Pocket TTS model weights are downloaded separately.

Optional computer tools are downloaded directly from upstream after explicit
selection: Cua Driver 0.34.0 (Apache-2.0), Node.js 24.21.0 (MIT and bundled
third-party notices), Playwright MCP 0.0.83 and Playwright (Apache-2.0), and
Chromium/Chrome for Testing 155.0.8059.12 (its bundled open-source licenses).
Original license and notice files in these archives and npm packages are retained
in the private components directory. Ollaya 0.8.0 is Apache-2.0; its Laya English
model retains Convai Innovations' upstream license and attribution. Downloads
are optional and are not hosted or included in the main Echo installer.
