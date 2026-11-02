"""Large uploads: full streamed review and readonly Chat retrieval via source IDs."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,socket,subprocess,threading,time,urllib.request,uuid
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('material-review-test-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True)
original=''.join(f'ROW_{i:05} function(value) returns value;\r\n' for i in range(18000))+'BUG_NEEDLE at the end\r\n';assert len(original.encode())>500000
model='qwen2.5:3b';fragments=[];review_calls=0;chat_tools=[]
def result(message):
    text=message['content']
    if text.startswith('STATUS: succeeded\nSOURCE_TOOL: '):
        metadata=json.loads(next(line[len('SOURCE_PAGE: '):] for line in text.splitlines() if line.startswith('SOURCE_PAGE: ')))
        metadata['text']=text.split('\nSOURCE_TEXT_BEGIN\n',1)[1].rsplit('\nSOURCE_TEXT_END',1)[0];return metadata
    value=json.loads(message['content']);return json.loads(value['result']) if isinstance(value.get('result'),str) else value.get('result',value)
class Fixture(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,value):
        data=json.dumps(value).encode()+b'\n'
        try:self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
        except ConnectionError:pass
    def do_GET(self):self.reply({'models':[{'name':model}]} if self.path=='/api/tags' else {'models':[]})
    def do_POST(self):
        global review_calls
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if self.path=='/api/show':return self.reply({'model_info':{'qwen.context_length':65536},'thinking':{'values':[False]}})
        if self.path!='/api/chat' or not body.get('messages'):return self.reply({'done':True,'status':'success'})
        messages=body['messages'];user=max(i for i,m in enumerate(messages) if m['role']=='user');request=messages[user]['content'];tools=[m for m in messages[user+1:] if m['role']=='tool'];call=None
        if 'REVIEW_REGISTRY_FIXTURE' in request:
            review_calls+=1
            if 'Source fragment ' in request:
                fragments.append(request.split('Report concrete findings briefly, with exact supporting excerpts.\n',1)[1]);answer='Observed this exact source fragment; no tests executed.'
            elif 'Synthesize notes from ' in request:answer='REGISTRY_REVIEW_COMPLETE: observations only; no tests executed.'
            else:answer='Merged partial notes; cross-fragment relations unverified.'
            return self.reply({'message':{'role':'assistant','content':answer},'done':True})
        if 'QUESTION_REGISTRY_FIXTURE' in request:
            names={d['function']['name'] for d in body.get('tools',[])};assert names<=set(['read_task','read_run_log','read_artifact','search_artifacts','read_sources']),names;chat_tools.append(names)
            if not tools:call=('search_artifacts',{'query':'BUG_NEEDLE'})
            else:
                reads=[m for m in tools if m.get('tool_name')=='read_artifact']
                if not reads:
                    found=result(next(m for m in tools if m.get('tool_name')=='search_artifacts'))['results'][0];call=('read_artifact',{'id':found['artifact_id'],'offset':found['offset']})
                else:
                    page=result(reads[-1])
                    if 'BUG_NEEDLE' not in page['text']:call=('read_artifact',{'id':page['artifact']['id'],'offset':page['next_offset']})
        message={'role':'assistant','content':'REGISTRY_QUESTION_COMPLETE: found BUG_NEEDLE in the saved source.'}
        if call:message={'role':'assistant','content':'','tool_calls':[{'function':{'name':call[0],'arguments':call[1]}}]}
        self.reply({'message':message,'done':True})
fixture=ThreadingHTTPServer(('127.0.0.1',0),Fixture);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
base=f'http://127.0.0.1:{port}';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token=''
(root/'data/settings.json').write_text(json.dumps({'workspace':str(root/'data/workspace'),'agent_model':model,'chat_model':model,'cpu_only':True,'source':'en','target':'ru','theme':'dark'}))
log=(root/'test.log').open('w');process=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
def call(path,body=None):return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=60)
def read(path,body=None):
    with call(path,body) as response:return json.load(response)
def turn(content,attachment):
    chat=read('/api/chats',{})
    with call('/api/chat',{'conversation':chat['id'],'content':content,'mode':'chat','web':'off','source':'en','target':'ru','attachments':[attachment]}) as response:
        for line in response:
            if line.startswith(b'data:'):assert json.loads(line[5:])['type']!='error',line
    return read('/api/chats/'+chat['id'])['messages'][-1]
try:
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(encoding='utf-8'))
        try:token=read('/api/bootstrap')['token'];break
        except Exception:time.sleep(.05)
    boundary='MaterialSourceBoundary';body=(f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="large.txt"\r\nContent-Type: text/plain\r\n\r\n'.encode()+original.encode()+f'\r\n--{boundary}--\r\n'.encode())
    with http.open(urllib.request.Request(base+'/api/upload',data=body,headers={'Content-Type':'multipart/form-data; boundary='+boundary,'X-Studio-Token':token}),timeout=10) as response:attachment=json.load(response)['id']
    answer=turn('REVIEW_REGISTRY_FIXTURE review the attached source',attachment);report=answer['diagnostics']['material_review']
    assert 'REGISTRY_REVIEW_COMPLETE' in answer['content'] and ''.join(fragments)==original
    assert answer['diagnostics']['source_parts']==len(fragments)>48 and report['inference_count']==review_calls and report['inferences_partial'] is True and len(report['inferences'])==48
    assert report['source_coverage'][0]['complete'] is True and report['source_coverage'][0]['characters_read']==len(original)
    assert len(answer['tools'][0]['result'])<8192 and answer['tools'][0]['material_artifact']['characters']==len(original)
    answer=turn('QUESTION_REGISTRY_FIXTURE locate BUG_NEEDLE in the attached source',attachment)
    assert 'REGISTRY_QUESTION_COMPLETE' in answer['content'] and chat_tools
    assert any(t['name']=='read_artifact' and 'BUG_NEEDLE' in result({'content':json.dumps({'result':t['result']})})['text'] for t in answer['tools'])
    from playwright.sync_api import sync_playwright
    with sync_playwright() as playwright:
        browser=playwright.chromium.launch(channel='msedge',headless=True);page=browser.new_page(viewport={'width':320,'height':700});errors=[];page.on('pageerror',lambda error:errors.append(str(error)));page.goto(base);page.wait_for_selector('#prompt')
        source=answer['diagnostics']['material_sources'][0];conversation=read('/api/runs/'+source['run'])['conversation'];page.evaluate('(id)=>switchConversation(id)',conversation)
        page.locator('[data-material-sources]').click();page.wait_for_selector('.material-list .button');page.locator('.material-list .button').first.click();page.wait_for_function('(text)=>document.querySelector("#artifact-dialog .artifact-content")?.textContent===text',arg=original[:8000])
        page.screenshot(path=str(root/'material-catalog-mobile.png'));assert page.locator('.artifact-close').bounding_box()['y']<700;assert not errors,errors;browser.close()
    print('PASS uploaded source above 500 KB reaches full streaming review exactly once; bounded inference history preserves complete coverage and saved notes')
    print('PASS Chat retrieves late source text through scoped artifacts without filesystem, command or browser-action capabilities')
finally:
    try:read('/api/shutdown',{})
    except Exception:pass
    try:process.wait(timeout=8)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
    fixture.shutdown();log.close()
