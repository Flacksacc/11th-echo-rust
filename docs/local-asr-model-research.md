# Local speech recognition options for Echo

This report prioritizes English dictation and fully local operation on both the current computer and modest Windows PCs. Recommendations are engineering judgments supported by the application code and primary sources; comparative performance inside Echo remains unmeasured.

## Recommendation

Keep the current Parakeet model as the reference and add alternatives in stages. The most useful shortlist is:

| Purpose | First candidate | Why test it |
| --- | --- | --- |
| Less CPU with the smallest application change | **Parakeet TDT/CTC 110M, TDT INT8 export** | Closely matches the existing recognizer configuration; substantially smaller architecture. |
| Natural live dictation with long pauses | **Moonshine Streaming Small or Medium** | Incremental recognition can avoid repeatedly processing the entire unfinished phrase. |
| A conservative English accuracy experiment | **Parakeet Unified English 0.6B, offline INT8** | Same runtime family, with a modest improvement in NVIDIA's matched evaluation. |
| A promising new accuracy/efficiency experiment | **Granite Speech 5.0 470M TurboCTC, Q8 or Q4** | Recent English CTC model with an available native CPU/Vulkan port. |
| Optional GPU acceleration without changing the acoustic model first | **Current Parakeet through a native Vulkan backend** | Separates the benefit of a different execution engine from the benefit of different model weights. |

These are candidates to compare, not five defaults to ship immediately. Model sources and integration qualifications appear below. My recommended first implementation is a model selector supporting the existing model and Parakeet 110M, followed by a Moonshine streaming prototype and a Granite CPU/Vulkan prototype.

There is also an application-level opportunity: Echo currently creates live previews by repeatedly decoding the unfinished audio. Reducing that repeated work may save more CPU during thoughtful, pause-heavy dictation than replacing the model alone.

## What this program currently uses

The local backend is **NVIDIA Parakeet TDT 0.6B v2, quantized to INT8, through sherpa-onnx 1.13.4**. It is not Whisper. The inspected loader explicitly selects the CPU execution provider, a NeMo transducer, and greedy decoding. Audio is 16 kHz mono.

Repository evidence:

| Area | Relevant code | Finding |
| --- | --- | --- |
| Model and download | [local_sherpa.rs](../src/transcription/local_sherpa.rs), model constants | One pinned model archive, file sizes and SHA-256 checks. |
| Recognition engine | Same file, `load_engine` and `decode` | Offline recognizer; fresh recognition stream for each decode. |
| Live previews | Same file, `LocalSherpaTranscriber::run` | Periodically reprocesses the unfinished audio history. |
| Model selection | [transcription/mod.rs](../src/transcription/mod.rs), provider construction | Local construction takes `config.local`; a different `model_id` alone does not select another model. |
| Runtime packaging | [Cargo.toml](../Cargo.toml), [build-installer.ps1](../installer/build-installer.ps1) | Pinned native sherpa version and Windows static-MT packaging. |
| Pause controls | [appwindow.slint](../ui/appwindow.slint), local advanced settings | Current working tree allows an end-of-speech pause up to 10,000 ms. |

The pinned archive is 482,468,385 bytes, about **460 MiB**. The listed installed model, token, and VAD assets total about **631 MiB**. Neither number measures process RAM or GPU memory. The normal thread default is half the physical cores, capped at four; it therefore defaults to four on this computer. These are code defaults, not a claim about the settings currently saved by the installed application.

Hardware inspection found:

| | Current computer | Proposed modest-PC test class |
| --- | --- | --- |
| CPU | AMD Ryzen 9 7950X, 16 cores / 32 logical processors | Older 4–6-core laptop or desktop CPU |
| RAM | 63.1 GiB reported, approximately 64 GB | 8 GB and 16 GB configurations |
| GPU | NVIDIA RTX 5080, 16,303 MiB reported | Integrated graphics; also test CPU-only |
| GPU driver | 616.92 | Record exact driver for every GPU test |

Your computer has ample capacity for every primary candidate. The useful question is how much recognition competes with other work, including games, rather than whether the model fits. The modest-PC class above is a proposed validation target; no second machine was measured.

## Candidate comparison

“Existing runtime” means sherpa exposes an appropriate model family. It does **not** mean Echo can load the model unchanged, or that every new export has been tested with its pinned native library.

| Model | Scale and recognition mode | Integration route | Assessment for English dictation |
| --- | --- | --- | --- |
| Parakeet TDT 0.6B v2 | 600M; offline | Already implemented | Strong baseline; keep available. [^1] |
| Parakeet TDT/CTC 110M | About 114M; offline TDT or CTC | Existing sherpa transducer for TDT; separate CTC configuration | First low-CPU candidate; likely an accuracy tradeoff. [^2] |
| Parakeet Unified English 0.6B | 600M; offline or buffered streaming | Sherpa exports; offline is the closest adaptation | First conservative accuracy trial. Buffered streaming still recomputes context. [^3] |
| Moonshine Streaming Small / Medium | 123M / 245M; incremental streaming | Moonshine native C API, or transcribe.cpp | Strongest architectural match for continuous previews and long pauses. [^4][^5] |
| Moonshine Streaming Tiny | 34M; incremental streaming | Same native routes | Resource-constrained option, with a larger accuracy sacrifice. [^4] |
| Granite Speech 5.0 TurboCTC | 470M; offline CTC in the native port | transcribe.cpp, CPU or Vulkan | High-priority experimental contender; very recent implementation. [^8][^9] |
| Nemotron Speech Streaming English 0.6B | 600M; cache-aware streaming | Sherpa online transducer exports | Alternative streaming prototype using the existing runtime family. [^10][^11] |
| Parakeet Realtime EOU 120M | 120M; cache-aware streaming | Native streaming adapter required | Useful for previews; lacks punctuation and capitalization. [^12] |
| Zipformer English 20M | 20M; streaming | Sherpa `OnlineRecognizer` | Small-machine fallback; test free-form dictation and punctuation carefully. [^13] |
| Whisper base.en / small.en | Offline; quantized variants available | whisper.cpp or sherpa Whisper configuration | Useful independent baseline, particularly with optional Vulkan; not an assumed CPU upgrade. [^14] |
| Parakeet TDT 0.6B v3 | 600M; offline, 25 languages | Existing sherpa transducer family | Better scope for multilingual support, not an automatic English upgrade. [^15] |
| Canary 180M Flash | 182M; offline, four languages | Sherpa Canary configuration | Secondary English/multilingual experiment. [^16] |
| SenseVoice Small | Offline, five languages | Sherpa SenseVoice configuration | More compelling when Asian-language support matters. [^17] |
| Qwen3-ASR 0.6B | Offline/streaming model family, 30 languages | Sherpa offline Qwen3 configuration or another native adapter | Broader language coverage; not the first choice for minimum CPU or download size. [^18][^19] |

### Parakeet 110M: easiest useful addition

NVIDIA's hybrid checkpoint contains both TDT and CTC decoding paths. Its published **7.49% average WER refers to TDT**; it should not be assigned to an independently exported CTC INT8 model. It supports punctuation and capitalization and uses CC-BY-4.0. [^2]

Choose the TDT INT8 export first because Echo already loads encoder, decoder, and joiner files. That reduces adaptation work and makes a comparison easier to interpret. A smaller parameter count is evidence for a useful experiment, not proof of a proportional speedup.

I verified current GitHub release metadata: a TDT INT8 archive exists. An older issue requesting that export is stale, and a similarly named Hugging Face repository can be only a placeholder. Use the actual release asset and validate its contents. The accompanying [asset inventory](local-asr-release-assets.json) records names, sizes, timestamps, URLs, and available GitHub digests. [^20]

For download planning, selected compressed sherpa archives are approximately 103 MiB for 110M TDT INT8, 460 MiB for the current v2, and 478 MiB for Unified offline INT8. These are transfer sizes, not installed footprint or runtime memory. [^20]

### Moonshine: the most relevant streaming alternative

The current English Streaming Small and Medium models are distinct from the older Moonshine Tiny/Base models. Sherpa's Moonshine configuration and “v2” export naming should not be treated as evidence that the new streaming architectures are interchangeable with those older offline graphs.

The streaming paper describes an encoder that processes new audio with bounded attention context. This directly addresses Echo's growing-prefix preview workload, although decoding, application buffering, and endpoint decisions still contribute latency and CPU use. [^5]

Use the official native C API or validate transcribe.cpp's streaming support. Moonshine supplies a Windows C++ distribution, so this does not require users to install Python. It does require an additional native dependency and an adapter that maintains recognition state across audio chunks. [^6]

An important current detail: July 30, 2026 quantization updates improved the shipped Tiny model. The documentation separates floating-point leaderboard results from quantized LibriSpeech-clean results; older quantized Tiny figures should not be reused as current results. [^7]

My recommendation is Small for a low-resource preset and Medium for a quality-oriented streaming trial. Tiny is worth testing on weak machines, but should not replace the current default solely because it is small.

### Granite Speech 5: promising, but evaluate the actual Windows port

IBM released the English 470M TurboCTC model on August 25, 2026. It uses an acoustic encoder and non-autoregressive CTC decoding rather than a large language-model decoder. The primary checkpoint is Apache-2.0. [^8]

The transcribe.cpp port provides Q8 at **506 MB** and Q4 at **279 MB**. Its maintainer reports 1.34% LibriSpeech-clean WER for both, versus 1.33% for the reference. This is clean read speech, not evidence that quantization preserves every accent or noisy phrase. The native port is offline. Use the Apache model, not the separately licensed noncommercial sibling. [^9]

This is a strong candidate for both machines: test Q8 first on yours, and Q4 alongside Q8 on the modest PC. Its recency means version pinning and Windows validation deserve particular attention. A web demo that streams results does not establish streaming support in this native adapter.

## What the accuracy numbers do—and do not—show

WER measures word substitutions, deletions, and insertions. Lower is better, but normalizing punctuation and case can hide exactly the sentence-boundary behavior that bothers you.

These published figures are useful for screening. They are not a single controlled Windows benchmark:

| Published evaluation | Average WER | Interpretation |
| --- | ---: | --- |
| Parakeet v2 original model card, eight datasets | 6.05% | Reference-quality baseline. [^1] |
| Parakeet 110M TDT model card | 7.49% | Expected quality/resource tradeoff; not the CTC INT8 result. [^2] |
| Moonshine Streaming Medium, floating point, eight datasets | 6.65% | Competitive screening result, not shipped INT8 accuracy. [^4] |
| Moonshine Streaming Small, same table | 7.84% | Further resource/quality tradeoff. [^4] |
| Moonshine Streaming Tiny, same table | 12.00% | Substantial compromise for unconstrained dictation. [^4] |
| Parakeet v3 English, eight datasets | 6.34% | Newer and multilingual does not necessarily improve English. [^15] |
| Unified card's matched offline comparison | v2 6.04%; Unified 5.91% | Most useful direct evidence for the conservative upgrade: 0.13 percentage points. [^3] |

The Unified comparison is a small improvement, not a transformative one. Its streaming configurations trade latency for quality: the same card reports 6.29% at 1.12 seconds and 7.35% at 0.24 seconds. Those configuration latencies are not complete microphone-to-text latency in Echo. [^3]

**Do not rank Granite's advertised 5.00% against Parakeet's 6.05% directly.** IBM's August results use an updated seven-dataset public mixture, including cleaned/resegmented sets. The familiar older results use eight datasets. IBM also measures throughput on an H200, which says little about an individual Windows dictation session. [^21]

For the additional streaming candidates, Nemotron reports 6.93% at 1.12 seconds and 7.67% at 160 ms; Parakeet EOU reports 9.30% at 160 ms and omits punctuation/case. These are useful architectural alternatives, not established improvements over the current final transcript. [^10][^12]

## CPU, memory, and small amounts of GPU

### Published desktop evidence

Granite's native-port maintainer reports the following on a Ryzen 7 PRO 4750U running Fedora 43, using Q8, one warmup and the mean of three runs: [^9]

| Audio duration | CPU decode time | Integrated-GPU Vulkan decode time |
| --- | ---: | ---: |
| 11.0 seconds | 696 ms | 652 ms |
| 35.3 seconds | 2.28 seconds | 1.48 seconds |

These are two clips, not a broad benchmark, and not Windows measurements. They suggest a worthwhile trial on modest hardware. They also show why GPU offload might save little on a short utterance while helping a longer one. They do not establish RAM, VRAM, power consumption, or gaming impact.

Separately, parakeet.cpp reports approximately 34.7-times-real-time for v2 Q8 and 91.5-times for 110M Q8 in an eight-thread CPU test over 100 LibriSpeech-clean samples. Its comparison is against NeMo, **not Echo's sherpa INT8 backend**. Its “agreement WER” measures disagreement with a reference implementation, not recognition errors against human transcripts. [^22]

Moonshine's benchmark documentation measures finalization after VAD decides speech has ended; it excludes the waiting period. That distinction matters when your configured pause is ten seconds. A fast published finalization result does not eliminate a deliberately long endpoint wait. [^23]

### Recommended GPU routes

**Vulkan is the first portable prototype I would try.** parakeet.cpp offers Windows CPU, Vulkan, and CUDA packages and a native C interface. That permits testing the existing acoustic model through another runtime. transcribe.cpp documents Windows Vulkan builds and includes Rust bindings, offering one experimental route to several shortlisted families. These are additional native runtimes, not drop-in ONNX files. [^24][^25]

**CUDA through sherpa is possible, but changes packaging.** The existing CPU static package does not become GPU-enabled by changing a string. Sherpa documents Windows CUDA builds; the ONNX Runtime, CUDA, and cuDNN versions must match the packaged binary, with suitable support for the RTX 5080. DLL discovery and a clean Windows install must be tested. [^26][^27]

**DirectML is not absent from sherpa.** The pinned 1.13.4 source contains a Windows DirectML execution-provider path behind a build flag. Enabling it requires an appropriate build and deployment, plus performance testing; it is not evidence that the current installer supports it. Microsoft now describes DirectML as being in sustained engineering and recommends Windows ML for new Windows deployments. [^28][^29]

For any ONNX GPU path, verify operator placement. An INT8 export tuned for CPU does not guarantee that its expensive operations all execute efficiently on the GPU. Compare an appropriate GPU precision/export and inspect CPU fallback. Execution providers assign supported graph portions to devices. [^30]

“Small GPU use” should mean three separately measured things: peak dedicated memory, time spent computing, and interference with the foreground application. Model-file size is not VRAM usage. A model can fit easily and still create noticeable bursts while gaming.

On your PC, retain a CPU mode even if GPU decoding is faster. On modest PCs, make CPU the dependable default until integrated-GPU tests demonstrate a benefit. Load only the selected model, avoid concurrent decode jobs, and benchmark short interactive requests rather than server-style batches. These are proposed application policies, not measured guarantees.

## Why pauses feel unnatural, and useful settings now

Four separate mechanisms affect the result: VAD decides whether audio contains speech; its silence timer closes an audio segment; a maximum-duration limit can also split a segment; the recognizer chooses punctuation from the context it receives. A ten-second silence allowance addresses the second mechanism. It cannot guarantee that the recognizer never inserts a period during a hesitation.

In the current implementation, increasing the pause allowance also keeps unfinished history around longer. Every preview decode processes that growing history again. The loop awaits decoding, so the effective update spacing includes decode time; it is not a guaranteed one-second cadence. Native incremental streaming can reduce this repeated work without requiring you to finish thoughts sooner.

These are starting settings to try, not changes made by this report:

| Setting | Starting point | What it changes |
| --- | --- | --- |
| End-of-speech pause | **10,000 ms** | Keeps shorter thinking pauses inside the same VAD segment. The working-tree control now permits this. |
| Maximum phrase duration | **60 seconds** | Reduces forced splits in longer thoughts; increases unfinished context and preview work. |
| Partial update interval | **1,500–2,000 ms** | Reduces repeated decoding at the cost of less frequent preview updates. |
| Threads, your PC | **4 initially; compare 2** | Tests whether less CPU contention is worth slightly slower decoding. |
| Threads, modest PC | **2 initially; compare 1 and 4** | Finds an appropriate latency/resource balance for that processor. |
| VAD threshold | **Keep 0.5 initially** | Change only for missed quiet speech or persistent background-noise activation. |
| Pre/post padding and minimum speech | **Keep current defaults initially** | Avoid changing several boundary-detection variables at once. |
| Full-session re-decode | **Optional trial for short dictation** | Gives final recognition broader context; adds work after Stop. Current full-session buffer is capped at three minutes. |

The source default silence remains 600 ms; allowing ten seconds does not automatically change an existing saved preference. Full-session re-decode may improve continuity, but punctuation improvement is not guaranteed. Manual Stop should continue to finalize immediately rather than waiting for the silence timeout. An automatic endpoint should not become an instruction to paste irreversible fragments while you are still composing.

## Integration plan

### Stage 1: model selection within sherpa

Add a small model catalog containing the current v2, 110M TDT INT8, and Unified offline INT8. Each entry needs its own model family/configuration, complete file set, tokenizer, sample rate, sizes, hashes, license attribution, and required runtime version. The current constants and verification marker are tied to one model.

Separate model identity from backend and device. “Local” is a provider category; “Parakeet 110M” is a model; “CPU” is an execution choice. Key the cached recognizer by model, device, and thread configuration, and release the previous model when switching. Preserve the existing verified HTTPS downloads and safe extraction.

The current Rust wrapper has additional offline configuration types, including CTC, Whisper, Canary, SenseVoice and Qwen3-ASR. That is useful scaffolding, but each selected export must be smoke-tested against the actual 1.13.4 native binary. Newer documentation or a matching struct name alone is insufficient.

### Stage 2: one genuine streaming implementation

Prototype Moonshine Small/Medium with persistent stream state. Alternatively, evaluate Nemotron through sherpa's online transducer API: official INT8 exports exist at multiple chunk sizes. Unified streaming exports also appear in the current release inventory, but its buffered architecture should not be confused with cache-aware recognition. [^11][^20]

Preserve Echo's `Partial`, `Committed`, and error behavior. Define how a revised hypothesis replaces preview text, how VAD interacts with model endpointing, and how Stop flushes pending audio. Keep this adapter separate from the existing offline segment decoder so streaming state is not accidentally discarded for each chunk.

### Stage 3: one optional native CPU/Vulkan backend

Prototype transcribe.cpp for Granite and, if suitable, additional families. Its Rust bindings build native code during development; users can receive compiled binaries without Python, CMake, or developer tools. Pin a tested revision and bundle every required runtime dependency. Start with documented Windows Vulkan support rather than assuming Linux CUDA examples validate Windows CUDA. [^25]

Do not replace sherpa wholesale before side-by-side tests. A narrower parakeet.cpp integration is also reasonable if the immediate aim is GPU acceleration for Parakeet rather than a larger model catalog.

For any eventual release, run the repository's required formatting, clippy, tests, release build and installer checks, and exercise the main window, settings, tray, hotkey, overlay, model downloads and a clean Windows account. Compilation alone will not validate native-library packaging or interaction behavior.

## Benchmark that would settle the choice

Use identical recorded input for every candidate, followed by a live interaction pass. With explicit consent, prepare 60–100 clips totaling roughly 30–60 minutes: everyday dictation, technical names, numbers, corrections, quiet speech, background noise, and pauses of 3, 5, 10 and 15 seconds. Include long thoughts, sessions approaching three minutes, and sessions exceeding the current full-redecode limit.

Keep cloud rewriting disabled during recognizer scoring so it cannot hide transcription errors. Do not collect private recordings or full transcripts in diagnostic logs by default.

| Measurement | Why it matters |
| --- | --- |
| Normalized WER plus punctuation/case review | Separates recognition quality from the sentence-boundary issue. |
| Manual correction effort | Measures usefulness for actual dictation, including names and numbers. |
| First-preview latency and preview revisions | Measures whether the interface feels responsive and stable. |
| Stop-to-final latency, median and p95 | Captures the wait the user experiences, including flushing and optional re-decode. |
| CPU seconds per audio minute and peak utilization | Distinguishes total work from brief bursts. |
| Cold load, warm decode, peak RAM and download size | Exposes costs hidden by warmed-up benchmarks. |
| Dedicated/shared GPU memory, GPU time and foreground frame times | Tests the “very little GPU” requirement directly. |

Run CPU tests at one, two and four threads, batch size one, with the same audio segmentation first. Then test each model's intended streaming mode as a separate interaction comparison. Record the exact model hash, runtime revision, power mode and hardware. Compare CPU and GPU during the same representative game or foreground workload on the RTX 5080.

Select defaults only after the results show a useful tradeoff. Suggested eventual choices are **Balanced** (current Parakeet or a proven successor), **Low CPU** (110M or Moonshine Small), and **Optional GPU** (a validated native backend). Label these as product presets only after measurements support them.

The download catalog should preserve model-specific license information. Parakeet v2 uses CC-BY-4.0, Unified uses NVIDIA's Open Model License, and the current English Moonshine streaming models use MIT. Runtime and model licenses are separate; a permissively licensed inference library does not change the terms attached to its weights. [^1][^3][^4]

## Lower-priority options and limitations

Whisper remains valuable as an independent recognizer. whisper.cpp provides native CPU and GPU execution, whereas faster-whisper's usual Python interface would add packaging work contrary to Echo's current end-user requirements. It is not disqualified as an engine, but embedding a native CTranslate2 path is a separate integration project. [^14][^31]

Canary's four-language scope and SenseVoice's five-language scope make them useful later. Qwen3-ASR offers broader coverage, but the current sherpa INT8 archive is roughly 838 MiB and includes a substantial decoder; it is not the obvious lightweight English choice. [^16][^17][^18][^19][^20]

Larger speech-language models can be investigated if the objective shifts toward maximum accuracy or richer tasks. For this request, adding several billion parameters before testing the shortlisted acoustic models would complicate CPU and GPU budgets without establishing a benefit.

The strongest remaining uncertainty is the **actual quality/resource tradeoff inside Echo on Windows**: alternative-model latency, CPU consumption, RAM and GPU use have not been measured in the application. Published results establish worthwhile candidates; the proposed benchmark establishes a defensible default.

## Sources

Primary sources accessed September 14, 2026. Model cards describe their publishers' evaluations; native-port benchmarks describe their maintainers' implementations. The release inventory is metadata, not a verified production download manifest: missing digests must be supplied and downloaded contents verified before shipping.

[^1]: NVIDIA, [Parakeet TDT 0.6B v2 model card](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2).
[^2]: NVIDIA, [Parakeet TDT/CTC 110M model card](https://huggingface.co/nvidia/parakeet-tdt_ctc-110m).
[^3]: NVIDIA, [Parakeet Unified English 0.6B model card](https://huggingface.co/nvidia/parakeet-unified-en-0.6b), April 2026.
[^4]: Moonshine, [current available models, English reference WER and licenses](https://moonshine-voice.readthedocs.io/en/latest/models/available-models/).
[^5]: Kudlur et al., [Moonshine v2 streaming speech recognition paper](https://arxiv.org/html/2602.12241v1), February 2026.
[^6]: Moonshine, [native C API](https://moonshine-voice.readthedocs.io/en/latest/api/c-api/) and [Windows-capable quickstart](https://moonshine-voice.readthedocs.io/en/latest/quickstart/).
[^7]: Moonshine, [accuracy and current quantization results](https://moonshine-voice.readthedocs.io/en/latest/models/accuracy/).
[^8]: IBM, [Granite Speech 5.0 470M TurboCTC model card](https://huggingface.co/ibm-granite/granite-speech-5.0-470m-turboctc), August 25, 2026.
[^9]: transcribe.cpp maintainers, [Granite 5 native implementation, quantization and benchmarks](https://github.com/handy-computer/transcribe.cpp/blob/main/docs/models/granite-speech-5.0-470m-turboctc.md), upstream checkpoint pinned September 12, 2026.
[^10]: NVIDIA, [Nemotron Speech Streaming English 0.6B model card](https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b).
[^11]: sherpa-onnx, [Nemotron streaming models and online recognizer examples](https://k2-fsa.github.io/sherpa/onnx/nemo/nemotron-streaming.html).
[^12]: NVIDIA, [Parakeet Realtime EOU 120M model card](https://huggingface.co/nvidia/parakeet_realtime_eou_120m-v1).
[^13]: sherpa-onnx, [small English online models](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/small-online-models.html).
[^14]: ggml-org, [whisper.cpp implementation, models and supported backends](https://github.com/ggml-org/whisper.cpp).
[^15]: NVIDIA, [Parakeet TDT 0.6B v3 model card](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3).
[^16]: NVIDIA, [Canary 180M Flash model card](https://huggingface.co/nvidia/canary-180m-flash).
[^17]: FunAudioLLM, [SenseVoice Small model card](https://huggingface.co/FunAudioLLM/SenseVoiceSmall).
[^18]: Qwen, [Qwen3-ASR 0.6B model card](https://huggingface.co/Qwen/Qwen3-ASR-0.6B).
[^19]: sherpa-onnx, [Qwen3-ASR exports and file layout](https://k2-fsa.github.io/sherpa/onnx/qwen3-asr/pretrained.html).
[^20]: sherpa-onnx, [ASR model release assets](https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models); selected API metadata saved in [local-asr-release-assets.json](local-asr-release-assets.json). See also [NeMo offline model instructions](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/offline-transducer/nemo-transducer-models.html) and [Moonshine export documentation](https://k2-fsa.github.io/sherpa/onnx/moonshine/index.html).
[^21]: IBM authors, [Granite Speech 5 TurboCTC release article and evaluation chart](https://huggingface.co/blog/ibm-granite/granite-speech-5-0-470m-turboctc), August 25, 2026.
[^22]: parakeet.cpp maintainers, [benchmark methodology and results](https://github.com/mudler/parakeet.cpp/blob/master/benchmarks/BENCHMARK.md).
[^23]: Moonshine, [benchmark methodology and endpoint latency definition](https://moonshine-voice.readthedocs.io/en/latest/using/benchmarks/).
[^24]: mudler, [parakeet.cpp native implementation and Windows packages](https://github.com/mudler/parakeet.cpp).
[^25]: Handy Computer, [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp), [Windows build instructions](https://github.com/handy-computer/transcribe.cpp/blob/main/docs/build-windows.md), and [Rust native bindings](https://github.com/handy-computer/transcribe.cpp/blob/main/bindings/rust/sys/README.md).
[^26]: sherpa-onnx, [Windows CUDA build instructions](https://k2-fsa.github.io/sherpa/onnx/install/windows/build-cuda.html).
[^27]: Microsoft, [ONNX Runtime CUDA execution-provider requirements](https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider.html).
[^28]: sherpa-onnx, [version 1.13.4 session implementation, including conditional DirectML support](https://github.com/k2-fsa/sherpa-onnx/blob/v1.13.4/sherpa-onnx/csrc/session.cc).
[^29]: Microsoft, [ONNX Runtime on Windows and Windows ML guidance](https://onnxruntime.ai/docs/get-started/with-windows.html).
[^30]: Microsoft, [ONNX Runtime execution providers and graph partitioning](https://onnxruntime.ai/docs/execution-providers/).
[^31]: SYSTRAN, [faster-whisper implementation and dependencies](https://github.com/SYSTRAN/faster-whisper).
