"""Long fake tool run: bounded live view, full durable paging, Edge/reload."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,socket,subprocess,threading,time,urllib.request,uuid
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('timeline-test-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True)
(root/'data/workspace/source.txt').write_text(''.join(f'LINE_{i}\n' for i in range(1,101)))
model='qwen2.5:3b';calls=0;holding=threading.Event();release=threading.Event();seen_checkpoint=False
class Fixture(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,value):
        data=json.dumps(value).encode()+b'\n'
        try:self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
        except ConnectionError:pass
    def do_GET(self):self.reply({'models':[{'name':model}]} if self.path=='/api/tags' else {'models':[]})
    def do_POST(self):
        global calls,seen_checkpoint
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if self.path=='/api/show':return self.reply({'model_info':{'qwen.context_length':65536},'thinking':{'values':[False]}})
        if self.path!='/api/chat' or not body.get('messages'):return self.reply({'done':True,'status':'success'})
        seen_checkpoint|=any(m.get('_run_checkpoint') for m in body['messages']);index=calls;calls+=1
        if index<80:call=('read_file',{'path':'source.txt','start_line':index+1,'max_lines':1})
        elif index==80:call=('read_run_log',{})
        else:
            holding.set();release.wait(45)
            return self.reply({'message':{'role':'assistant','content':'TIMELINE_COMPLETE'},'done':True})
        self.reply({'message':{'role':'assistant','content':'','thinking':f'THOUGHT_{index} <button data-action="settings">literal</button> '+('reason '*300),'tool_calls':[{'function':{'name':call[0],'arguments':call[1]}}]},'done':True})
fixture=ThreadingHTTPServer(('127.0.0.1',0),Fixture);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
base=f'http://127.0.0.1:{port}';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token=''
(root/'data/settings.json').write_text(json.dumps({'workspace':str(root/'data/workspace'),'agent_model':model,'chat_model':model,'cpu_only':True,'source':'en','target':'ru','theme':'dark','context':{'window_tokens':16384}}))
log=(root/'test.log').open('w');process=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
def read(path,body=None):
    with http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=10) as response:return json.load(response)
try:
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(encoding='utf-8'))
        try:token=read('/api/bootstrap')['token'];break
        except Exception:time.sleep(.05)
    chat=read('/api/chats',{});run=str(uuid.uuid4());read('/api/runs',{'request_id':run,'conversation':chat['id'],'content':'TIMELINE_FIXTURE inspect the project file over many observations','mode':'agent','web':'off','source':'en','target':'ru','attachments':[]})
    assert holding.wait(30),(calls,read('/api/runs/'+run),read('/api/chats/'+chat['id']))
    snapshot=read('/api/session');view=snapshot['runs'][0]['state']
    assert len(view['streamTools'])==48 and len(view['streamThinking'].encode())<=16384 and view['thinkingOffset']>0,view.keys()
    assert seen_checkpoint,'long model context never checkpointed'
    ids=set();before=None
    while True:
        page=read('/api/runs/'+run+'/tools'+('?before='+str(before) if before else ''))
        for row in page['tools']:assert row['tool']['id'] not in ids;ids.add(row['tool']['id'])
        before=page['next_before']
        if not before:break
    assert len(ids)==81,len(ids)
    thoughts=[];before=None
    while True:
        page=read('/api/runs/'+run+'/text?kind=thinking'+('&before='+str(before) if before else ''));thoughts[:0]=[row['text'] for row in page['items']];before=page['next_before']
        if not before:break
    assert len(thoughts)==81 and thoughts[0].startswith('THOUGHT_0 ') and thoughts[-1].startswith('THOUGHT_80 ')
    from playwright.sync_api import sync_playwright
    with sync_playwright() as playwright:
        browser=playwright.chromium.launch(channel='msedge',headless=True);page=browser.new_page(viewport={'width':1280,'height':900});errors=[];page.on('pageerror',lambda error:errors.append(str(error)));page.goto(base);page.wait_for_selector('#prompt');page.evaluate('(id)=>switchConversation(id)',chat['id']);page.wait_for_selector('#stream-history [data-tool-history]');page.reload();page.wait_for_selector('#stream-history [data-tool-history]')
        assert page.locator('#stream-tools > [data-tool-key]').count()<=48
        page.locator('#stream-history [data-tool-history]').click();page.wait_for_selector('.run-history-content > [data-tool-key]');assert page.locator('.run-history-content > [data-tool-key]').count()==32
        page.locator('.history-older').click();page.wait_for_function('()=>document.querySelector(".history-newer")?.disabled===false');assert page.locator('.run-history-content > [data-tool-key]').count()==32
        page.set_viewport_size({'width':320,'height':700});page.screenshot(path=str(root/'timeline-mobile.png'));assert page.locator('.history-close').bounding_box()['y']<700
        page.locator('.history-close').click();page.locator('#stream-history [data-kind="thinking"]').click();page.wait_for_selector('.run-history-content pre');assert page.locator('.run-history-content button').count()==0
        page.locator('.history-close').click();release.set()
        page.wait_for_function('()=>!state.busy',timeout=15000);assert not errors,errors;browser.close()
    saved=read('/api/chats/'+chat['id'])['messages'][-1]
    assert 'TIMELINE_COMPLETE' in saved['content'] and len(saved['tools'])==48
    assert saved['diagnostics']['execution']['actions']==81 and saved['diagnostics']['execution']['details_partial'] is True
    assert len(saved['diagnostics']['thinking'].encode())<=16384 and saved['diagnostics']['thinking_partial'] is True
    print('PASS 81 tool actions and all thinking remain pageable; live/reload/final windows stay bounded and full evidence counts survive model checkpoints')
    print('PASS real Edge journal navigation, literal thinking text and footer at 320 px')
finally:
    release.set()
    try:read('/api/shutdown',{})
    except Exception:pass
    try:process.wait(timeout=8)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
    fixture.shutdown();log.close()
