from pathlib import Path
import json,os,subprocess,time,urllib.request,uuid,socket,sys
sys.stdout.reconfigure(encoding='utf-8')
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('foundation-014-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True)
foundation='hf.co/DevQuasar/huihui-ai.Foundation-Sec-8B-abliterated-GGUF:Q4_K_M'
settings={'workspace':str(root/'data/workspace'),'source':'en','target':'ru','theme':'dark','cpu_only':True,'compute':{'backend':'cpu','cpu_threads':min(10,os.cpu_count() or 1),'fallback_to_cpu':True},'agent_model':foundation,'chat_model':foundation,'agent_max_actions':4,'agent_timeout_seconds':300}
(root/'data/settings.json').write_text(json.dumps(settings))
(root/'data/workspace/fixture.txt').write_text('FOUNDATION_NATIVE_READ_VERIFIED_014',encoding='utf-8')
with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
base='http://127.0.0.1:'+str(port);http=urllib.request.build_opener(urllib.request.ProxyHandler({}));log=(root/'test.log').open('w')
p=subprocess.Popen([str(project/'target/release/local-gemma-studio.exe'),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':'http://127.0.0.1:11435'},stdout=log,stderr=log)
token=''
def call(path,body=None,timeout=420):
 req=urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json','X-Studio-Token':token})
 return http.open(req,timeout=timeout)
try:
 for _ in range(80):
  try:
   app=json.load(call('/api/bootstrap',timeout=2));token=app['token'];break
  except Exception:time.sleep(.1)
 chat=json.load(call('/api/chats',{}))
 payload={'conversation':chat['id'],'content':'Read fixture.txt and return its exact text. Use the read_file tool.','attachments':[],'mode':'agent','source':'en','target':'ru','web':'off','thinking':'off'}
 events=[]
 with call('/api/chat',payload) as r:
  for line in r:
   if not line.startswith(b'data:'):continue
   e=json.loads(line[5:]);events.append(e)
   if e['type'] in ['tool_start','tool_result','notice','error']:print(e['type'],json.dumps(e,ensure_ascii=False)[:700],flush=True)
 result=json.load(call('/api/chats/'+chat['id']))['messages'][-1]
 (root/'result.json').write_text(json.dumps(result,ensure_ascii=False,indent=2),encoding='utf-8')
 print('NATIVE_RESULT',result['content'],flush=True)
 assert any(t['name']=='read_file' and t['status']=='succeeded' for t in result['tools']),result
 assert 'FOUNDATION_NATIVE_READ_VERIFIED_014' in result['content'],result
 print('PASS native Foundation-Sec read_file and grounded answer through Rust JSON adapter',flush=True)
finally:
 try:call('/api/shutdown',{},timeout=5).close()
 except Exception:pass
 try:p.wait(timeout=10)
 except subprocess.TimeoutExpired:p.terminate();p.wait(timeout=5)
 log.close()
