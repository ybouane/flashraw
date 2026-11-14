# Demosaic benchmark

Compares FlashRaw's demosaic modes with LibRaw (through `rawpy`), OpenCV and colour-demosaicing. Three views:

| Script | Question | Needs |
|---|---|---|
| `truth.py` | How close to the known answer? Clean tiles are re-mosaicked (Bayer, X-Trans), written as DNG (`dng.py`), developed by every method and scored (PSNR, edge PSNR, colour error). | rawpy, opencv-python-headless, colour-demosaicing |
| `real.py` | How clean and sharp on real files? No reference: colour-speckle index and fine detail. | rawpy |
| `speed.py` | How fast? Post-process stage on one Bayer and one X-Trans file. | rawpy |

Tiles for `truth.py` come from `tools/demosaic/edgeaware_train.py gt`. For an unbiased run build the CLI with tables
trained without the evaluation tiles (`load(gt, 'train')` in the training script) and pass that binary: the shipped
tables are trained on every tile. Run all three and compare releases; see `docs/design/edge-aware-demosaic.md`
for the method. Results are written as JSON; this is the only place other libraries may be named besides the
benchmark docs.
