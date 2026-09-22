# Third-party notices

Echo can download an ONNX conversion of **NVIDIA Parakeet TDT 0.6B v2** on the user's request. The model is not included in the installer. It is licensed under CC BY 4.0. Original model: `nvidia/parakeet-tdt-0.6b-v2`. ONNX conversion and runtime integration: `k2-fsa/sherpa-onnx`.

The local runtime also contains sherpa-onnx (Apache License 2.0), ONNX Runtime and its transitive native components, and uses Silero VAD (MIT) when its separately downloaded model is enabled. The corresponding license texts are installed alongside this notice.

Optional post-processing downloads the INT8 **FunASR CT-Transformer Chinese/English punctuation model**, converted and distributed by k2-fsa/sherpa-onnx (2024-04-12 vocabulary-272727 package). It is not included in the installer. Upstream model: https://huggingface.co/funasr/ct-punc (Apache License 2.0); conversion: https://github.com/k2-fsa/sherpa-onnx/releases/tag/punctuation-models. The Apache 2.0 license text is included in `licenses/sherpa-onnx-Apache-2.0.txt`. English written-form rules run locally in Rust using text2num 2.8.0 (MIT).

`RUST_THIRD_PARTY_NOTICES.txt` contains the locked Rust dependency inventory and the license or notice files shipped by those package sources.
