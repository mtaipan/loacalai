"""Real DOCX worker, fake inference, failed unit and application restart; private data."""
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import io,json,os,shutil,socket,sqlite3,subprocess,threading,time,urllib.request,urllib.error,uuid,zipfile
project=Path(__file__).resolve().parents[2];root=project/'build-tools'/('document-resume-'+uuid.uuid4().hex[:8]);(root/'data/workspace').mkdir(parents=True);(root/'runtime').mkdir()
for name in ['document_worker.py','document_tools.py']:shutil.copy2(project/'runtime'/name,root/'runtime'/name)
subprocess.run(['powershell.exe','-NoProfile','-Command','New-Item -ItemType Junction -Path $env:TEST_DOC_LINK -Target $env:TEST_DOC_TARGET | Out-Null'],env={**os.environ,'TEST_DOC_LINK':str(root/'runtime/python'),'TEST_DOC_TARGET':str(project/'runtime/python')},check=True)
model='hf.co/IndexTeam/Index-Translate-2B-GGUF:Q6_K';calls=[];fail_second=True;bad_format=False;digest='fixture-model-digest-1'
(root/'data/settings.json').write_text(json.dumps({'workspace':str(root/'data/workspace'),'translation_model':model,'cpu_only':True,'source':'en','target':'ru','theme':'dark'}))
class Engine(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def reply(self,value,status=200):
        body=json.dumps(value).encode()+b'\n';self.send_response(status);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
    def do_GET(self):self.reply({'models':[{'name':model,'digest':digest}]} if self.path=='/api/tags' else {'models':[]})
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or '{}')
        if self.path=='/api/show':return self.reply({'model_info':{'fixture.context_length':32768},'thinking':{'values':[False]}})
        text=json.dumps(body);unit='first' if 'UNIT_FIRST' in text else 'second' if 'UNIT_SECOND' in text else None
        if unit:
            calls.append(unit)
            if unit=='second' and fail_second:return self.reply({'error':'OWN_SECOND_UNIT_FAILED'},503)
            answer='Перевод первый' if unit=='first' else 'Перевод второй'
            content=body.get('messages',[{}])[-1].get('content','')
            if unit=='second' and '<p>' in content and not bad_format:answer=content[content.rfind('<p>'):].replace('UNIT_SECOND','Перевод второй').replace('TAIL','хвост')
            return self.reply({'done':True,'done_reason':'stop','message':{'role':'assistant','content':answer}})
        self.reply({'done':True})
fixture=ThreadingHTTPServer(('127.0.0.1',0),Engine);threading.Thread(target=fixture.serve_forever,daemon=True).start()
with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}';http=urllib.request.build_opener(urllib.request.ProxyHandler({}));token='';log=(root/'test.log').open('w');process=None
def call(path,body=None):return http.open(urllib.request.Request(base+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json','X-Studio-Token':token}),timeout=60)
def read(path,body=None):
    with call(path,body) as response:return json.load(response)
def start():
    global process,token
    process=subprocess.Popen([os.environ.get('LOCAL_GEMMA_TEST_EXE',str(project/'target/release/local-gemma-studio.exe')),'--server'],cwd=root,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_PORT':str(port),'LOCAL_GEMMA_OLLAMA':f'http://127.0.0.1:{fixture.server_port}'},stdout=log,stderr=log)
    for _ in range(100):
        if process.poll() is not None:raise AssertionError((root/'test.log').read_text(errors='replace'))
        try:token=read('/api/bootstrap')['token'];return
        except Exception:time.sleep(.05)
    raise AssertionError('Own server did not start')
def stop():
    try:read('/api/shutdown',{})
    except Exception:pass
    try:process.wait(timeout=8)
    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
def translate(item,source='en',target='ru'):
    events=[]
    with call('/api/documents/translate',{'attachment':item['id'],'source':source,'target':target}) as response:
        for line in response:
            if line.startswith(b'data:'):events.append(json.loads(line[5:]))
    return events
def upload_document(name,data):
    boundary='own-'+uuid.uuid4().hex
    payload=(f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{name}"\r\nContent-Type: application/octet-stream\r\n\r\n').encode()+data+f'\r\n--{boundary}--\r\n'.encode()
    with http.open(urllib.request.Request(base+'/api/upload',data=payload,headers={'Content-Type':'multipart/form-data; boundary='+boundary,'X-Studio-Token':token})) as response:return json.load(response)
try:
    start();buffer=io.BytesIO()
    with zipfile.ZipFile(buffer,'w') as archive:
        archive.writestr('[Content_Types].xml','<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>')
        archive.writestr('word/document.xml','<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>UNIT_FIRST</w:t></w:r></w:p><w:p><w:r><w:t>UNIT_SECOND</w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t> TAIL</w:t></w:r></w:p></w:body></w:document>')
    boundary='own-'+uuid.uuid4().hex;body=(f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="fixture.docx"\r\nContent-Type: application/vnd.openxmlformats-officedocument.wordprocessingml.document\r\n\r\n').encode()+buffer.getvalue()+f'\r\n--{boundary}--\r\n'.encode()
    with http.open(urllib.request.Request(base+'/api/upload',data=body,headers={'Content-Type':'multipart/form-data; boundary='+boundary,'X-Studio-Token':token})) as response:item=json.load(response)
    for bad in [{'text_offset':True},{'max_pages':0},{'ocr':'false'},{'text_offset':1}]:
        try:read('/api/documents/read',{'attachment':item['id'],**bad});raise AssertionError('Malformed options accepted')
        except urllib.error.HTTPError as error:assert error.code==400
    assert not calls
    first=translate(item);assert any(e['type']=='error' and 'OWN_SECOND_UNIT_FAILED' in e['message'] for e in first),first
    assert not any(e['type']=='document_result' for e in first)
    checkpoint=next(e['checkpoint']['id'] for e in first if e['type']=='document_checkpoint')
    with sqlite3.connect(root/'data/tasks.sqlite3') as db:assert db.execute('SELECT COUNT(*) FROM document_parts WHERE job=?',(checkpoint,)).fetchone()[0]==1
    stop();fail_second=False;start();before=len(calls);second=translate(item)
    result=next(e for e in second if e['type']=='document_result');assert calls[before:]==['second'],calls
    assert result['report']['checkpoint']['id']==checkpoint and result['report']['checkpoint']['reused_units']==1 and result['report']['checkpoint']['model_identity_verified']
    with call('/api/files/'+result['attachment']['id']) as response:output=response.read()
    with zipfile.ZipFile(io.BytesIO(output)) as archive:xml=archive.read('word/document.xml').decode();assert 'Перевод первый' in xml and 'Перевод второй' in xml
    before=len(calls);third=translate(item);assert len(calls)==before and next(e for e in third if e['type']=='document_result')['report']['checkpoint']['reused_units']==2
    digest='fixture-model-digest-2';before=len(calls);changed=translate(item);new=next(e for e in changed if e['type']=='document_result');assert calls[before:]==['first','second'] and new['report']['checkpoint']['id']!=checkpoint
    digest='fixture-model-digest-bad-format';bad_format=True;before=len(calls);invalid=translate(item)
    assert calls[before:]==['first','second','second'] and any(e['type']=='error' for e in invalid) and not any(e['type']=='document_result' for e in invalid)
    invalid_id=next(e['checkpoint']['id'] for e in invalid if e['type']=='document_checkpoint')
    with sqlite3.connect(root/'data/tasks.sqlite3') as db:assert db.execute('SELECT COUNT(*) FROM document_parts WHERE job=?',(invalid_id,)).fetchone()[0]==1
    bad_format=False;before=len(calls);repaired=next(e for e in translate(item) if e['type']=='document_result');assert calls[before:]==['second'] and repaired['report']['checkpoint']['reused_units']==1
    print('PASS invalid formatting and its failed repair are never committed or exported; retry preserves the earlier validated paragraph')
    digest=None;unknown=next(e for e in translate(item) if e['type']=='document_result');assert unknown['report']['checkpoint']['model_identity_verified'] is False
    before=len(calls);assert next(e for e in translate(item) if e['type']=='document_result')['report']['checkpoint']['reused_units']==2 and len(calls)==before
    stop();start();before=len(calls);restarted=next(e for e in translate(item) if e['type']=='document_result');assert calls[before:]==['first','second'] and restarted['report']['checkpoint']['id']!=unknown['report']['checkpoint']['id']
    with sqlite3.connect(root/'data/tasks.sqlite3') as db:assert db.execute('SELECT COUNT(*) FROM document_parts WHERE job=?',(unknown['report']['checkpoint']['id'],)).fetchone()[0]==2
    print('PASS missing model digest permits same-process reuse, preserves parts across restart and prevents unverified cross-process cache reuse')
    print('PASS validated first DOCX paragraph survives second-unit failure and server restart; retry infers only missing unit; full export verified; completed retry uses no inference; changed model digest starts a new checkpoint')
    # Exceed every former aggregate DOCX quota, plus paragraph size/style quotas.
    runs=''.join('<w:r><w:rPr><w:b w:val="'+str(i%2)+'"/></w:rPr><w:t>'+('large '+str(i)+' '+('a'*350))+'</w:t></w:r>' for i in range(70))
    paragraphs='<w:p>'+runs+'</w:p>'+''.join('<w:p>'+''.join('<w:r><w:t>'+f'LARGE_{i:04d} '+('b'*170)+'</w:t></w:r>' for _ in range(3))+'</w:p>' for i in range(1200))
    large_buffer=io.BytesIO()
    with zipfile.ZipFile(large_buffer,'w') as archive:
        archive.writestr('[Content_Types].xml','<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>')
        archive.writestr('word/document.xml','<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>'+paragraphs+'</w:body></w:document>')
    large_bytes=large_buffer.getvalue();large_path=root/'large.docx';large_path.write_bytes(large_bytes);large=upload_document('large.docx',large_bytes)
    offset=0;chunks=[];version=None
    while True:
        packet=read('/api/documents/read',{'attachment':large['id'],'text_offset':offset,'source_sha256':version})
        assert len(packet['text'])<=20000 and packet['start_offset']==offset
        version=packet['source_sha256'];chunks.append(packet['text'])
        if not packet['has_more']:break
        assert packet['next_offset']>offset;offset=packet['next_offset']
    text=''.join(chunks);assert len(text)==packet['total_characters'] and len(text)>600000 and text.count('LARGE_')==3600
    before=len(calls);large_events=translate(large,'en','en');large_result=next(e for e in large_events if e['type']=='document_result');assert len(calls)==before and large_result['report']['processed_units']==1201 and large_result['report']['plan_storage']=='jsonl-v1'
    with call('/api/files/'+large_result['attachment']['id']) as response:assert response.read()==large_bytes
    print('PASS DOCX1201 paragraphs, >3600 text nodes, >600k characters, >4k/70 styles in one paragraph: paged full reading and disk-planned unchanged export preserve original bytes; no inference quality claim')
    from playwright.sync_api import sync_playwright
    with sync_playwright() as pw:
        browser=pw.chromium.launch(channel='msedge',headless=True);page=browser.new_page(viewport={'width':390,'height':800});errors=[];page.on('pageerror',lambda e:errors.append(str(e)));page.goto(base);page.wait_for_selector('#file-input',state='attached');page.locator('#file-input').set_input_files(str(large_path));page.wait_for_selector('#pending-attachments [data-preview]');page.locator('#pending-attachments [data-preview]').click();page.wait_for_function("()=>document.querySelector('#document-information').textContent.includes('1–20000')")
        page.locator('#document-next').click();page.wait_for_function("()=>document.querySelector('#document-information').textContent.includes('20001–40000')")
        assert page.evaluate("()=>Array.from(document.querySelector('#document-text').textContent).length===20000&&document.activeElement.id==='document-next'") and not errors,errors
        page.locator('[data-action="close-document"]').click();browser.close()
    own_source=Path(large['path']);assert own_source.resolve().is_relative_to(root.resolve())
    with own_source.open('ab') as changed:changed.write(b'OWN_REVISION_CHANGE')
    try:read('/api/documents/read',{'attachment':large['id'],'text_offset':20000,'source_sha256':version});raise AssertionError('Stale DOCX source accepted')
    except urllib.error.HTTPError as error:assert error.code==409
    # Native PDFium: one real dense page exceeds the old 40k total extraction quota.
    content=b'BT /F1 1 Tf 2 TL 10 2300 Td\n'+b''.join((f'(PDF_DENSE_LINE_{i:04d} '+('c'*65)+') Tj T*\n').encode() for i in range(1000))+b'ET'
    objects=[b'<< /Type /Catalog /Pages 2 0 R >>',b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1200 2400] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>',b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',b'<< /Length '+str(len(content)).encode()+b' >>\nstream\n'+content+b'\nendstream']
    pdf=bytearray(b'%PDF-1.4\n');positions=[0]
    for i,obj in enumerate(objects,1):positions.append(len(pdf));pdf+=f'{i} 0 obj\n'.encode()+obj+b'\nendobj\n'
    xref=len(pdf);pdf+=b'xref\n0 6\n0000000000 65535 f \n'+b''.join(f'{p:010d} 00000 n \n'.encode() for p in positions[1:])+f'trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF'.encode()
    dense=upload_document('dense.pdf',bytes(pdf));offset=0;parts=[];pdf_version=None
    while True:
        packet=read('/api/documents/read',{'attachment':dense['id'],'start_page':1,'max_pages':1,'text_offset':offset,'source_sha256':pdf_version});pdf_version=packet['source_sha256'];parts.append(packet['text']);assert len(packet['text'])<=20000
        if not packet['has_more_text']:break
        assert packet['next_offset']>offset;offset=packet['next_offset']
    assert len(''.join(parts))>80000 and ''.join(parts).count('PDF_DENSE_LINE_')==1000
    dense_events=translate(dense,'en','en');dense_result=next(e for e in dense_events if e['type']=='document_result')
    with call('/api/files/'+dense_result['attachment']['id']) as response:assert response.read().decode().count('PDF_DENSE_LINE_')==1000
    dense_path=root/'dense.pdf';dense_path.write_bytes(bytes(pdf))
    with sync_playwright() as pw:
        browser=pw.chromium.launch(channel='msedge',headless=True);page=browser.new_page(viewport={'width':390,'height':800});page.goto(base);page.wait_for_selector('#file-input',state='attached');page.locator('#file-input').set_input_files(str(dense_path));page.wait_for_selector('#pending-attachments [data-preview]');page.locator('#pending-attachments [data-preview]').click();page.wait_for_function("()=>document.querySelector('#document-information').textContent.includes('1–20000')")
        for start in range(20000,len(''.join(parts)),20000):
            page.locator('#document-next').click();page.wait_for_function('(start)=>document.querySelector("#document-information").textContent.includes("символы "+(start+1)+"–")',arg=start)
        assert page.locator('#document-next').is_disabled() and page.evaluate("()=>document.activeElement.dataset.action==='read-document'")
        page.locator('[data-action="close-document"]').click();browser.close()
    print('PASS Edge390 DOCX next-range controls, stale-source409 and native dense PDF >80k text: all paged text and selected-page export retained')

finally:
    if process is not None and process.poll() is None:stop()
    fixture.shutdown();log.close()
