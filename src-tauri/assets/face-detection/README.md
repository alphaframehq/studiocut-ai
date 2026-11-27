# Face-detection model — BlazeFace full-range

StudioCut AI uses this model for face-tracked auto-reframe
(`reframe_face_track`). The model is bundled in the repository so offline
builds keep working; no download happens at runtime.

## Model

- **Model:** BlazeFace full-range (Google MediaPipe face detector)
- **License:** Apache License 2.0 — see `LICENSE` in this directory.
  The model code and weights are Apache-2.0.
- **Training-data provenance:** BlazeFace was trained on images collected
  with consent. It was not trained on WIDER FACE. This is why it was
  chosen over YuNet and UltraFace, whose weights are MIT-licensed but
  whose training set (WIDER FACE, CC BY-NC-ND 4.0, academic-research-only
  terms) leaves it unsettled whether the dataset terms encumber the
  trained weights for commercial open-source use. YuNet and UltraFace
  are not used by StudioCut AI.
- **Official source (TFLite):**
  `https://storage.googleapis.com/mediapipe-models/face_detector/blaze_face_full_range/float16/latest/blaze_face_full_range.tflite`
- **Source SHA-256 (`.tflite`, verified 27 Nov 2026):**
  `3698b18f063835bc609069ef052228fbe86d9c9a6dc8dcb7c7c2d69aed2b181b`

## Conversion to ONNX

BlazeFace ships as TFLite. StudioCut AI runs it under `tract` (pure Rust),
which needs ONNX, so the official `.tflite` was converted in-house with
pinned `tf2onnx` 1.16.1 (TensorFlow 2.16.2, NumPy 1.26.4) on 27 Nov 2026:

```bash
python -m tf2onnx.convert \
  --tflite blaze_face_full_range.tflite \
  --output blaze_face_full_range.onnx \
  --opset 13
```

No third-party ONNX mirror was used as the bundled model. The
NardoCreations/mediapipe-face-onnx pack (Apache-2.0) was consulted only
as a cross-check reference for the anchor scheme and decode, never as
the bundled file.

- **Bundled file SHA-256 (`.onnx`, 2,076,735 bytes):**
  `7a986d07f1beb367b278cf0518a770bd864e2c47866a3b03b2bf2e6358f17abc`
- **ONNX opset:** 13, 178 nodes, passes the ONNX checker.
- **Numerical parity (same face photo, TFLite vs tract/ONNX):**
  regressors max absolute difference 8.4e-05, scores max absolute
  difference 8.6e-06; top detection score 0.7053179 (TFLite) vs
  0.7053183 (ONNX). The conversion is numerically equivalent for
  practical purposes.

## Input / output

- **Input:** `input`, shape `[1, 192, 192, 3]`, float32, RGB, values in
  `[-1, 1]` (NHWC layout — the conversion preserves the TFLite layout).
  Frames are resized to 192x192 with aspect-preserving letterboxing
  (see `src-tauri/src/face.rs`).
- **Outputs:**
  - `reshaped_regressor_face_4`, shape `[1, 2304, 16]` — per-anchor box
    and 6 keypoints, in YXHW order: `[y_centre, x_centre, height,
    width, kp0_y, kp0_x, ..., kp5_y, kp5_x]`.
  - `reshaped_classifier_face_4`, shape `[1, 2304, 1]` — per-anchor
    score logits (sigmoid applied during decode).
- **Anchors:** 2304, one fixed-size anchor per cell of a 48x48 grid
  (stride 4, centres `(gx + 0.5) / 48`, `(gy + 0.5) / 48`, width =
  height = 1.0), per the official `face_detection_full_range` graph
  (`num_layers: 1`, `strides: 4`, `num_boxes: 2304`) and the
  `SsdAnchorsCalculator` fixed-anchor-size branch.
- **Decode:** MediaPipe `TensorsToDetectionsCalculator` (default YXHW
  format, `x_scale = y_scale = w_scale = h_scale = 192`), linear
  (not exponential) box size. The linear variant was verified against
  the exponential variant on a real public-domain portrait: linear
  gives a 0.50 x 0.50 box framing the face (score 0.705); exponential
  gives a 1.65 x 1.65 box extending far outside the frame.
- **Detection threshold:** score >= 0.5 after sigmoid; non-maximum
  suppression at IoU 0.3 (the MediaPipe suppression threshold).

## Runtime

`tract` 0.23, CPU-only features (`default-features = false, features =
["onnx"]`). Measured release-binary size delta of adding tract (scratch
binaries, Linux x86_64): +32,113,176 bytes unstripped, +23,368,016 bytes
stripped. The previous "~26 MB" estimate was unverified and is not used.

## Reproducing the bundled file

1. Download the official `.tflite` from the source URL above.
2. Verify its SHA-256 against the value recorded above.
3. Convert with the pinned `tf2onnx` command above.
4. Verify the output SHA-256 against the value recorded above and run
   the ONNX checker (`onnx.checker.check_model`).
