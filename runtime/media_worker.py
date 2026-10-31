"""Isolated JSON worker. Rust owns chat, tools, storage and permissions."""
import argparse
import contextlib
import json
import sys

sys.stdout.reconfigure(encoding='utf-8')
sys.stderr.reconfigure(encoding='utf-8')
parser = argparse.ArgumentParser()
parser.add_argument('action', choices=['ocr', 'audio', 'record', 'capture_area'])
parser.add_argument('--file')
parser.add_argument('--language')
parser.add_argument('--seconds', type=int, default=10)
args = parser.parse_args()
try:
    import media_tools
    with contextlib.redirect_stdout(sys.stderr):
        if args.action == 'capture_area':
            result = media_tools.capture_area(args.language or 'en-US')
        elif args.action == 'ocr':
            result = {'text': media_tools.image_text(args.file, args.language or 'ru')}
        elif args.action == 'audio':
            result = media_tools.speech_text(args.file, args.language)
        else:
            result = media_tools.microphone_text(args.seconds, args.language)
    print(json.dumps(result, ensure_ascii=False))
except Exception as exc:
    print(json.dumps({'error': str(exc)}, ensure_ascii=False))
    sys.exit(1)
