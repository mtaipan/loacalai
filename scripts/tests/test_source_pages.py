"""Real agent byte continuation over UTF-16 long lines and explicit CP1251."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,socket,subprocess,threading,time,urllib.request,uuid
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('source-test-'+uuid.uuid4().hex[:8]);workspace=root/'data/workspace';workspace.mkdir(parents=True)
original='first\r\n'+('Привет 🦀漢字 '*9000)+'\r\nlast';(workspace/'source.txt').write_bytes(b'\xff\xfe'+original.encode('utf-16le'))
legacy='Привет, мир!\r\n';(workspace/'legacy.txt').write_bytes(legacy.encode('cp1251'));model='qwen2.5:3b'
class Fixture(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,value):
        data=json.dumps(value).encode()+b'\n'
        try:self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
        except ConnectionError:pass
    def do_GET(self):self.reply({'models':[{'name':model}]} if self.path=='/api/tags' else {'models':[]})
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if self.path=='/api/show':return self.reply({'model_info':{'qwen.context_length':65536},'thinking':{'values':[False]}})
        if self.path!='/api/chat' or not body.get('messages'):return self.reply({'done':True,'status':'success'})
        messages=body['messages'];user=max(i for i,m in enumerate(messages) if m['role']=='user');request=messages[user]['content'];results=[m for m in messages[user+1:] if m['role']=='tool']
        if 'INGEST_FIXTURE' in request:
            if results:return self.reply({'message':{'role':'assistant','content':'SOURCE_INGESTED'},'done':True})
            return self.reply({'message':{'role':'assistant','content':'','tool_calls':[{'function':{'name':'ingest_file','arguments':{'path':'source.txt'}}}]},'done':True})
        args=None
        if 'LEGACY_FIXTURE' in request:
            if not results:args={'path':'legacy.txt','encoding':'windows-1251'}
        elif not results:args={'path':'source.txt','max_lines':500}
        else:
            text=results[-1]['content']
            if not text.startswith('STATUS: succeeded'):
                try:value=json.loads(text)
                except ValueError:value={}
                if value.get('status')=='failed':return self.reply({'message':{'role':'assistant','content':'SOURCE_CHANGE_DETECTED'},'done':True})
                text=value.get('metadata','')
            source=next((line[len('SOURCE_PAGE: '):] for line in text.splitlines() if line.startswith('SOURCE_PAGE: ')),None)
            assert source,'agent context lost byte continuation metadata'
            page=json.loads(source)
            if page['next_offset'] is not None:
                args={'path':'source.txt','offset':page['next_offset'],'revision':page['revision'],'encoding':page['encoding'],'max_lines':500}
                if 'CHANGE_FIXTURE' in request:
                    with (workspace/'source.txt').open('ab') as f:f.write('changed'.encode('utf-16le'))
        if args is None:return self.reply({'message':{'role':'assistant','content':'SOURCE_COMPLETE'},'done':True})
        self.reply({'message':{'role':'assistant','content':'','tool_calls':[{'function':{'name':'read_file','arguments':json.dumps(args)}}]},'done':True})
fixture=ThreadingHTTPServer(('127.0.0.1',0),Fixture);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
base=f'http://127.0.0.1:{port}';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token=''
(root/'data/settings.json').write_text(json.dumps({'workspace':str(workspace),'agent_model':model,'chat_model':model,'cpu_only':True,'source':'en','target':'ru','theme':'dark'}))
log=(root/'test.log').open('w');process=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
def call(path,body=None):return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=15)
def read(path,body=None):
    with call(path,body) as response:return json.load(response)
def run(request):
    chat=read('/api/chats',{})
    with call('/api/chat',{'conversation':chat['id'],'content':request,'mode':'agent','web':'off','source':'en','target':'ru','attachments':[]}) as response:
        for line in response:
            if line.startswith(b'data:'):assert json.loads(line[5:])['type']!='error',line
    return read('/api/chats/'+chat['id'])['messages'][-1]
def observed(tool):
    with call('/api/artifacts/'+tool['result_artifact']['id']+'/download') as response:return json.load(response)
try:
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(encoding='utf-8'))
        try:token=read('/api/bootstrap')['token'];break
        except Exception:time.sleep(.05)
    answer=run('SOURCE_FIXTURE read source.txt completely')
    assert 'SOURCE_COMPLETE' in answer['content'] and all(t['status']=='succeeded' for t in answer['tools']),answer['content']
    pages=[observed(tool) for tool in answer['tools']];assert len(pages)>8 and ''.join(page['text'] for page in pages)==original
    assert pages[0]['encoding']=='utf-16le' and pages[0]['bom_bytes']==2
    assert any(page['partial_line'] for page in pages)
    assert all(a['end_byte']==b['start_byte'] for a,b in zip(pages,pages[1:]))
    assert pages[-1]['has_more'] is False and pages[-1]['end_byte']==(workspace/'source.txt').stat().st_size
    prepared=run('INGEST_FIXTURE prepare source.txt as a saved material')
    tool=prepared['tools'][0];assert tool['name']=='ingest_file' and tool['status']=='succeeded' and 'SOURCE_INGESTED' in prepared['content'],prepared
    source=tool['material_artifact'];assert source['metadata']['coverage']['complete'] is True
    with call('/api/artifacts/'+source['id']+'/download') as response:assert response.read().decode()==original
    from playwright.sync_api import sync_playwright
    with sync_playwright() as playwright:
        browser=playwright.chromium.launch(channel='msedge',headless=True);page=browser.new_page(viewport={'width':320,'height':700});errors=[];page.on('pageerror',lambda error:errors.append(str(error)));page.goto(base);page.wait_for_selector('#prompt')
        conversation=read('/api/runs/'+source['run'])['conversation'];page.evaluate('(id)=>switchConversation(id)',conversation);page.locator('.tool-card summary').first.click();page.locator('[data-artifact="'+source['id']+'"]').click();page.wait_for_selector('#artifact-dialog .artifact-content')
        page.wait_for_function('(text)=>document.querySelector("#artifact-dialog .artifact-content")?.textContent===text',arg=original[:8000]);page.screenshot(path=str(root/'material-mobile.png'));assert page.locator('.artifact-close').bounding_box()['y']<700;assert not errors,errors;browser.close()
    answer=run('LEGACY_FIXTURE read legacy.txt');assert observed(answer['tools'][0])['text']==legacy
    answer=run('CHANGE_FIXTURE read source.txt completely');assert answer['tools'][0]['status']=='succeeded' and answer['tools'][1]['status']=='failed' and 'SOURCE_CHANGE_DETECTED' in answer['content']
    assert observed(answer['tools'][0])['text']==pages[0]['text'],'source mutation changed an earlier observation'
    print('PASS agent follows byte/revision/encoding cursors through UTF-16 long lines without gaps; CP1251 is explicit and source changes reject stale continuation')
finally:
    try:read('/api/shutdown',{})
    except Exception:pass
    try:process.wait(timeout=8)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
    fixture.shutdown();log.close()
