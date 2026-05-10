---
marp: true
---

export SCRATCH=/scratch/bhbf/lostadi
export HF_TOKEN=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

source /scratch/bhbf/lostadi/hf_dl_venv/bin/activate

python3 - <<'PYEOF'
import os
from huggingface_hub import snapshot_download

token   = os.environ["HF_TOKEN"]
raw_dir = os.environ["SCRATCH"] + "/shapesplats_raw"

print(f"Downloading to: {raw_dir}", flush=True)
path = snapshot_download(
    repo_id="ShapeNet/ShapeSplatsV1",
    repo_type="dataset",
    local_dir=raw_dir,
    local_dir_use_symlinks=False,
    ignore_patterns=["*.gitattributes", "*.gitignore", "README.md"],
    token=token,
)
print(f"Done: {path}", flush=True)
PYEOF