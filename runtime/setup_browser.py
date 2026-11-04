"""Download browser dependencies beside the app; reuse installed Edge/Chrome."""
import os
import subprocess
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
os.environ.setdefault('PLAYWRIGHT_BROWSERS_PATH', str(root/'data/cache/browsers'))
os.environ.setdefault('PIP_CACHE_DIR', str(root/'data/cache/pip'))
subprocess.run([sys.executable, '-m', 'pip', 'install', '--no-warn-script-location', 'playwright==1.64.1'], check=True)
from playwright.sync_api import sync_playwright
with sync_playwright() as p:
    for channel in (['msedge', 'chrome'] if sys.platform == 'win32' else ['chrome']):
        try:
            b = p.chromium.launch(channel=channel, headless=True)
            b.close()
            print('Ready: installed '+channel+'; browser download was not required.')
            break
        except Exception:
            pass
    else:
        subprocess.run([sys.executable, '-m', 'playwright', 'install', 'chromium'], check=True)
        browser = p.chromium.launch(headless=True)
        browser.close()
        print('Ready: Chromium launched from '+os.environ['PLAYWRIGHT_BROWSERS_PATH'])
