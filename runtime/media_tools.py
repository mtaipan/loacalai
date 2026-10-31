"""Offline Windows OCR, Whisper transcription and explicit microphone recording."""
import json
import contextlib
import os
import sys
from pathlib import Path
import subprocess
import tempfile
import wave

BASE = Path(__file__).resolve().parent
SPEECH_MODEL = Path(os.environ.get('LOCAL_GEMMA_SPEECH_MODEL', str(BASE / 'models' / 'whisper-small')))
_whisper = None

def capture_area(language='en-US'):
    with contextlib.ExitStack() as cleanup:
        path=Path(os.environ['LOCAL_GEMMA_CAPTURE_PATH']) if os.environ.get('LOCAL_GEMMA_CAPTURE_PATH') else Path(cleanup.enter_context(tempfile.TemporaryDirectory(prefix='local-screen-area-')))/'region.png'
        cleanup.callback(lambda:path.unlink(missing_ok=True))
        if sys.platform=='darwin':
            result=subprocess.run(['/usr/sbin/screencapture','-i','-x',str(path)],capture_output=True,timeout=120)
            if not path.is_file():
                if result.returncode and result.stderr.strip():raise RuntimeError('Снимок экрана недоступен. Разрешите запись экрана в настройках macOS. '+result.stderr.decode('utf-8',errors='replace'))
                return {'cancelled':True}
        elif os.name=='nt':
            result=subprocess.run(['powershell.exe','-STA','-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',str(BASE/'capture_area.ps1'),'-OutputPath',str(path)],capture_output=True,timeout=120,creationflags=subprocess.CREATE_NO_WINDOW)
            if result.returncode:raise RuntimeError(result.stderr.decode('utf-8',errors='replace').strip())
            state=json.loads(result.stdout.decode('utf-8-sig'))
            if state.get('cancelled'):return state
        else:raise RuntimeError('Выделение области экрана поддерживается в Windows и macOS.')
        text=image_text(path,language)
        if not text.strip():raise ValueError('В выбранной области текст не распознан. Выберите более крупный текст и проверьте язык OCR.')
        return {'text':text,'cancelled':False}


def image_text(path, language='ru'):
    if sys.platform == 'darwin':
        return image_text_macos(path, language)
    from PIL import Image, ImageOps
    path = Path(path)
    if not path.is_file():
        raise ValueError(f'Картинка не найдена: {path}')
    with Image.open(path) as original:
        image = ImageOps.exif_transpose(original).convert('RGB')
        # Windows OCR has a maximum bitmap dimension; normalize formats and orientation.
        image.thumbnail((4000, 4000))
        with tempfile.TemporaryDirectory(prefix='local-ocr-') as temporary:
            normalized = Path(temporary) / 'image.png'
            image.save(normalized)
            result = subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive',
                '-ExecutionPolicy', 'Bypass', '-File', str(BASE / 'ocr.ps1'),
                '-ImagePath', str(normalized), '-Language', language],
                capture_output=True, timeout=90, creationflags=subprocess.CREATE_NO_WINDOW)
    if result.returncode:
        raise RuntimeError(result.stderr.decode('utf-8', errors='replace').strip())
    return json.loads(result.stdout.decode('utf-8-sig'))['text']


def image_text_macos(path, language='ru'):
    import Vision
    from Foundation import NSURL
    request = Vision.VNRecognizeTextRequest.alloc().init()
    request.setRecognitionLevel_(Vision.VNRequestTextRecognitionLevelAccurate)
    selected = {'ru': 'ru-RU', 'en': 'en-US'}.get(language, language)
    request.setRecognitionLanguages_([selected, 'en-US'] if selected != 'en-US' else ['en-US'])
    request.setUsesLanguageCorrection_(True)
    handler = Vision.VNImageRequestHandler.alloc().initWithURL_options_(NSURL.fileURLWithPath_(str(Path(path).resolve())), {})
    success, error = handler.performRequests_error_([request], None)
    if not success:
        raise RuntimeError(f'Apple Vision OCR: {error}')
    return '\n'.join(str(item.topCandidates_(1)[0].string()) for item in request.results() or [])


def speech_text(path, language=None):
    global _whisper
    from faster_whisper import WhisperModel
    if not SPEECH_MODEL.joinpath('model.bin').is_file():
        raise RuntimeError('Модель речи не установлена. Запустите setup_media.bat.')
    if not Path(path).is_file():
        raise ValueError(f'Аудиофайл не найден: {path}')
    if _whisper is None:
        _whisper = WhisperModel(str(SPEECH_MODEL), device='cpu', compute_type='int8',
                                cpu_threads=6, local_files_only=True)
    try:
        segments, info = _whisper.transcribe(str(path), language=language or None,
            beam_size=5, vad_filter=True, condition_on_previous_text=False)
        lines = [segment.text.strip() for segment in segments]
    except Exception as exc:
        raise RuntimeError(f'Не удалось распознать аудио: {exc}') from exc
    return {'text': '\n'.join(lines), 'language': info.language,
            'duration': round(info.duration, 2)}


def microphone_text(seconds=10, language=None):
    import sounddevice as sd
    seconds = int(seconds)
    if not 1 <= seconds <= 120:
        raise ValueError('Длительность записи должна быть от 1 до 120 секунд.')
    try:
        device = sd.query_devices(kind='input')
        rate = int(device['default_samplerate'])
        print(f'Запись с микрофона: {device["name"]}, {seconds} секунд. Говорите...', flush=True)
        audio = sd.rec(int(seconds * rate), samplerate=rate, channels=1, dtype='int16')
        sd.wait()
    except Exception as exc:
        raise RuntimeError(f'Микрофон недоступен: {exc}. Можно передать готовый аудиофайл.') from exc
    finally:
        sd.stop()
    with tempfile.TemporaryDirectory(prefix='local-voice-') as temporary:
        path = Path(temporary) / 'recording.wav'
        with wave.open(str(path), 'wb') as output:
            output.setnchannels(1)
            output.setsampwidth(2)
            output.setframerate(rate)
            output.writeframes(audio.tobytes())
        print('Распознавание речи...', flush=True)
        return speech_text(path, language)
