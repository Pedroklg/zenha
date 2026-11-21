# Vendored Xiph RNNoise v0.2

Source files from [xiph/rnnoise](https://github.com/xiph/rnnoise) tag `v0.2`
(commit `904a876dce1f9ab8860c0a5000ed151f9f6eef58`), licensed under BSD-3-Clause
(see `COPYING`).

- `src/rnnoise_init_model.c`: extracted `init_rnnoise` function from `rnnoise_data_little.c`
  so `USE_WEIGHTS_FILE` builds do not have to compile the 15 MB ASCII weights file.
- `src/denoise.c`: initialize `model->file = NULL` (and check `malloc` for `NULL`) in
  `rnnoise_model_from_buffer` so `rnnoise_model_free` does not call `fclose` on an
  uninitialized pointer.
- `rnnoise_little_0b50c45.bin`: binary weights blob dumped from the official
  `rnnoise_data-0b50c45.tar.gz` (`rnnoise_data_little.c`) model and loaded via
  `rnnoise_model_from_buffer` directly from aligned static memory.
