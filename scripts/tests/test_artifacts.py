"""Immutable observations, UTF-8 paging, scoped retrieval and real Edge inspector."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import hashlib,json,os,socket,subprocess,threading,time,urllib.request,uuid
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('artifact-test-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True)
model='qwen2.5:3b';artifact_id=None
source=''.join(f'Line {i}: Материал 🦀 INDEX_NEEDLE <button data-action="settings">literal</button> '+('source-data '*10)+'\r\n' for i in range(1,301))
(root/'data/workspace/long-source.txt').write_bytes(source.encode())
def payload(message):
    value=json.loads(message['content']);result=value.get('result',value)
    if isinstance(result,str):
        try:return json.loads(result)
        except ValueError:return result
    return result
class Fixture(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,body):
        data=json.dumps(body).encode()+b'\n';self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def do_GET(self):self.reply({'models':[{'name':model}]} if self.path=='/api/tags' else {'models':[]})
    def do_POST(self):
        global artifact_id
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if self.path=='/api/show':return self.reply({'model_info':{'qwen.context_length':65536},'thinking':{'values':[False]}})
        if self.path!='/api/chat':return self.reply({'done':True,'status':'success'})
        messages=body['messages'];last=max(i for i,m in enumerate(messages) if m.get('role')=='user');text=messages[last].get('content','');tools=[m for m in messages[last+1:] if m.get('role')=='tool'];call=None
        if 'READ_ARTIFACT_FIXTURE' in text and not tools:call=('read_file',json.dumps({'path':'long-source.txt','max_lines':500}))
        elif 'SEARCH_ARTIFACT_FIXTURE' in text:
            if not tools:call=('search_artifacts',{'query':'INDEX_NEEDLE'})
            elif not any(m.get('tool_name')=='read_artifact' for m in tools):
                found=payload(next(m for m in tools if m.get('tool_name')=='search_artifacts'));artifact_id=found['results'][0]['artifact_id'];call=('read_artifact',{'id':artifact_id})
        elif 'FOREIGN_ARTIFACT_FIXTURE' in text and not tools:call=('read_artifact',{'id':artifact_id})
        message={'role':'assistant','content':'The supplied observation was processed. No tests were executed.'}
        if call:message={'role':'assistant','content':'','tool_calls':[{'function':{'name':call[0],'arguments':call[1]}}]}
        self.reply({'done':True,'done_reason':'stop','message':message})
fixture=ThreadingHTTPServer(('127.0.0.1',0),Fixture);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token=''
(root/'data/settings.json').write_text(json.dumps({'workspace':str(root/'data/workspace'),'agent_model':model,'chat_model':model,'cpu_only':True,'source':'en','target':'ru','theme':'dark'}))
log=(root/'test.log').open('w');process=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
def call(path,body=None):return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=30)
def read(path,body=None):
    with call(path,body) as response:return json.load(response)
def turn(chat,text):
    with call('/api/chat',{'conversation':chat['id'],'content':text,'mode':'agent','source':'en','target':'ru','web':'off'}) as response:
        for line in response:
            if line.startswith(b'data:'):assert json.loads(line[5:])['type']!='error',line
    return read('/api/chats/'+chat['id'])
try:
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(encoding='utf-8'))
        try:token=read('/api/bootstrap')['token'];break
        except Exception:time.sleep(.1)
    else:raise AssertionError('server startup failed')
    chat=turn(read('/api/chats',{}),'READ_ARTIFACT_FIXTURE read long-source.txt in the project')
    tool=next(tool for tool in chat['messages'][-1]['tools'] if tool['name']=='read_file');assert tool['status']=='succeeded',tool
    assert isinstance(tool['arguments'],dict),'JSON-string arguments must normalize before path scope checks'
    assert tool['result_preview_only'] and len(tool['result'].encode())<16384
    descriptor=tool['result_artifact'];artifact_id=descriptor['id'];pieces=[];offset=0
    while True:
        page=read('/api/artifacts/'+artifact_id+'?offset='+str(offset));pieces.append(page['text'])
        if not page['has_more']:break
        offset=page['next_offset']
    original=''.join(pieces);observed=json.loads(original);assert observed['text']==''.join(source.splitlines(keepends=True)[:observed['end_line']]);assert observed['has_more'], 'the saved page must retain incomplete source coverage'
    assert hashlib.sha256(original.encode()).hexdigest()==descriptor['sha256']
    (root/'data/workspace/long-source.txt').write_text('Changed after capture',encoding='utf-8')
    with call('/api/artifacts/'+artifact_id+'/download') as response:
        raw=response.read();assert int(response.headers['Content-Length'])==len(raw)
    assert raw==original.encode()
    chat=turn(chat,'SEARCH_ARTIFACT_FIXTURE search saved sources for INDEX_NEEDLE and open the saved beginning')
    assert [t['name'] for t in chat['messages'][-1]['tools']]==['search_artifacts','read_artifact']
    foreign=turn(read('/api/chats',{}),'FOREIGN_ARTIFACT_FIXTURE try to read the other task source')
    assert foreign['messages'][-1]['tools'][0]['status']=='failed'
    assert 'INDEX_NEEDLE' not in foreign['messages'][-1]['tools'][0]['result']
    from playwright.sync_api import sync_playwright
    with sync_playwright() as playwright:
        browser=playwright.chromium.launch(channel='msedge',headless=True);page=browser.new_page(viewport={'width':1280,'height':900});errors=[];page.on('pageerror',lambda error:errors.append(str(error)));page.goto(base);page.wait_for_selector('#prompt');page.evaluate('(id)=>switchConversation(id)',chat['id'])
        card=page.locator('.tool-card').first;card.locator('summary').click();card.locator('[data-artifact]').first.click();page.wait_for_function('()=>document.querySelector(".artifact-content")?.textContent.includes("INDEX_NEEDLE")')
        assert page.locator('.artifact-content [data-action]').count()==0
        page.locator('.artifact-next').click();page.wait_for_function('()=>document.querySelector(".artifact-position").textContent.startsWith("Символы 8000")')
        page.set_viewport_size({'width':320,'height':700});assert page.evaluate('document.documentElement.scrollWidth<=innerWidth');footer=page.locator('#artifact-dialog>.dialog-footer').bounding_box();assert footer['y']+footer['height']<=700,footer
        page.screenshot(path=str(root/'artifact-mobile.png'));page.locator('.artifact-close').click();assert not errors,errors;browser.close()
    print('PASS large observations archive once, preserve full Unicode/CRLF text and SHA-256, survive source changes and download without truncation')
    print('PASS keyword retrieval and saved-source reads span runs but reject another task; JSON-string arguments normalize before scope checks')
    print('PASS Edge artifact inspector pages literal content at 320 px')
finally:
    try:call('/api/shutdown',{}).close()
    except Exception:pass
    try:process.wait(timeout=10)
    except subprocess.TimeoutExpired:process.terminate();process.wait(timeout=5)
    fixture.shutdown();log.close()
