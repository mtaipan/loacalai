"""Isolated providers/inference: search packets, filters and independent mode rights."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,socket,subprocess,threading,time,urllib.request,urllib.parse,urllib.error,uuid,sqlite3,hashlib
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('search-test-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True);(root/'data/workspace/fixture.txt').write_text('LOCAL_PROJECT_EVIDENCE')
model='qwen2.5:3b';foundation='hf.co/DevQuasar/huihui-ai.Foundation-Sec-8B-abliterated-GGUF:Q4_K_M';provider_calls=[];inferences=[];shows=[];provider_fails=False;waiting=threading.Event();release_wait=threading.Event()
class Fixture(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,value):
        data=json.dumps(value).encode()+b'\n';self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def do_GET(self):
        if self.path.startswith('/search?'):
            params=urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query);provider_calls.append(params)
            if params.get('q')==['WAIT_FOR_CANCEL']:waiting.set();release_wait.wait(10);return
            if provider_fails:self.send_error(503);return
            rows=[{'url':site+f'/page?number={i}','title':f'SOURCE_{i} <b>literal</b>','content':f'EXCERPT_{i}'} for i in range(20)]
            rows.extend([{'url':site+'/page?number=0&utm_source=duplicate#fragment','title':'Duplicate','content':'duplicate'},{'url':'https://127.0.0.1.evil.invalid/','title':'Domain suffix attack','content':'must be excluded'}])
            return self.reply({'results':rows,'paging':True})
        if self.path.startswith('/page'):
            data=b'<html><title>Primary fixture</title><p>ACTUAL_READ_SOURCE</p></html>';self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(data);return
        self.reply({'models':[{'name':model},{'name':foundation}]} if self.path=='/api/tags' else {'models':[]})
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if self.path=='/api/show':shows.append(body);return self.reply({'model_info':{'qwen.context_length':65536},'thinking':{'values':[False]}})
        if self.path=='/api/generate' and body.get('prompt'):
            inferences.append(body);return self.reply({'response':'Specialist advice from supplied evidence only.','done':True})
        if self.path!='/api/chat' or not body.get('messages'):return self.reply({'done':True})
        inferences.append(body);messages=body['messages'];tools=[m for m in messages if m.get('role')=='tool'];names={tool['function']['name'] for tool in body.get('tools',[])};text='\n'.join(m.get('content','') for m in messages if m['role']=='user')
        def call(name,args):return self.reply({'message':{'role':'assistant','content':'','tool_calls':[{'function':{'name':name,'arguments':args}}]},'done':True})
        if 'MIXED_PROJECT' in text or 'CONCEPT_OPERATION' in text:
            if not any(m.get('tool_name')=='read_file' for m in tools):return call('read_file',{'path':'fixture.txt'})
            assert 'LOCAL_PROJECT_EVIDENCE' in tools[-1]['content'] or any('LOCAL_PROJECT_EVIDENCE' in m['content'] for m in tools)
        else:
            assert not names.intersection({'write_file','read_file','run_command','browser_action','write_stdin'}),names
            if 'read_search_plan' in names:
                if not any(m.get('tool_name')=='read_search_plan' for m in tools):return call('read_search_plan',{})
                if not any(m.get('tool_name')=='update_search_plan' for m in tools):return call('update_search_plan',{'steps':[{'title':'Compare primary sources','status':'in_progress'}]})
            if 'HOSTILE_CHAT' in text and not any(m.get('tool_name')=='read_file' for m in tools):return call('read_file',{'path':'fixture.txt'})
            if 'HOSTILE_CHAT' not in text and not any(m.get('tool_name')=='fetch_url' for m in tools):return call('fetch_url',{'url':site+'/page?number=0'})
        if 'CITATION_CASE' in text:
            source=json.loads(next(m['content'] for m in tools if m.get('tool_name')=='fetch_url'))['source_id'];content=f'Actual claim [cite:{source}]. Missing [cite:S-'+('0'*64)+']. Unobserved [reference](https://unobserved.invalid/reference).'
        else:content='SEARCH_FLOW_DONE. Actual source '+site+'/page?number=0'
        self.reply({'message':{'role':'assistant','content':content},'done':True})
fixture=ThreadingHTTPServer(('127.0.0.1',0),Fixture);site=f'http://127.0.0.1:{fixture.server_port}';threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token=''
settings={'workspace':str(root/'data/workspace'),'agent_model':model,'chat_model':model,'team_model':model,'search_model':model,'cpu_only':True,'source':'en','target':'ru','theme':'dark','web':{'provider':'searxng','searxng_url':site,'reranker_enabled':True},'assistant':{'agent_temperature':.13,'chat_temperature':.67}}
(root/'data/settings.json').write_text(json.dumps(settings));(root/'runtime').mkdir();subprocess.run(['powershell.exe','-NoProfile','-Command','New-Item -ItemType Junction -Path $env:TEST_PY_LINK -Target $env:TEST_PY_TARGET | Out-Null'],env={**os.environ,'TEST_PY_LINK':str(root/'runtime/python'),'TEST_PY_TARGET':str(project/'runtime/python')},check=True);(root/'runtime/web_worker.py').write_bytes((project/'runtime/web_worker.py').read_bytes())
log=(root/'test.log').open('w');process=None
def call(path,body=None):return http.open(urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=40)
def read(path,body=None):
    with call(path,body) as response:return json.load(response)
def start():
    global process,token
    process=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':site},stdout=log,stderr=log)
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(encoding='utf-8'))
        try:token=read('/api/bootstrap')['token'];return
        except Exception:time.sleep(.05)
    raise AssertionError('Server did not start')
def stop():
    try:read('/api/shutdown',{})
    except Exception:pass
    try:process.wait(timeout=8)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
def turn(query,mode='search',options=None,chat=None,network='search',allow_error=False,attachments=None):
    chat=chat or read('/api/chats',{'mode':mode});body={'conversation':chat['id'],'content':query,'mode':mode,'web':network,'source':'en','target':'ru','attachments':[]}
    if options is not None:body['search']=options
    if attachments:body['attachments']=attachments
    with call('/api/chat',body) as response:
        for line in response:
            if line.startswith(b'data:'):
                event=json.loads(line[5:]);assert allow_error or event['type']!='error',event;assert event['type']!='approval',event
    return read('/api/chats/'+chat['id'])
try:
    start();options={'depth':'results','focus':'news','period':'week','language':'ru','region':'RU','domains':['127.0.0.1']};query='Exact fixture query'
    chat=turn(query,options=options);answer=chat['messages'][-1];result=answer['diagnostics']['search']['result'];assert answer.get('model') is None and not inferences and not shows
    assert len(result['results'])==8 and result['page_result_count']==20 and result['next_result_offset']==8 and result['warnings'];assert provider_calls[-1]['categories']==['news'] and provider_calls[-1]['language']==['ru-RU'] and 'time_range' not in provider_calls[-1]
    source=result['page_source']['id'];first_calls=len(provider_calls);stop();start()
    chat=turn(query,options={**options,'page_source':source,'result_offset':8},chat=chat);result=chat['messages'][-1]['diagnostics']['search']['result'];assert result['results'][0]['title'].startswith('SOURCE_8') and len(provider_calls)==first_calls
    chat=turn(query,options={**options,'page_source':source,'result_offset':16},chat=chat);result=chat['messages'][-1]['diagnostics']['search']['result'];assert len(result['results'])==4 and result['next_result_offset'] is None and result['next_page']==2 and not inferences and not shows
    foreign=turn(query,options={**options,'page_source':source,'result_offset':8},allow_error=True);assert foreign['messages'][-1]['tools'][0]['status']=='failed';assert len(provider_calls)==first_calls
    print('PASS results bypass LLM/reranker; full provider page, dedup, strict domains, disclosed filters, cached packet restart and cross-task denial')
    boundary='SearchMaterials';material=('MATERIAL_NEEDLE source text\n'*40000).encode();body=(f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="local.txt"\r\nContent-Type: text/plain\r\n\r\n'.encode()+material+f'\r\n--{boundary}--\r\n'.encode())
    with http.open(urllib.request.Request(base+'/api/upload',data=body,headers={'Content-Type':'multipart/form-data; boundary='+boundary,'X-Studio-Token':token}),timeout=10) as response:attachment=json.load(response)['id']
    local=turn('MATERIAL_NEEDLE',options={'depth':'results','focus':'materials'},network='off',attachments=[attachment]);result=local['messages'][-1]['diagnostics']['search']['result'];assert result['has_more'] and len(result['results'])==32
    local=turn('MATERIAL_NEEDLE',options={'depth':'results','focus':'materials','after':result['next_after'],'snapshot':result['snapshot']},network='off',chat=local);result=local['messages'][-1]['diagnostics']['search']['result'];assert result['results'] and not inferences and not shows and len(provider_calls)==first_calls
    print('PASS local materials use paged keyword retrieval with no external requests or LLM, including large uploaded sources')
    for mode in ['agent','team']:
        for network in ['search','research']:
            result=turn('MIXED_PROJECT: search docs and fix the project',mode=mode,network=network)['messages'][-1];assert any(tool['name']=='read_file' and tool['status']=='succeeded' for tool in result['tools'])
    for text in ['Что означает ошибка? Исправьте проект CONCEPT_OPERATION','Ты знаешь этот проект? Обнови зависимости CONCEPT_OPERATION']:
        assert any(tool['name']=='read_file' and tool['status']=='succeeded' for tool in turn(text,mode='agent',network='off')['messages'][-1]['tools'])
    print('PASS Agent/Team search/research retains project actions, Russian operation forms retain tools')
    for depth in ['quick','detailed','deep']:
        answer=turn('Read primary fixture sources '+site+'/page?number=0',options={'depth':depth,'focus':'web'})['messages'][-1];assert 'SEARCH_FLOW_DONE' in answer['content'] and any(tool['name']=='fetch_url' and tool['status']=='succeeded' for tool in answer['tools']);assert answer['diagnostics']['sampling']['chat_temperature']==.67
        if depth!='quick':assert any(tool['name']=='update_search_plan' and tool['status']=='succeeded' for tool in answer['tools'])
    answer=turn('HOSTILE_CHAT search this topic',mode='chat')['messages'][-1];assert any(tool['name']=='read_file' and tool['status']=='failed' for tool in answer['tools'])
    print('PASS search depths use selected model, reading and chat sampling; Chat cannot gain OS actions through retrieval')
    cited=turn('CITATION_CASE read the primary source '+site+'/page?number=0',options={'depth':'quick','domains':['127.0.0.1']});answer=cited['messages'][-1];report=answer['diagnostics']['citations'];assert report['resolved']==1 and report['missing_ids']==1 and report['unverified_links']==1 and report['semantic_support_verified'] is False
    assert '[cite:' not in answer['content'] and '[1]('+site+'/page?number=0)' in answer['content'] and '[источник не найден]' in answer['content']
    run=answer['diagnostics']['execution']['run_id'];sources=read('/api/runs/'+run+'/sources');assert len(sources['items'])==20 and any(source['state']=='received' for source in sources['items']);assert not sources['indexing']['has_more']
    downloaded=read('/api/artifacts/'+report['artifact']['id']);assert json.loads(downloaded['text'])['semantic_support_verified'] is False
    stop()
    # Own fixture only: simulate schema-7 artifacts with no derived source index,
    # including one malformed historical result. No primary artifact is erased.
    conn=sqlite3.connect(root/'data/tasks.sqlite3');conn.executescript('DROP TABLE web_sources; DROP TABLE web_source_receipts; DROP TABLE web_source_index_errors; PRAGMA user_version=7;')
    raw='not-json';digest=hashlib.sha256(raw.encode()).hexdigest();broken=str(uuid.uuid4());conn.execute('INSERT OR IGNORE INTO artifact_bodies VALUES (?,?,?)',(digest,len(raw),len(raw)));conn.execute('INSERT OR IGNORE INTO artifact_chunks(hash,part,char_start,characters,text) VALUES (?,0,0,?,?)',(digest,len(raw),raw));conn.execute('INSERT INTO artifacts(id,run,kind,metadata,hash,created) VALUES (?,?,?,?,?,?)',(broken,run,'search_results','{}',digest,'2020-01-01T00:00:00Z'));conn.commit();conn.close()
    start();recovered=read('/api/runs/'+run+'/sources');assert len(recovered['items'])==20 and recovered['indexing']['errors']['count']==1 and not recovered['indexing']['has_more'];assert read('/api/artifacts/'+broken)['text']==raw
    assert read('/api/runs/'+run+'/sources?retry_errors=true')['indexing']['errors']['count']==1
    print('PASS source receipts survive restart/window independence; structured citations resolve observed IDs, unknown IDs/links stay unverified, semantic support is never asserted')
    provider_fails=True;before=len(inferences);answer=turn('Provider failure',options={'depth':'results'},allow_error=True)['messages'][-1];assert answer['tools'][0]['status']=='failed' and len(inferences)==before;assert answer['diagnostics']['execution']['failed']==1
    print('PASS provider error persists a failed tool, no fabricated answer or fallback inference')
    provider_fails=False
    cancelled=read('/api/chats',{'mode':'search'});accepted=read('/api/runs',{'conversation':cancelled['id'],'content':'WAIT_FOR_CANCEL','mode':'search','web':'search','source':'en','target':'ru','attachments':[],'search':{'depth':'results'}});assert waiting.wait(5)
    read('/api/stop/'+accepted['run_id'],{})
    for _ in range(100):
        if read('/api/runs/'+accepted['run_id'])['status']=='cancelled':break
        time.sleep(.05)
    else:raise AssertionError('Search did not stop')
    assert read('/api/chats/'+cancelled['id'])['messages'][-1]['tools'][0]['status']=='cancelled';release_wait.set()
    print('PASS cancellation interrupts the waiting provider worker and persists cancelled rather than failed/successful action')
    # Own headless browser only; no user session, clipboard or inference.
    from playwright.sync_api import sync_playwright
    with sync_playwright() as pw:
        browser=pw.chromium.launch(channel='msedge',headless=True);page=browser.new_page(viewport={'width':320,'height':760});errors=[];page.on('pageerror',lambda error:errors.append(str(error)));page.goto(base);page.wait_for_selector('[data-mode="search"]');page.locator('[data-mode="search"]').click();page.locator('#search-preferences summary').click();page.locator('#search-depth').select_option('results');page.locator('#search-focus').select_option('news');page.locator('#search-domains').fill('127.0.0.1');page.locator('#search-domains').press('Tab');page.locator('#prompt').fill('UI fixture results');page.locator('#send').click();page.wait_for_selector('.search-result',timeout=20000);assert page.locator('.search-result').count()==8 and page.locator('.search-result b').count()==0
        assert page.evaluate('document.documentElement.scrollWidth<=innerWidth'),page.evaluate('document.documentElement.scrollWidth');assert not errors,errors;page.screenshot(path=str(root/'search-mobile.png'),full_page=True)
        page.locator('[data-source-index]').last.click();page.wait_for_selector('.source-index-row');assert page.locator('.source-index-row').count()==20 and page.locator('.source-index-row b').count()==0;assert not page.locator('.source-retry').is_visible();assert page.locator('.source-close').bounding_box()['y']<760;page.screenshot(path=str(root/'source-index-mobile.png'),full_page=True);page.locator('.source-close').click();browser.close()
    print('PASS Edge 320px search controls, literal source cards and no JS errors')
finally:
    release_wait.set()
    if process:stop()
    fixture.shutdown();log.close()
