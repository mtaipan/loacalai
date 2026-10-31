import json,os,subprocess,sys,threading
from pathlib import Path
project=Path(__file__).resolve().parents[2]
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
sys.stdout.reconfigure(encoding='utf-8')
submitted=[]
class Site(BaseHTTPRequestHandler):
 def log_message(self,*a):pass
 def do_GET(self):
  self.send_response(200);self.send_header('Content-Type','text/html; charset=utf-8');self.send_header('Set-Cookie','sid=DO_NOT_EXPOSE; HttpOnly; Secure; SameSite=Lax');self.send_header('X-Content-Type-Options','nosniff');self.end_headers()
  self.wfile.write(b'''<html><body><form onsubmit="event.preventDefault();fetch('/submit',{method:'POST',body:JSON.stringify({login:document.querySelector('#login').value,password:document.querySelector('#password').value})}).then(()=>document.querySelector('#result').textContent='TEST_SIGNED_IN')">
   <label>Username<input id="login" name="username"></label><label>Password<input id="password" type="password"></label><button>Sign in</button></form>
   <button onclick="document.querySelector('#login').outerHTML=document.querySelector('#login').outerHTML">Replace field</button><p id="result">EMPTY</p></body></html>''')
 def do_POST(self):
  submitted.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))));self.send_response(200);self.end_headers();self.wfile.write(b'{}')
server=ThreadingHTTPServer(('127.0.0.1',3331),Site);threading.Thread(target=server.serve_forever,daemon=True).start()
p=subprocess.Popen([sys.executable,str(project/'runtime/web_worker.py')],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8',env={**os.environ,'PYTHONUTF8':'1'})
texts=['For the fixture, enter alice and password demo-secret, then sign in.']
audit=[]
def request(name,args,user_texts=texts,error=None):
 p.stdin.write(json.dumps({'name':name,'args':args,'user_urls':['http://127.0.0.1:3331/'],'user_texts':user_texts,'audit_urls':audit})+'\n');p.stdin.flush();r=json.loads(p.stdout.readline())
 if error:assert r.get('error_code')==error,r
 else:assert 'error' not in r,r
 return r
try:
 r=request('fetch_url',{'url':'http://127.0.0.1:3331/'})
 assert r['headers']['X-Content-Type-Options']=='nosniff' and r['cookies'][0]['httponly'] and 'DO_NOT_EXPOSE' not in json.dumps(r)
 r=request('browser_open',{'url':'http://127.0.0.1:3331/'})
 refs={e['name']:e['ref'] for e in r['elements']}
 request('browser_prepare',{'action':'fill','ref':refs['Username'],'text':'your_username'},[],error='needs_input')
 request('browser_prepare',{'action':'fill','ref':refs['Password'],'text':'invented-secret'},[],error='needs_input')
 assert not submitted
 audit[:]=['http://127.0.0.1:3331/']
 r=request('browser_prepare',{'action':'fill','ref':refs['Username'],'text':'audit-test'},[])
 assert r['element']['security_test'] is True
 request('browser_prepare',{'action':'fill','ref':refs['Password'],'text':'your_password'},[],error='needs_input')
 audit[:]=['http://127.0.0.1:3332/']
 request('browser_prepare',{'action':'fill','ref':refs['Username'],'text':'audit-test'},[],error='needs_input')
 audit.clear()
 request('browser_action',{'action':'fill','ref':refs['Username'],'text':'alice'})
 # Both refs deliberately come from BEFORE the first fill.
 r=request('browser_action',{'action':'fill','ref':refs['Password'],'text':'demo-secret'})
 assert next(e['ref'] for e in r['elements'] if e['name']=='Username')==refs['Username']
 assert next(e['ref'] for e in r['elements'] if e['name']=='Password')==refs['Password']
 request('browser_read',{})
 r=request('browser_action',{'action':'click','ref':refs['Sign in']})
 assert submitted==[{'login':'alice','password':'demo-secret'}] and 'TEST_SIGNED_IN' in r['text'],r
 request('browser_action',{'action':'fill','ref':refs['Password'],'text':''},[])
 r=request('browser_action',{'action':'click','ref':refs['Replace field']})
 fresh=next(e['ref'] for e in r['elements'] if e['name']=='Username');assert fresh!=refs['Username']
 request('browser_prepare',{'action':'fill','ref':refs['Username'],'text':'alice'},error='stale_ref')
 request('browser_action',{'action':'fill','ref':fresh,'text':'alice'})
 request('browser_open',{'url':'http://127.0.0.1:3331/'})
 request('browser_prepare',{'action':'click','ref':refs['Sign in']},error='stale_ref')
 assert len(submitted)==1
 print('PASS audit scope, cross-origin denial, cookie/header evidence without secret values; original-ref login/password/click batch; stable refs across snapshots; replacement/navigation never rebound to another node; missing/placeholder credentials blocked before action')
finally:
 p.stdin.write('{"name":"close"}\n');p.stdin.flush();p.wait(timeout=10);server.shutdown();assert p.returncode==0,p.stderr.read()
