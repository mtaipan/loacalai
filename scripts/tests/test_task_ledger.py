"""Task ledger API and bounded reader in an isolated home with fake inference."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import hashlib,json,os,socket,sqlite3,subprocess,threading,time,urllib.request,uuid
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('ledger-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True)
model='qwen2.5:3b'
class Engine(BaseHTTPRequestHandler):
 def log_message(self,*a):pass
 def reply(self,v):
  data=json.dumps(v).encode()+b'\n';self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
 def do_GET(self):self.reply({'models':[{'name':model}]})
 def do_POST(self):
  v=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))))
  self.reply({'model_info':{'qwen.context_length':32768},'thinking':{'values':[False]}} if self.path=='/api/show' else {'message':{'role':'assistant','content':'Fixture result.'},'done':True})
fixture=ThreadingHTTPServer(('127.0.0.1',0),Engine);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token='';log=(root/'test.log').open('w');process=None
(root/'data/settings.json').write_text(json.dumps({'workspace':str(root/'data/workspace'),'cpu_only':True,'source':'en','target':'ru','agent_model':model,'chat_model':model,'theme':'dark'}))
def read(path,body=None):
 with http.open(urllib.request.Request(base+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=20) as response:return json.load(response)
try:
 process=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
 for _ in range(120):
  if process.poll() is not None:raise AssertionError((root/'test.log').read_text(encoding='utf-8'))
  try:token=read('/api/bootstrap')['token'];break
  except Exception:time.sleep(.05)
 else:raise AssertionError('Fixture startup failed')
 chat=read('/api/chats',{'mode':'agent'});source='prefix 🦀\0\r\n'+'Required behavior 🦀\0\r\n'*900+'END';accepted=read('/api/runs',{'conversation':chat['id'],'content':source,'mode':'agent','source':'en','target':'ru','attachments':[],'web':'off'})
 for _ in range(200):
  if read('/api/runs/'+accepted['run_id'])['status'] in ('completed','failed'):break
  time.sleep(.05)
 task=read('/api/chats/'+chat['id']+'/tasks')[0]['id'];run=accepted['run_id'];ids=[]
 with sqlite3.connect(root/'data/tasks.sqlite3') as db:
  request=db.execute('SELECT sequence FROM task_requests WHERE run=?',(run,)).fetchone()[0]
  for n in range(25):
   id=str(uuid.uuid4());ids.append(id);text=source[n:]
   db.execute('INSERT INTO task_requirements(id,task,request,start,characters,source_hash,title,criterion,assessment,note,revision,created_run,updated_run,created,updated) VALUES(?,?,?,?,?,?,?,?,?,?,1,?,?,?,?)',(id,task,request,n,len(text),hashlib.sha256(text.encode()).hexdigest(),f'Requirement {n} <b>literal</b>','Preserve full request','reported_complete','Model assessment only',run,run,'2020','2020'))
 seen=set();cursor=None
 while True:
  value=read('/api/tasks/'+task+'/ledger/requirements'+('?before='+str(cursor) if cursor else ''));assert len(value['items'])<=8
  for item in value['items']:assert item['id'] not in seen;seen.add(item['id']);assert item['semantic_verification']=='unverified'
  if not value['has_more']:break
  cursor=value['next_before']
 assert len(seen)==25
 parts=[];offset=0
 while True:
  v=read('/api/tasks/'+task+'/requirements/'+ids[-1]+'/source?offset='+str(offset));assert len(v['text'])<=8000;parts.append(v['text'])
  if not v['has_more']:break
  offset=v['next_offset']
 assert ''.join(parts)==source[24:]
 try:read('/api/tasks/'+str(uuid.uuid4())+'/requirements/'+ids[-1]+'/source');raise AssertionError('Foreign source exposed')
 except urllib.error.HTTPError:pass
 print('PASS API pages all25 requirements; exact full Unicode/NUL/CRLF source packets; foreign task denied',flush=True)
 from playwright.sync_api import sync_playwright
 with sync_playwright() as pw:
  browser=pw.chromium.launch(channel='msedge',headless=True);page=browser.new_page(viewport={'width':390,'height':844});errors=[];page.on('pageerror',lambda e:errors.append(str(e)));page.goto(base);page.wait_for_selector('#prompt');page.evaluate('id=>selectChat(id)',chat['id']);page.evaluate('showTaskState()');page.wait_for_selector('#task-dialog select');page.locator('#task-dialog summary').filter(has_text='Требования задачи').click();page.wait_for_selector('.ledger-row');assert page.locator('.ledger-row').count()==8 and page.locator('.ledger-row b').count()==0
  page.locator('.ledger-row').first.get_by_role('button',name='Исходный фрагмент запроса').click();page.wait_for_function("document.querySelector('.ledger-source').textContent.length>400");first=page.locator('.ledger-source').first.text_content();page.locator('.ledger-row').first.get_by_role('button',name='Следующая часть',exact=True).click();page.wait_for_function('first=>document.querySelector(\'.ledger-source\').textContent!==first',arg=first);page.locator('.ledger-row').first.get_by_role('button',name='Предыдущая часть',exact=True).click();page.wait_for_function('first=>document.querySelector(\'.ledger-source\').textContent===first',arg=first)
  for _ in range(3):page.locator('#task-dialog').get_by_role('button',name='Следующая страница',exact=True).click();page.wait_for_timeout(150)
  assert page.locator('.ledger-row').count()==1;page.locator('#task-dialog').get_by_role('button',name='Назад',exact=True).click();page.wait_for_timeout(150);assert page.locator('.ledger-row').count()==8
  assert not errors,errors;assert page.evaluate('document.querySelector(\'#task-dialog\').scrollWidth<=document.querySelector(\'#task-dialog\').clientWidth');page.screenshot(path=str(root/'ledger-mobile.png'));browser.close()
 print('PASS Edge390 literal titles, max8 rows, source next/back and ledger pages without JS errors',flush=True)
finally:
 if process and process.poll() is None:
  try:read('/api/shutdown',{})
  except Exception:pass
  try:process.wait(timeout=8)
  except subprocess.TimeoutExpired:process.kill();process.wait()
 fixture.shutdown();log.close()
