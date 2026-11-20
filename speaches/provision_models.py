#!/usr/bin/env python3
"""Fill the persistent Hugging Face cache with the model Speaches serves.

Run by the one-shot `speaches-models` compose service. An already cached
snapshot is returned without any network access, so repeated deploys are cheap.
"""
from __future__ import annotations

import os
import sys

from huggingface_hub import snapshot_download

DEFAULT_MODEL = "Systran/faster-whisper-small"
# The set Speaches reads; README carries the model card used by /v1/models.
ALLOW_PATTERNS = [
    "config.json",
    "preprocessor_config.json",
    "model.bin",
    "tokenizer.json",
    "vocabulary.*",
    "README.md",
]


def main() -> int:
    model = os.environ.get("VOICE_MODEL", "").strip() or DEFAULT_MODEL
    try:
        path = snapshot_download(model, allow_patterns=ALLOW_PATTERNS, local_files_only=True)
    except Exception:
        try:
            path = snapshot_download(model, allow_patterns=ALLOW_PATTERNS)
        except Exception as error:
            print(
                f"speaches-models: {model}: {type(error).__name__}: {error}",
                file=sys.stderr,
                flush=True,
            )
            return 1
        print(f"speaches-models: downloaded {model} to {path}", flush=True)
        return 0
    print(f"speaches-models: {model} already cached at {path}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
