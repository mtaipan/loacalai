import os
from pathlib import Path

BASE = Path(__file__).resolve().parent
os.environ.setdefault('HF_HUB_DISABLE_SYMLINKS_WARNING', '1')
os.environ.setdefault('HF_HOME', str(BASE.parent / 'data' / 'cache' / 'huggingface'))
from huggingface_hub import snapshot_download

if __name__ == '__main__':
    snapshot_download('Systran/faster-whisper-small', local_dir=Path(os.environ.get('LOCAL_GEMMA_SPEECH_PATH', str(BASE.parent / 'data' / 'models' / 'whisper-small'))),
        allow_patterns=['config.json', 'model.bin', 'tokenizer.json', 'vocabulary.*'])
    print('Whisper Small ready.')
