# Model attribution

bazarr-parakeet code is licensed under MIT. Model weights are separate works and
are not included in the application image or source distribution.

The default model is Moondream Parakeet Ultra, a post-trained derivative of
NVIDIA Parakeet TDT 0.6B v3. The default ONNX conversion is provided by altunenes.

- Moondream model: https://huggingface.co/moondream/parakeet-ultra
- NVIDIA base model: https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3
- ONNX conversion: https://huggingface.co/altunenes/parakeet-rs/tree/main/parakeet-ultra
- License: Creative Commons Attribution 4.0 International
  https://creativecommons.org/licenses/by/4.0/

Moondream post-trained the NVIDIA model; altunenes converted it to ONNX.
This project does not modify those downloaded weights. The downloader records
the repository, exact revision, and file checksums in the model manifest.
Alternative model weights retain their respective licenses and attribution
requirements.
