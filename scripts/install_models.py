"""Download only models explicitly checked in the installer."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.request

root=Path(sys.argv[1]).resolve()
queue=json.loads((root/'data/installer-models.json').read_text(encoding='utf-8'))
http=urllib.request.build_opener(urllib.request.ProxyHandler({}))
base='http://127.0.0.1:3210'
process=None
try:
    try:
        app=json.load(http.open(base+'/api/bootstrap',timeout=2))
    except Exception:
        process=subprocess.Popen([str(root/'LocalGemmaStudio.exe'),'--server'],cwd=root,
            env={**os.environ,'LOCAL_GEMMA_HOME':str(root)},creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        for _ in range(120):
            try:
                app=json.load(http.open(base+'/api/bootstrap',timeout=1));break
            except Exception:
                time.sleep(.25)
        else:
            raise RuntimeError('Studio did not start')
    if app.get('app')!='Local Gemma Studio': raise RuntimeError('Unexpected app on Studio port')
    if not app.get('root') or Path(app['root']).resolve()!=root:
        raise RuntimeError('Close the Studio instance from the other installation first')
    token=app['token']
    for item in queue:
        req=urllib.request.Request(base+'/api/models/pull',data=json.dumps(item).encode(),headers={
            'Content-Type':'application/json','X-Studio-Token':token})
        previous=''
        with http.open(req,timeout=7200) as response:
            for line in response:
                if not line.startswith(b'data:'): continue
                event=json.loads(line[5:])
                if event['type']=='error': raise RuntimeError(event['message'])
                if event['type']!='progress': continue
                p=event['progress'];text=item['model']+': '+p.get('status','')
                if p.get('total'): text+=' '+str(100*p.get('completed',0)//p['total'])+'%'
                if text!=previous: print(text,flush=True);previous=text
finally:
    if process:
        process.terminate();process.wait(timeout=15)
